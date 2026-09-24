//! Bridge module: shared state and events between the HotLoop and external callers.
//!
//! Architecture:
//! - `SharedState` composes four domain-specific containers:
//!   - `MarketDataState` — lock-free quotes (SeqLock), TBT, real-time bars, news ticks.
//!   - `OrderState` — fills, order updates, cancel rejects, what-if, order cache.
//!   - `ReferenceState` — historical data, contracts, scanners, news archives, market rules.
//!   - `PortfolioState` — account snapshot, position info, atomic positions.
//! - `Event` enum carries all events through a channel for the `EClient` API.
//! - The HotLoop pushes to SharedState sub-containers directly.
//! - External callers read snapshots and poll events without blocking the hot loop.

mod event;
pub use event::*;
mod seq_quote;
pub use seq_quote::*;
mod market_data;
pub use market_data::*;
mod orders;
pub use orders::*;
mod reference;
pub use reference::*;
mod portfolio;
pub use portfolio::*;
mod slot_table;
mod record;
pub use record::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use std::sync::{Condvar, Mutex};


/// The token the venue grants a session that may name Nasdaq by its older
/// spelling. Stated on the granted-feature list at logon.
const ISLAND_FOR_NASDAQ_GRANT: &str = "ISLAND2NASDAQ";
use crate::types::*;

/// `Quote` as its fields, in order. Both directions destructure or name every
/// field, so a field added to `Quote` fails to compile here rather than being
/// silently dropped from everything the seqlock publishes.
const QUOTE_WORDS: usize = 16;

fn quote_to_words(q: &Quote) -> [i64; QUOTE_WORDS] {
    let Quote {
        bid, ask, last, bid_size, ask_size, last_size, volume,
        open, high, low, close, timestamp_ns,
        bid_exch_mask, ask_exch_mask, last_exch_mask, halted,
    } = *q;
    [
        bid, ask, last, bid_size, ask_size, last_size, volume,
        open, high, low, close, timestamp_ns as i64,
        bid_exch_mask, ask_exch_mask, last_exch_mask, halted,
    ]
}

fn quote_from_words(w: [i64; QUOTE_WORDS]) -> Quote {
    Quote {
        bid: w[0], ask: w[1], last: w[2],
        bid_size: w[3], ask_size: w[4], last_size: w[5], volume: w[6],
        open: w[7], high: w[8], low: w[9], close: w[10],
        timestamp_ns: w[11] as u64,
        bid_exch_mask: w[12], ask_exch_mask: w[13], last_exch_mask: w[14],
        halted: w[15],
    }
}

#[cfg(test)]
mod seq_quote_tests {
    use super::*;

    /// A reader and a writer working the same slot at once. The payload is
    /// accessed as atomics, so the race the version counter guards against is
    /// a defined operation rather than undefined behaviour — which is what
    /// lets this run under Miri at all.
    #[test]
    fn a_concurrent_reader_never_sees_half_a_quote() {
        let slot = std::sync::Arc::new(SeqQuote::new());
        let writer = {
            let slot = slot.clone();
            std::thread::spawn(move || {
                for i in 1..500i64 {
                    // Every field moves together, so any snapshot mixing two
                    // generations is visible as a field that disagrees.
                    slot.write(&Quote {
                        bid: i, ask: i, last: i,
                        bid_size: i, ask_size: i, last_size: i, volume: i,
                        open: i, high: i, low: i, close: i,
                        timestamp_ns: i as u64,
                        bid_exch_mask: i, ask_exch_mask: i, last_exch_mask: i,
                        halted: i,
                    }, i as u64);
                }
            })
        };
        for _ in 0..500 {
            let q = slot.read();
            assert_eq!(q.bid, q.last_exch_mask, "a snapshot must come from one write");
            assert_eq!(q.timestamp_ns as i64, q.bid, "including the field of another type");
        }
        writer.join().unwrap();
    }
}

#[cfg(test)]
mod order_replay_tests {
    use super::*;
    use crate::types::{OrderStatus, OrderUpdate};

    fn update(order_id: u64, status: OrderStatus, filled: f64, remaining: f64) -> OrderUpdate {
        OrderUpdate {
            order_id, instrument: 0, status,
            filled_qty: filled, remaining_qty: remaining, avg_price: 0,
            perm_id: 0, parent_id: 0, timestamp_ns: 0,
        }
    }

    /// The connect-time replay names what the server thinks is working, and it
    /// is published as it arrives. An order this client has already been paid
    /// on must not come back from it as live.
    #[test]
    fn the_replay_does_not_resurrect_an_order_that_finished() {
        let s = OrderState::new();
        s.push_completed_order(crate::types::CompletedOrder {
            order_id: 7, instrument: 0, status: OrderStatus::Filled,
            filled_qty: 1, timestamp_ns: 0,
        });

        s.push_order_info(7, RichOrderInfo {
            contract: Default::default(),
            order: Default::default(),
            order_state: crate::types::model::OrderState {
                status: "Submitted".to_string(), ..Default::default()
            },
            last_exec: Default::default(),
        });

        assert!(
            s.drain_open_orders().is_empty(),
            "a filled order is not reported as working again by the replay",
        );
    }

    /// The venue echoes a working status behind a fill. The order is retired
    /// by then, so the echo finds no record to be refused by and reported a
    /// filled order as working with nothing filled.
    #[test]
    fn a_finished_order_is_not_reopened_by_a_frame_behind_it() {
        let s = OrderState::new();
        s.push_order_update(update(7, OrderStatus::Filled, 1.0, 0.0));
        s.push_completed_order(crate::types::CompletedOrder {
            order_id: 7, instrument: 0, status: OrderStatus::Filled,
            filled_qty: 1, timestamp_ns: 0,
        });

        // Queued before the fill was known, which is the real ordering.
        s.push_order_update(update(7, OrderStatus::PreSubmitted, 0.0, 1.0));

        let seen = s.drain_order_updates();
        assert_eq!(seen.len(), 1, "only the fill reaches the caller: {seen:?}");
        assert_eq!(seen[0].status, OrderStatus::Filled);

        // An order that has not finished is untouched by this.
        s.push_order_update(update(8, OrderStatus::PreSubmitted, 0.0, 1.0));
        assert_eq!(s.drain_order_updates().len(), 1, "a live order still reports");
    }
}

// ── Domain-specific state containers ──

/// How long a completion is remembered.
///
/// Held by age rather than by count. What this has to outlive is the window in
/// which a stale frame for the order can still arrive — a reconnect replays
/// recent activity within seconds — and that window is a duration, not a number
/// of orders. Counting instead meant a busy session's unrelated completions
/// pushed a still-relevant one out, and the replay it was there to refuse got
/// back in.
const COMPLETED_RETENTION: Duration = Duration::from_secs(300);

/// Hard cap on how many completions are remembered at once, regardless of
/// age. Expired entries are pruned first; a session that completes orders
/// faster than they expire would otherwise leave every young entry in
/// place, so once pruning alone cannot bring the map back under this bound,
/// the oldest survivors are evicted until it does. Generous enough that
/// reaching it at all means completions are arriving far faster than any
/// legitimate replay could still be racing the ones being dropped.
const COMPLETED_MAX: usize = 65_536;

/// Shared state between hot loop and external caller.
/// Composed of domain-specific containers for clear ownership boundaries.
pub struct SharedState {
    /// What the session runs under, settled when it opened.
    ///
    /// Held here so the engine reads a value rather than the process it runs
    /// in: two sessions in one process have their own, and neither can change
    /// the other's mid-flight.
    pub settings: std::sync::Mutex<std::sync::Arc<crate::settings::SessionSettings>>,
    /// Prices, books and streams.
    pub market: MarketDataState,
    /// Fills, order changes and previews.
    pub orders: OrderState,
    /// Everything that is not a price: contracts, history, news, scans.
    pub reference: ReferenceState,
    /// What the account holds and what it is worth.
    pub portfolio: std::sync::Arc<PortfolioState>,
    session_account: Mutex<String>,
    portfolios: Mutex<std::collections::HashMap<String, std::sync::Arc<PortfolioState>>>,
    account_requests: Mutex<std::collections::HashMap<String, String>>,
    account_selections_said: Mutex<std::collections::HashSet<String>>,
    /// Last measured auth-connection round-trip time in nanoseconds
    /// (0 = never measured). Sampled from the test-request/echo cycle —
    /// see `HotLoop` liveness and `ControlCommand::Ping`.
    /// How many events this session discarded because nobody read far enough.
    ///

    ccp_rtt_ns: AtomicU64,
    /// What the session has sent and received on the venue's connections.
    traffic: std::sync::Arc<crate::protocol::connection::TrafficCounts>,
    /// The counter every record of this session is stamped from.
    stamps: Stamps,
    /// The session's own records: the connection going and coming back, the
    /// venue's data connections doing the same, the slots taken and given
    /// back, and the last record of all.
    session_records: Queue<Record>,
    /// What a call pushes: its refusals, the answers composed where they are
    /// delivered, and the answers given at the call.
    calls: Queue<Record>,
    /// Whether the connection is up, as the engine last said. A loss and a
    /// recovery are each said once, as this flips.
    link_up: AtomicBool,
    /// Whether the last loss was deliberate. Recorded at the moment of loss,
    /// not derived later.
    connection_lost_by_design: AtomicBool,
    /// Whether the session's last record has been pushed.
    closed: AtomicBool,
    /// No new caller work is accepted once the session starts closing.
    admission_closed: AtomicBool,
    /// Serialize a send with closing admission.
    admission: Mutex<()>,
    pub(crate) logout_sent: AtomicBool,
    pub(crate) engine_panicked: AtomicBool,
    /// Every command admitted to the engine, raised before its send.
    admitted: AtomicU64,
    /// Every command the engine's loop has finished with, published once per
    /// lap: those it has taken so far, less those it still holds.
    finished: AtomicU64,
    /// Notifier for waking consumers (e.g. Python event loop) when data arrives.
    notify_mutex: Mutex<bool>,
    notify_condvar: Condvar,
    /// What the owner of this session is woken by, besides the condvar above.
    wake_hook: Mutex<Option<WakeHook>>,
    /// Whether the hook may be called: cleared as it is, and set again as a
    /// read begins, so it is called at most once per read.
    wake_armed: AtomicBool,
    /// The stamp counter as the last notification found it, so the next one
    /// can tell whether a record was pushed since.
    wake_cut: AtomicU64,
}

/// A hook the engine calls on its own thread when something is there to read.
pub type WakeHook = std::sync::Arc<dyn Fn() + Send + Sync>;

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedState {
    /// Name the account whose figures the opening download states.
    #[doc(hidden)]
    pub fn set_session_account(&self, account: &str) {
        *self.session_account.lock().unwrap() = account.to_string();
    }

    /// The account named, or the opening account where no name was given.
    pub(crate) fn account_name(&self, account: &str) -> String {
        if account.is_empty() { self.session_account.lock().unwrap().clone() } else { account.to_string() }
    }

    /// Figures and holdings kept separately for every account the venue names.
    #[doc(hidden)]
    pub fn portfolio_for(&self, account: &str) -> std::sync::Arc<PortfolioState> {
        if account.is_empty() || *self.session_account.lock().unwrap() == account {
            return self.portfolio.clone();
        }
        self.portfolios.lock().unwrap().entry(account.to_string())
            .or_insert_with(|| std::sync::Arc::new(PortfolioState::stamping(&self.stamps))).clone()
    }

    /// Bind an account response key before its request goes out.
    pub(crate) fn name_account_request(&self, key: &str, account: &str) {
        self.account_requests.lock().unwrap().insert(key.to_string(), self.account_name(account));
    }

    /// The account named by an account response's request key.
    pub(crate) fn portfolio_for_request(&self, key: &str) -> Option<std::sync::Arc<PortfolioState>> {
        if key.is_empty() { return Some(self.portfolio.clone()); }
        let account = self.account_requests.lock().unwrap().get(key).cloned()?;
        Some(self.portfolio_for(&account))
    }

    pub(crate) fn portfolio_for_message(&self, msg: &[u8]) -> Option<std::sync::Arc<PortfolioState>> {
        let parsed = crate::protocol::fix::fix_parse(msg);
        let key = parsed.get(&8292).filter(|key| !key.is_empty())
            .or_else(|| parsed.get(&6529)).map(String::as_str).unwrap_or("");
        if !key.is_empty() { return self.portfolio_for_request(key); }
        let mut field = "";
        let mut account = "";
        for part in std::str::from_utf8(msg).unwrap_or("").split('\x01') {
            if let Some(value) = part.strip_prefix("8001=") { field = value; }
            if let Some(value) = part.strip_prefix("8004=") {
                if matches!(field, "AccountCode" | "AddAccountCode") { account = value; }
                field = "";
            }
        }
        Some(self.portfolio_for(account))
    }

    pub(crate) fn note_unapplied_account_selection(&self, name: &str) {
        if self.account_selections_said.lock().unwrap().insert(name.to_string()) {
            log::warn!("account selection {name} is taken and not applied");
        }
    }

    /// The accounts whose figures this session has requested or received.
    pub(crate) fn account_portfolios(&self) -> Vec<(String, std::sync::Arc<PortfolioState>)> {
        let mut all = vec![(self.account_name(""), self.portfolio.clone())];
        all.extend(self.portfolios.lock().unwrap().iter().map(|(a, p)| (a.clone(), p.clone())));
        all
    }

    /// What this session runs under.
    pub fn settings(&self) -> std::sync::Arc<crate::settings::SessionSettings> {
        self.settings.lock().unwrap().clone()
    }

    /// Whether a US stock trading on Nasdaq is named by the older spelling.
    ///
    /// The setting asks for it and the venue grants it, and both are required.
    /// The grant is read off the granted-feature list at logon and held beside
    /// the setting. Read once per contract definition, so
    /// the grant is settled at logon rather than scanned for here.
    pub fn island_for_nasdaq(&self) -> bool {
        self.settings().island_for_nasdaq && self.reference.island_granted()
    }

    /// Stated once, as the session opens, before the engine's threads start.
    #[doc(hidden)]
    pub fn set_settings(&self, settings: std::sync::Arc<crate::settings::SessionSettings>) {
        *self.settings.lock().unwrap() = settings;
    }

    /// An empty one.
    pub fn new() -> Self {
        let stamps = Stamps::default();
        Self {
            settings: std::sync::Mutex::new(std::sync::Arc::new(Default::default())),
            market: MarketDataState::stamping(&stamps),
            orders: OrderState::stamping(&stamps),
            reference: ReferenceState::stamping(&stamps),
            portfolio: std::sync::Arc::new(PortfolioState::stamping(&stamps)),
            session_account: Mutex::new(String::new()),
            portfolios: Mutex::new(std::collections::HashMap::new()),
            account_requests: Mutex::new([("AR.1".to_string(), String::new()), ("PLR.1".to_string(), String::new())].into_iter().collect()),
            account_selections_said: Mutex::new(std::collections::HashSet::new()),
            ccp_rtt_ns: AtomicU64::new(0),
            traffic: Default::default(),
            session_records: Queue::new(&stamps),
            calls: Queue::new(&stamps),
            stamps,
            link_up: AtomicBool::new(true),
            connection_lost_by_design: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            admission_closed: AtomicBool::new(false),
            admission: Mutex::new(()),
            logout_sent: AtomicBool::new(false),
            engine_panicked: AtomicBool::new(false),
            admitted: AtomicU64::new(0),
            finished: AtomicU64::new(0),
            notify_mutex: Mutex::new(false),
            notify_condvar: Condvar::new(),
            wake_hook: Mutex::new(None),
            wake_armed: AtomicBool::new(true),
            wake_cut: AtomicU64::new(0),
        }
    }

    /// The connection went. Hot-loop side.
    ///
    /// Said once, as the connected flag flips: a halt after a loss already
    /// said, or a second transport going with the first, says nothing more.
    /// The record carries whether the loss was asked for, which is decided
    /// here rather than by a later reader. A shutdown records its own reason
    /// and records it after any venue-side drop, so deriving this afterwards
    /// reports a caller-requested stop for a session the venue ended, and
    /// every absence after it reads as tidying.
    #[doc(hidden)]
    #[inline]
    pub fn set_connection_lost(&self) {
        let by_design = self.reference.session_over()
            == Some(crate::reliability::retry::DisconnectReason::ByDesign.as_str());
        self.connection_lost_by_design.store(by_design, Ordering::Release);
        if self.link_up.swap(false, Ordering::AcqRel) {
            self.session_records.push(Record::ConnectionLost { by_design });
        }
        self.notify();
    }

    /// Whether the last recorded loss was one this process asked for.
    #[inline]
    pub fn connection_lost_by_design(&self) -> bool {
        self.connection_lost_by_design.load(Ordering::Acquire)
    }

    /// The connection came back after a loss. Hot-loop side. Said once, as
    /// the connected flag flips back.
    #[doc(hidden)]
    #[inline]
    pub fn set_connection_restored(&self) {
        if !self.link_up.swap(true, Ordering::AcqRel) {
            self.session_records.push(Record::ConnectionRestored);
        }
        self.notify();
    }

    /// Whether the connection is up, as the engine last said.
    pub fn link_up(&self) -> bool {
        self.link_up.load(Ordering::Acquire)
    }

    /// Take the notices that the connection went, and say whether there was
    /// one. For a reader of the engine's own, which reads no other record.
    #[doc(hidden)]
    pub fn take_connection_lost(&self) -> bool {
        !self.session_records.take_if(|r| matches!(r, Record::ConnectionLost { .. })).is_empty()
    }

    /// The same for the notices that it came back.
    #[doc(hidden)]
    pub fn take_connection_restored(&self) -> bool {
        !self.session_records.take_if(|r| matches!(r, Record::ConnectionRestored)).is_empty()
    }

    /// One of the connections the venue keeps data on went away or came back.
    /// Hot-loop side; a record, so it is heard in its place.
    #[doc(hidden)]
    pub fn push_venue_data_notice(&self, which: crate::bridge::VenueDataConnection, up: bool) {
        self.session_records.push(Record::VenueData((which, up)));
        self.notify();
    }

    /// Take the data-connection notices, in the order they came.
    pub fn drain_venue_data_notices(&self) -> Vec<(crate::bridge::VenueDataConnection, bool)> {
        self.session_records
            .take_if(|r| matches!(r, Record::VenueData(_)))
            .into_iter()
            .filter_map(|r| match r {
                Record::VenueData(notice) => Some(notice),
                _ => None,
            })
            .collect()
    }

    /// A slot is held from here under `generation`, or given back under it.
    /// Hot-loop side, where the occupancy is named: a record, so a reader
    /// applies it in its place, before anything pushed under the slot after it.
    #[doc(hidden)]
    pub fn push_slot_record(&self, record: Record) {
        debug_assert!(matches!(record, Record::SlotTaken { .. } | Record::SlotReleased { .. }));
        self.session_records.push(record);
    }

    /// Where the session's connections count what they read and write.
    /// Hot-loop side, which hands it to each connection it takes.
    #[doc(hidden)]
    pub fn traffic_counts(&self) -> &std::sync::Arc<crate::protocol::connection::TrafficCounts> {
        &self.traffic
    }

    /// What the session has sent and received on the venue's connections since
    /// it opened: bytes and messages, each way.
    pub fn traffic(&self) -> crate::protocol::connection::Traffic {
        self.traffic.read()
    }

    /// Record an auth-connection RTT sample. Hot-loop side.
    #[inline]
    pub fn set_ccp_rtt(&self, rtt: std::time::Duration) {
        self.ccp_rtt_ns.store(rtt.as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
    }

    /// Last measured auth-connection round-trip time, if any.
    /// A gauge, not a benchmark: the sample is the interval from a test
    /// request to the first inbound traffic that followed it, which on an
    /// active feed can undercount by racing data already in flight.
    #[inline]
    pub fn last_ccp_rtt(&self) -> Option<std::time::Duration> {
        match self.ccp_rtt_ns.load(Ordering::Relaxed) {
            0 => None,
            ns => Some(std::time::Duration::from_nanos(ns)),
        }
    }

    /// Hand the engine a command without waiting, counted until the engine
    /// has finished with it.
    ///
    /// The count is raised before the send, so every command the loop takes
    /// has been counted by then; a send the engine is no longer there for
    /// takes its count back. A TWS call returns once its message is written,
    /// and this is that write: the channel is unbounded, and nothing here
    /// waits for the loop to make room.
    pub fn admit(
        &self,
        tx: &std::sync::mpsc::Sender<ControlCommand>,
        cmd: ControlCommand,
    ) -> Result<(), crate::error_codes::Refusal> {
        let _admission = self.admission.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.closed_pushed() || (self.admission_closed() && !matches!(cmd, ControlCommand::Logout | ControlCommand::Shutdown)) {
            return Err(crate::error_codes::Refusal::not_connected("Engine stopped"));
        }
        self.admitted.fetch_add(1, Ordering::AcqRel);
        tx.send(cmd).map_err(|gone| {
            self.admitted.fetch_sub(1, Ordering::AcqRel);
            crate::error_codes::Refusal::not_connected(format!("Engine stopped: {gone}"))
        })
    }

    /// Close the session to new caller work before starting its finalizer.
    pub(crate) fn close_admission(&self) {
        let _admission = self.admission.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.admission_closed.store(true, Ordering::Release);
    }

    pub(crate) fn admission_closed(&self) -> bool {
        self.admission_closed.load(Ordering::Acquire)
    }

    /// How many commands have been admitted and not finished: waiting in the
    /// channel, or taken and held by the loop for naming, for the order
    /// buffer or for an exchange it must wait behind.
    ///
    /// `finished` is read first. Every command it counts was admitted before
    /// the loop took it, so the difference is never negative; and a command
    /// the loop has taken still counts until the lap that took it publishes,
    /// so the count never drops between the take and the hold.
    pub fn backlog(&self) -> usize {
        let finished = self.finished.load(Ordering::Acquire);
        let admitted = self.admitted.load(Ordering::Acquire);
        usize::try_from(admitted.saturating_sub(finished)).unwrap_or(usize::MAX)
    }

    /// Hot-loop side: how many commands the loop has finished with, once its
    /// lap has taken its commands and updated its holds.
    #[doc(hidden)]
    pub fn publish_finished(&self, finished: u64) {
        self.finished.store(finished, Ordering::Release);
    }

    /// Signal that new data is available. Called by hot loop after pushing data.
    ///
    /// Then calls the wake hook, if one is set, with this signal's lock
    /// released and no other lock of the engine's held: only when a record has
    /// been pushed or conflated state written since the last notification, and
    /// at most once until the next read begins.
    #[inline]
    pub fn notify(&self) {
        {
            let mut pending = self.notify_mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            *pending = true;
            self.notify_condvar.notify_one();
        }
        self.wake();
    }

    /// Call the wake hook, if anything has changed and a read has begun since
    /// it was last called.
    ///
    /// The engine notifies at the end of every lap, and a connected loop laps
    /// as fast as it can: a hook called on every notification would wake its
    /// owner at that rate, whether or not there was anything to read.
    fn wake(&self) {
        let cut = self.stamps.cut();
        let pushed = self.wake_cut.swap(cut, Ordering::SeqCst) != cut;
        let written = self.stamps.take_written();
        std::sync::atomic::fence(Ordering::SeqCst);
        if !(pushed || written) || !self.wake_armed.load(Ordering::SeqCst) {
            return;
        }
        // Taken out of the lock before it is called, so a hook that sets a
        // hook, or reads this session, waits on nothing this holds.
        let hook = self.wake_hook.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let Some(hook) = hook else { return };
        if !self.wake_armed.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook())) {
            // A caller can panic with a payload whose destructor also panics.
            // Dropping it would let that second panic escape this boundary.
            std::mem::forget(payload);
            log::error!("the hook set to be woken by this session panicked, and is removed");
            let mut held = self.wake_hook.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if held.as_ref().is_some_and(|set| std::sync::Arc::ptr_eq(set, &hook)) {
                *held = None;
            }
            drop(held);
            // Removing the last reference also drops anything the hook kept.
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(hook))) {
                std::mem::forget(payload);
            }
        }
    }

    /// Set the hook [`notify`](Self::notify) calls, replacing the one before
    /// it; `None` removes it. Replacing it does not begin another read.
    pub fn set_wake_hook(&self, hook: Option<WakeHook>) {
        let previous = {
            let mut held = self.wake_hook.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *held, hook)
        };
        drop(previous);
    }

    /// A read has begun: the hook may be called again for what changes after
    /// this.
    pub fn arm_wake(&self) {
        self.wake_armed.store(true, Ordering::SeqCst);
        std::sync::atomic::fence(Ordering::SeqCst);
    }

    /// Wait for data notification with a timeout. Returns true if notified, false if
    /// timed out.
    pub fn wait_for_data(&self, timeout: std::time::Duration) -> bool {
        let mut pending = self.notify_mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *pending {
            *pending = false;
            return true;
        }
        let (mut flag, result) = self
            .notify_condvar
            .wait_timeout(pending, timeout)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let had_data = *flag;
        // Taken back through the guard the wait returns. Letting go of it to
        // take it again left a gap an announcement could land in, and the
        // clear that followed wiped it: what had arrived was never announced
        // to anybody, and the last thing a producer sent before going quiet
        // was the one that went missing.
        if had_data {
            *flag = false;
        }
        had_data || !result.timed_out()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use crate::types::model as api;
    use super::*;

    /// The venue broadcasts notices unasked and only a subscriber drains
    /// them, so a session that never subscribes would otherwise hold every
    /// notice of the day for the life of the process. Past the bound the
    /// oldest are dropped, and a late subscriber is handed the most recent.
    #[test]
    fn broadcast_notices_do_not_pile_up_unread() {
        let shared = SharedState::new();
        for id in 0..(NEWS_BULLETIN_LIMIT as i32 + 10) {
            shared.market.push_news_bulletin(crate::types::NewsBulletin {
                msg_id: id, msg_type: 1, message: String::new(), exchange: String::new(),
            });
        }
        let held = shared.market.drain_news_bulletins();
        assert_eq!(held.len(), NEWS_BULLETIN_LIMIT, "the buffer grew past its bound");
        assert_eq!(held[0].msg_id, 10, "the oldest were kept and the newest dropped");
        assert_eq!(held[held.len() - 1].msg_id, NEWS_BULLETIN_LIMIT as i32 + 9);
    }

    /// The connection notices a read takes, as (lost, by design) and
    /// restored, in order.
    fn connection_records(shared: &SharedState) -> Vec<Option<bool>> {
        shared
            .take_records(shared.next_seq(), Take::Dispatch { bulletins: false })
            .into_iter()
            .filter_map(|(_, r)| match r {
                Record::ConnectionLost { by_design } => Some(Some(by_design)),
                Record::ConnectionRestored => Some(None),
                _ => None,
            })
            .collect()
    }

    /// Whether a loss was asked for is decided as it is recorded.
    ///
    /// Shutting down records its own reason. Derived from that reason after
    /// the fact, a session the venue took away reports as caller-requested,
    /// and every absence after it reads as ordinary tidying.
    #[test]
    fn a_loss_remembers_whether_it_was_asked_for() {
        use crate::reliability::retry::DisconnectReason;

        // The venue takes the session away: nothing has recorded a reason.
        let shared = SharedState::new();
        shared.set_connection_lost();
        // The tidying that follows records one, as a shutdown does.
        shared.reference.set_session_over(DisconnectReason::ByDesign.as_str());
        assert_eq!(connection_records(&shared), [Some(false)], "nobody asked for this one");

        // And a shutdown, which records its reason before the loss.
        let asked = SharedState::new();
        asked.reference.set_session_over(DisconnectReason::ByDesign.as_str());
        asked.set_connection_lost();
        assert_eq!(connection_records(&asked), [Some(true)]);
    }

    /// A loss is said once and its recovery once, as the connected flag flips:
    /// a second transport going with the first, or a halt after a loss already
    /// said, says nothing more, and a recovery with no loss before it is no
    /// recovery.
    #[test]
    fn a_loss_and_its_recovery_are_each_one_record() {
        let shared = SharedState::new();
        shared.set_connection_restored();
        assert!(connection_records(&shared).is_empty(), "a recovery from nothing says nothing");

        shared.set_connection_lost();
        shared.set_connection_lost();
        shared.set_connection_restored();
        shared.set_connection_restored();
        shared.set_connection_lost();
        assert_eq!(
            connection_records(&shared),
            [Some(false), None, Some(false)],
            "one 1100, one 1102, and the loss that followed",
        );
        assert!(!shared.link_up());
    }

    /// A quote is read with the occupancy it was written under, from the same
    /// write, so a reader cannot pair one contract's quote with the next
    /// contract's name for the slot.
    #[test]
    fn a_quote_is_read_with_the_occupancy_it_was_written_under() {
        let shared = SharedState::new();
        shared.market.set_generation(3, 7);
        shared.market.push_quote(3, &Quote { bid: 5, ..Default::default() });
        let (quote, generation) = shared.market.quote_with_generation(3);
        assert_eq!((quote.bid, generation), (5, 7));
        // Renamed, the quote standing is written again under the new name.
        shared.market.set_generation(3, 8);
        let (quote, generation) = shared.market.quote_with_generation(3);
        assert_eq!((quote.bid, generation), (5, 8));
    }

    #[test]
    fn seqquote_write_read_roundtrip() {
        let sq = SeqQuote::new();
        let q = Quote { bid: 150 * PRICE_SCALE, ask: 151 * PRICE_SCALE, ..Default::default() };
        sq.write(&q, 0);
        let read = sq.read();
        assert_eq!(read.bid, 150 * PRICE_SCALE);
        assert_eq!(read.ask, 151 * PRICE_SCALE);
    }

    #[test]
    fn seqquote_default_is_zero() {
        let sq = SeqQuote::new();
        let q = sq.read();
        assert_eq!(q.bid, 0);
        assert_eq!(q.ask, 0);
    }

    #[test]
    fn order_state_drain_open_orders_admits_inactive_excludes_rejected() {
        let ss = SharedState::new();
        ss.orders.push_order_info(90, RichOrderInfo {
            contract: api::Contract::default(),
            order: api::Order::default(),
            order_state: api::OrderState { status: "Inactive".into(), ..Default::default() },
            last_exec: api::Execution::default(),
        });
        ss.orders.push_order_info(91, RichOrderInfo {
            contract: api::Contract::default(),
            order: api::Order::default(),
            order_state: api::OrderState {
                status: "Inactive".into(),
                completed_status: "No valid bid/ask".into(),
                ..Default::default()
            },
            last_exec: api::Execution::default(),
        });

        let open = ss.orders.drain_open_orders();
        assert!(open.iter().any(|(id, _)| *id == 90),
            "genuinely-inactive order must be admitted to the open-order snapshot");
        assert!(!open.iter().any(|(id, _)| *id == 91),
            "rejected order (non-empty completed_status) must not resurrect");
    }

    #[test]
    fn shared_state_fills_drain() {
        let ss = SharedState::new();
        ss.orders.push_fill(Fill {
            instrument: 0, order_id: 1, side: Side::Buy,
            price: 100 * PRICE_SCALE, qty: 10, remaining: 0, timestamp_ns: 0,
            cum_qty: 10, avg_price: 100 * PRICE_SCALE,
        });
        ss.orders.push_fill(Fill {
            instrument: 0, order_id: 2, side: Side::Sell,
            price: 101 * PRICE_SCALE, qty: 5, remaining: 0, timestamp_ns: 0,
            cum_qty: 5, avg_price: 101 * PRICE_SCALE,
        });
        let fills = ss.orders.drain_fills();
        assert_eq!(fills.len(), 2);
        assert!(fills.iter().all(|(_, report)| report.is_none()), "none was pushed with one");
        // Second drain should be empty
        assert!(ss.orders.drain_fills().is_empty());
    }

    #[test]
    fn shared_state_order_updates_drain() {
        let ss = SharedState::new();
        ss.orders.push_order_update(OrderUpdate {
            order_id: 1, instrument: 0, status: OrderStatus::Submitted,
            filled_qty: 0.0, remaining_qty: 100.0, avg_price: 0, perm_id: 0, parent_id: 0, timestamp_ns: 0,
        });
        let updates = ss.orders.drain_order_updates();
        assert_eq!(updates.len(), 1);
        assert!(ss.orders.drain_order_updates().is_empty());
    }

    #[test]
    fn shared_state_position_roundtrip() {
        let ss = SharedState::new();
        assert_eq!(ss.portfolio.position(0), 0.0);
        ss.portfolio.set_position(0, 42.0);
        assert_eq!(ss.portfolio.position(0), 42.0);
        ss.portfolio.set_position(0, -10.0);
        assert_eq!(ss.portfolio.position(0), -10.0);
    }

    #[test]
    fn shared_state_account_roundtrip() {
        let ss = SharedState::new();
        let a = AccountState { net_liquidation: 100_000 * PRICE_SCALE, ..Default::default() };
        ss.portfolio.set_account(&a);
        let read = ss.portfolio.account();
        assert_eq!(read.net_liquidation, 100_000 * PRICE_SCALE);
    }

    #[test]
    fn reference_state_ccp_session_id_roundtrip() {
        let ss = SharedState::new();
        assert!(ss.reference.ccp_session_id().is_empty());
        ss.reference.set_ccp_session_id("abc.0001".to_string());
        assert_eq!(ss.reference.ccp_session_id(), "abc.0001");
    }

    #[test]
    fn reference_state_misc_urls_roundtrip() {
        let ss = SharedState::new();
        assert!(ss.reference.misc_urls().is_empty());
        assert!(ss.reference.misc_url("region_dam").is_none());
        let mut urls = HashMap::new();
        urls.insert("region_dam".to_string(), "api-east.example.com".to_string());
        urls.insert("margin".to_string(), "margin.example.com".to_string());
        ss.reference.set_misc_urls(urls);
        let map = ss.reference.misc_urls();
        assert_eq!(map.len(), 2);
        assert_eq!(ss.reference.misc_url("region_dam").as_deref(), Some("api-east.example.com"));
        assert_eq!(ss.reference.misc_url("missing"), None);
    }

    #[test]
    fn event_gateway_logon_carries_fields() {
        let mut urls = HashMap::new();
        urls.insert("region_dam".to_string(), "api.example.com".to_string());
        let event = Event::GatewayLogon {
            ccp_session_id: "sid.abcd".to_string(),
            misc_urls: urls,
        };
        match event {
            Event::GatewayLogon { ccp_session_id, misc_urls } => {
                assert_eq!(ccp_session_id, "sid.abcd");
                assert_eq!(misc_urls.get("region_dam").map(String::as_str), Some("api.example.com"));
            }
            _ => panic!("expected GatewayLogon"),
        }
    }

    #[test]
    fn seqquote_concurrent_read_write() {
        use std::sync::Arc;
        use std::thread;

        let sq = Arc::new(SeqQuote::new());
        let sq_writer = sq.clone();
        let sq_reader = sq.clone();

        let writer = thread::spawn(move || {
            for i in 0..1000 {
                let q = Quote { bid: i * PRICE_SCALE, ask: (i + 1) * PRICE_SCALE, ..Default::default() };
                sq_writer.write(&q, 0);
            }
        });

        let reader = thread::spawn(move || {
            for _ in 0..1000 {
                let q = sq_reader.read();
                // bid and ask should be consistent (ask = bid + PRICE_SCALE)
                if q.bid != 0 {
                    assert_eq!(q.ask, q.bid + PRICE_SCALE);
                }
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }

    fn info(status: &str) -> RichOrderInfo {
        RichOrderInfo {
            contract: api::Contract::default(),
            order: api::Order::default(),
            order_state: api::OrderState { status: status.to_string(), ..Default::default() },
            last_exec: api::Execution::default(),
        }
    }

    fn completed(order_id: u64) -> CompletedOrder {
        CompletedOrder {
            order_id, instrument: 0, status: crate::types::OrderStatus::Filled,
            filled_qty: 100, timestamp_ns: 0,
        }
    }

    /// A correction takes the completion notice with it.
    ///
    /// The venue can undo a trade that finished an order, which puts the order
    /// back to working. Only the memory that refuses a replay was cleared, so a
    /// completion already queued still went out afterwards and the caller was
    /// told the same order was both open and finished.
    #[test]
    fn a_correction_withdraws_a_completion_nobody_has_read_yet() {
        let shared = SharedState::new();
        shared.orders.push_completed_order(completed(9));
        shared.orders.push_completed_order(completed(10));

        shared.orders.push_order_correction(9, RichOrderInfo {
            contract: Default::default(),
            order: Default::default(),
            order_state: Default::default(),
            last_exec: Default::default(),
        });

        let seen: Vec<u64> = shared.orders.drain_completed_orders()
            .into_iter().map(|c| c.order_id).collect();
        assert_eq!(seen, vec![10], "the corrected order is not reported as finished: {seen:?}");
    }

    /// The replay flag belongs to the connection that earned it.
    ///
    /// Set once and never cleared, it outlives that connection: after a
    /// reconnect it answers from the previous session's record instead of
    /// waiting for the new one to name its working orders.
    #[test]
    fn a_new_connection_has_not_yet_named_what_it_has_working() {
        let shared = SharedState::new();
        assert!(!shared.orders.replay_done(), "nothing has been named yet");
        shared.orders.set_replay_done();
        assert!(shared.orders.replay_done());
        shared.orders.replay_is_pending();
        assert!(!shared.orders.replay_done(), "and a reconnect starts over");
    }

    /// And it has not named anything on it yet.
    ///
    /// The flag saying the venue had begun is what tells an account working
    /// nothing apart from one whose naming was cut short, and a withdrawal of
    /// every order reports the second and not the first. Carried across a
    /// reconnect it reports the second for ever: a later connection to an
    /// account with nothing working is answered with a partial-cancel warning
    /// about orders that do not exist.
    #[test]
    fn a_new_connection_has_not_named_anything_on_it_either() {
        let shared = SharedState::new();
        shared.orders.note_naming_began();
        assert!(shared.orders.naming_began());
        shared.orders.replay_is_pending();
        assert!(
            !shared.orders.naming_began(),
            "and a reconnect starts over",
        );
    }


    /// The bound the naming is waited on is spent from the moment the
    /// connection came up, not from the first caller to ask. A first request
    /// that waited the bound out otherwise spent it, and a global cancel
    /// issued straight after — the kill switch — waited nothing and said
    /// nothing.
    #[test]
    fn the_replay_bound_is_spent_from_the_connect_not_the_first_waiter() {
        let shared = SharedState::new();
        shared.orders.replay_is_pending();
        // The bound passes with nobody asking.
        std::thread::sleep(Duration::from_millis(3_200));
        let started = std::time::Instant::now();
        assert!(!shared.orders.wait_for_replay(), "the naming never finished");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a first waiter arriving after the bound has passed is answered at once, \
             not held for a fresh bound of its own",
        );
    }

    ///
    /// A producer announces each arrival on the notification flag; a
    /// consumer wakes on the wait alone and drains the arrivals only when it
    /// is told there are some. A timeout is therefore legitimate only when
    /// nothing is waiting — one that arrives with data still unread means a
    /// notification landed while the waiter was letting go of the flag, and
    /// the clear that followed wiped it. The hammer holds the notification
    /// lock briefly whenever it can, the way the hot loop and a dispatching
    /// caller contend for it; that is also what stretches the waiter's
    /// let-go wide enough for a notification to land in it. Each session
    /// ends in silence on purpose: a wiped notification is covered by the
    /// next one while they flow, so it is the last one that goes missing.
    #[test]
    fn a_wakeup_is_not_lost_to_the_waiter_that_is_waking() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        for session in 0..40u64 {
            let shared = Arc::new(SharedState::new());
            let queue: Arc<std::sync::Mutex<Vec<u64>>> =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            let done = Arc::new(AtomicBool::new(false));
            let stop = Arc::new(AtomicBool::new(false));
            let hammer = {
                let shared = shared.clone();
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if let Ok(held) = shared.notify_mutex.try_lock() {
                            let until = std::time::Instant::now() + Duration::from_micros(2);
                            while std::time::Instant::now() < until {
                                std::hint::spin_loop();
                            }
                            drop(held);
                        } else {
                            std::hint::spin_loop();
                        }
                    }
                })
            };
            let producer = {
                let shared = shared.clone();
                let queue = queue.clone();
                let done = done.clone();
                std::thread::spawn(move || {
                    for i in 1..=40u64 {
                        queue.lock().unwrap().push(i);
                        shared.notify();
                        std::thread::sleep(Duration::from_micros(50));
                    }
                    done.store(true, Ordering::Release);
                })
            };
            loop {
                if shared.wait_for_data(Duration::from_millis(10)) {
                    queue.lock().unwrap().clear();
                } else if done.load(Ordering::Acquire) {
                    assert!(
                        queue.lock().unwrap().is_empty(),
                        "session {session}: data arrived and no wakeup said so",
                    );
                    break;
                }
            }
            stop.store(true, Ordering::Relaxed);
            producer.join().unwrap();
            hammer.join().unwrap();
        }
    }

    /// A completed order is remembered as completed, so a replayed frame
    /// cannot write `Submitted` over the terminal entry and have
    /// `req_open_orders` report it as live. A strategy reading that would
    /// re-manage a position it already holds, or cancel an order that no
    /// longer exists, with the open-order snapshot corroborating it.
    #[test]
    fn a_completed_order_is_not_returned_to_the_open_book() {
        for terminal in ["Filled", "Cancelled", "Rejected"] {
            let shared = SharedState::new();
            shared.orders.push_order_info(7, info(terminal));

            for open in ["Submitted", "PreSubmitted", "PendingCancel", "PendingReplace"] {
                shared.orders.push_order_info(7, info(open));
                assert_eq!(
                    shared.orders.get_order_info(7).unwrap().order_state.status, terminal,
                    "{open} must not overwrite {terminal}",
                );
            }
            assert!(
                shared.orders.drain_open_orders().is_empty(),
                "and {terminal} stays out of the open-order snapshot",
            );
        }
    }

    /// A refused order is cached in the shape of a parked one — the status
    /// vocabulary has no refused string, and the refusal rides the completed
    /// status beside it. That shape is finished too: once the completion
    /// window has passed there is nothing left to refuse a replayed frame but
    /// the terminal-status guard, and a refused entry reported as live sends
    /// a strategy hedging a position it does not hold.
    #[test]
    fn a_refused_order_is_not_returned_to_the_open_book() {
        let shared = SharedState::new();
        shared.orders.push_order_info(7, RichOrderInfo {
            contract: api::Contract::default(),
            order: api::Order::default(),
            order_state: api::OrderState {
                status: "Inactive".to_string(),
                completed_status: "No valid bid/ask".to_string(),
                ..Default::default()
            },
            last_exec: api::Execution::default(),
        });

        for open in ["Submitted", "PreSubmitted", "PendingCancel", "PendingReplace"] {
            shared.orders.push_order_info(7, info(open));
            let cached = shared.orders.get_order_info(7).unwrap();
            assert_eq!(
                cached.order_state.status, "Inactive",
                "{open} must not overwrite a refusal",
            );
            assert_eq!(cached.order_state.completed_status, "No valid bid/ask");
        }
        assert!(
            shared.orders.drain_open_orders().is_empty(),
            "and the refusal stays out of the open-order snapshot",
        );

        // A genuinely parked order carries no completed status and has not
        // finished: the venue can bring it back to working, so a working
        // frame still supersedes it.
        shared.orders.push_order_info(8, info("Inactive"));
        shared.orders.push_order_info(8, info("Submitted"));
        assert_eq!(
            shared.orders.get_order_info(8).unwrap().order_state.status, "Submitted",
            "a parked order is not finished and still moves",
        );
    }

    /// Completing an order evicts its cache row, so the cached status cannot be
    /// what remembers the order is done — the replayed frame finds nothing to
    /// refuse and inserts itself. This is the ordinary path, not an edge case.
    #[test]
    fn a_completion_outlives_the_cache_row_it_evicts() {
        let shared = SharedState::new();
        shared.orders.push_order_info(7, info("Filled"));
        shared.orders.push_completed_order(completed(7));
        shared.orders.remove_order_info(7);

        shared.orders.push_order_info(7, info("Submitted"));
        assert!(
            shared.orders.get_order_info(7).is_none(),
            "a replayed frame must not re-open an order whose row has been evicted",
        );
        assert!(shared.orders.drain_open_orders().is_empty());
    }

    /// A terminal report arriving between the completion and the replay must
    /// not become the thing the guard compares against — a cached string is
    /// overwritten by the next terminal report, and the replay then passes.
    #[test]
    fn an_intervening_report_does_not_erase_the_completion() {
        let shared = SharedState::new();
        shared.orders.push_order_info(7, info("Filled"));
        shared.orders.push_completed_order(completed(7));

        shared.orders.push_order_info(7, info("Cancelled"));
        shared.orders.push_order_info(7, info("Submitted"));

        assert_ne!(
            shared.orders.get_order_info(7).unwrap().order_state.status, "Submitted",
            "the completion survives whatever terminal report lands on top of it",
        );
        assert!(shared.orders.drain_open_orders().is_empty());
    }

    /// A trade cancel or correction restates an execution the venue already
    /// reported, so it can return a filled order to a working quantity. It is
    /// the venue's statement, not a replay of an older one.
    #[test]
    fn a_trade_correction_can_reopen_a_completed_order() {
        let shared = SharedState::new();
        shared.orders.push_order_info(7, info("Filled"));
        shared.orders.push_completed_order(completed(7));
        shared.orders.remove_order_info(7);

        shared.orders.push_order_correction(7, info("PartiallyFilled"));
        assert_eq!(
            shared.orders.get_order_info(7).unwrap().order_state.status, "PartiallyFilled",
            "a correction is not a replay",
        );

        // And the order stops being remembered as completed, so its subsequent
        // ordinary reports are not refused either.
        shared.orders.push_order_info(7, info("Submitted"));
        assert_eq!(shared.orders.get_order_info(7).unwrap().order_state.status, "Submitted");
    }

    /// A correction and a delivered completion can reach the cache together.
    /// Whichever takes the lock first, the corrected working row survives.
    #[test]
    fn a_correction_racing_completion_cleanup_keeps_the_working_row() {
        let shared = SharedState::new();
        let gate = std::sync::Barrier::new(2);
        let mut lost = 0;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..250_000 {
                    let corrected = info("Submitted");
                    gate.wait();
                    shared.orders.push_order_correction(7, corrected);
                    gate.wait();
                }
            });
            for _ in 0..250_000 {
                shared.orders.push_order_info(7, info("Filled"));
                gate.wait();
                shared.orders.remove_completed_order_info(7);
                gate.wait();
                if !shared.orders.venue_is_working(7) { lost += 1; }
                shared.orders.drain_order_corrections();
            }
        });
        assert_eq!(lost, 0, "cleanup removed a correction's working row");
        shared.orders.push_order_info(7, info("Filled"));
        shared.orders.remove_completed_order_info(7);
        assert!(shared.orders.get_order_info(7).is_none(), "a finished row is still removed");
    }

    /// The ordinary direction still works — without this the guards above would
    /// pass against a cache that refuses every update.
    #[test]
    fn a_fill_still_writes_over_a_working_status() {
        let shared = SharedState::new();
        shared.orders.push_order_info(9, info("Submitted"));
        shared.orders.push_order_info(9, info("Filled"));
        assert_eq!(shared.orders.get_order_info(9).unwrap().order_state.status, "Filled");
    }

    /// Held by age, not by count. What the memory has to outlive is the window
    /// in which a stale frame for the order can still arrive; counting instead
    /// meant a busy session's unrelated completions pushed a still-relevant
    /// entry out, and the replay it was there to refuse got back in. Stays one
    /// short of COMPLETED_MAX so this exercises time-based retention only —
    /// the hard cap itself is a separate concern, proved below.
    #[test]
    fn a_completion_is_remembered_for_a_window_not_a_quota() {
        let shared = SharedState::new();
        shared.orders.push_completed_order(completed(7));

        // Far more unrelated completions than any small count-based bound
        // would hold, without reaching the hard cap.
        for id in 1000..(1000 + COMPLETED_MAX as u64 - 1) {
            shared.orders.push_completed_order(completed(id));
        }

        shared.orders.push_order_info(7, info("Submitted"));
        assert!(
            shared.orders.get_order_info(7).is_none(),
            "the completion survives however many other orders complete beside it",
        );
        assert!(shared.orders.drain_open_orders().is_empty());
    }

    /// And it does not accumulate for the life of the process: past the cap
    /// the oldest entries are dropped. None of these expire during the test
    /// (COMPLETED_RETENTION is minutes), so expiry-based pruning alone is a
    /// no-op here — only the hard eviction fallback can keep the map bounded.
    #[test]
    fn the_completed_memory_does_not_grow_without_limit() {
        let shared = SharedState::new();
        for id in 0..(COMPLETED_MAX as u64 + 10) {
            shared.orders.push_completed_order(completed(id));
        }
        let held = shared.orders.completed.lock().unwrap().len();
        assert!(held <= COMPLETED_MAX, "hard cap must hold even with nothing expired, held {held}");
    }
    #[test]
    fn seqquote_no_torn_reads() {
        use AtomicBool;
        use std::sync::Arc;
        use std::thread;

        // Every field of a given write carries the same value, so any reader
        // that ever observes a torn (half-old, half-new) struct will catch a
        // field disagreeing with `bid` here.
        fn quote_of(i: i64) -> Quote {
            Quote {
                bid: i, ask: i, last: i,
                bid_size: i, ask_size: i, last_size: i,
                volume: i, open: i, high: i, low: i, close: i,
                timestamp_ns: i as u64,
                bid_exch_mask: i, ask_exch_mask: i, last_exch_mask: i,
                // Every field moves together, so a snapshot mixing two
                // generations shows as a field disagreeing with the rest.
                halted: i,
            }
        }

        let sq = Arc::new(SeqQuote::new());
        let stop = Arc::new(AtomicBool::new(false));

        let writer = {
            let sq = sq.clone();
            thread::spawn(move || {
                for i in 1..=20_000i64 {
                    sq.write(&quote_of(i), 0);
                }
            })
        };

        let readers: Vec<_> = (0..4).map(|_| {
            let sq = sq.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let q = sq.read();
                    let v = q.bid;
                    let fields = [
                        q.ask, q.last, q.bid_size, q.ask_size, q.last_size,
                        q.volume, q.open, q.high, q.low, q.close,
                        q.timestamp_ns as i64, q.bid_exch_mask, q.ask_exch_mask, q.last_exch_mask,
                        q.halted,
                    ];
                    assert!(fields.iter().all(|&f| f == v), "torn SeqQuote read: bid={v} fields={fields:?}");
                }
            })
        }).collect();

        writer.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        for r in readers { r.join().unwrap(); }
    }
}

#[cfg(test)]
mod grant_tests {
    use super::*;

    #[test]
    fn the_older_spelling_takes_the_setting_and_the_grant() {
        let shared = SharedState::new();
        // The setting alone asks for it; the venue has granted nothing yet.
        assert!(shared.settings().island_for_nasdaq, "the documented default");
        assert!(!shared.island_for_nasdaq(), "and no grant is not a grant");

        shared.reference.set_enabled_features(vec!["NOAMOPTCHK".into()]);
        assert!(!shared.island_for_nasdaq(), "another grant is not this one");

        shared.reference.add_enabled_features(vec!["ISLAND2NASDAQ".into()]);
        assert!(shared.island_for_nasdaq(), "asked for and granted");
    }
}
