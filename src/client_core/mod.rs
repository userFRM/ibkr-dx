//! Shared dispatch core for Rust and Python EClient implementations.
//!
//! `ClientCore` owns all subscription tracking state (reqId maps, change-detection
//! snapshots, PnL/account subscriptions) and exposes "prepare" methods that return
//! intermediate structs. Language-specific EClient adapters convert these into their
//! respective callback formats (Rust `Wrapper` trait calls or PyO3 `call_method`).

pub(crate) mod attached_checks;
pub(crate) mod attached_orders;
pub(crate) mod attached_loading;
pub(crate) mod attached_prices;
pub(crate) mod attached_quote_contract;
pub(crate) mod attached_children;
pub(crate) mod attached_combos;
pub(crate) mod attached_combo_rules;

// The order-status vocabulary moved to the types it describes. Public here
// because that is the path a program written against this client already
// names, and used here for the same reason it was written.
pub use crate::types::order_status::{is_open_or_reactivatable, is_open_status, order_status_str};
use std::collections::{HashMap, HashSet};
use crate::error_codes::{
    CHANGE_CANNOT_CHANGE_TYPE, COMBINATION_LEG_INVALID, COMBINATION_NEEDS_LEGS, COMBO_AND_LEG_PRICES,
    CONDITION_CONTRACT_INCOMPLETE, DISCRETIONARY_AMOUNT_INVALID, DUPLICATE_TICKER_ID, GOOD_TILL_DATE_INVALID,
    E_TRADE_ONLY_DROPPED, E_TRADE_ONLY_WITHDRAWN, FIRM_QUOTE_ONLY_DROPPED, FIRM_QUOTE_ONLY_WITHDRAWN,
    NBBO_PRICE_CAP_DROPPED, NBBO_PRICE_CAP_WITHDRAWN,
    MISC_OPTION_KEY_INVALID, MISC_OPTION_VALUE_INVALID, NO_SUCH_BOOK, OCA_GROUP_REVISION,
    OCA_TYPE_REVISION, OPT_OUT_SMART_ROUTING_DROPPED, OPT_OUT_SMART_ROUTING_WITHDRAWN,
    MANUAL_CANCEL_TIME_INVALID, ORDER_TYPE_UNSUPPORTED, INVALID_ORDER_TYPE, PER_LEG_PRICES_UNSUPPORTED,
    REQUEST_NOT_PROCESSED, Refusal, SECURITY_NOT_PERMITTED, REQUEST_NOT_READ, TRIGGER_METHOD_INVALID, TRIGGER_PRICE_MISSING,
};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use std::sync::LazyLock;

use std::sync::mpsc::Sender;

use crate::types::model::{
    Contract as ApiContract, CommissionAndFeesReport as ApiCommissionAndFeesReport,
    Execution as ApiExecution, ExecutionFilter,
    Order as ApiOrder, TagValue,
    PRICE_SCALE_F,
};
use crate::bridge::SharedState;
use crate::types::*;

/// The only market data type the engine delivers (1 = realtime).
const MDT_REALTIME: i32 = 1;
const MDT_FROZEN: i32 = 2;
const MDT_DELAYED: i32 = 3;
const MDT_DELAYED_FROZEN: i32 = 4;

/// The feeds a session turns on, one bit each, as a gateway keeps them.
const FEED_FROZEN: i32 = 1;
const FEED_DELAYED: i32 = 2;
const FEED_DELAYED_FROZEN: i32 = 4;

/// The callback type for a subscription feed.
pub(crate) fn data_type_for_mode(mode: i32) -> i32 {
    match mode {
        1 => MDT_DELAYED,
        2 => MDT_FROZEN,
        3 => MDT_DELAYED_FROZEN,
        _ => MDT_REALTIME,
    }
}

// ── Tick type constants matching ibapi ──

/// Tick type 1: the bid.
pub const TICK_BID: i32 = 1;
/// Tick type 2: the ask.
pub const TICK_ASK: i32 = 2;
/// Tick type 4: the last.
pub const TICK_LAST: i32 = 4;
/// Tick type 6: the high.
pub const TICK_HIGH: i32 = 6;
/// Tick type 7: the low.
pub const TICK_LOW: i32 = 7;
/// Tick type 9: the close.
pub const TICK_CLOSE: i32 = 9;
/// Tick type 14: the open.
pub const TICK_OPEN: i32 = 14;
/// Tick type 0: the bid size.
pub const TICK_BID_SIZE: i32 = 0;
/// Tick type 3: the ask size.
pub const TICK_ASK_SIZE: i32 = 3;
/// Tick type 5: the last size.
pub const TICK_LAST_SIZE: i32 = 5;
/// Tick type 8: the volume.
pub const TICK_VOLUME: i32 = 8;
/// Tick type 45: the last timestamp.
pub const TICK_LAST_TIMESTAMP: i32 = 45;
/// Whether the venue has halted trading. Stated by the venue and delivered as
/// a generic tick, which is where the reference client puts it.
pub const TICK_HALTED: i32 = 49;
/// Tick type 32: the bid exchange.
pub const TICK_BID_EXCHANGE: i32 = 32;
/// Tick type 33: the ask exchange.
pub const TICK_ASK_EXCHANGE: i32 = 33;
/// Tick type 84: the last exchange.
pub const TICK_LAST_EXCHANGE: i32 = 84;

/// Whether one contract of this holding is worth more than one unit of the
/// price the venue quotes it at.
///
/// A quote is per unit, so an option or a future priced from one alone is
/// valued at a fraction of what it is worth — a hundredth, for the commonest
/// option multiplier. The venue states its own figure for such a position, and
/// that is what is reported rather than a price this arithmetic cannot use.
fn position_is_multiplied(pi: &PositionInfo) -> bool {
    match pi.multiplier.trim() {
        "" => false,
        stated => stated.parse::<f64>().is_ok_and(|m| m != 1.0),
    }
}

/// Render an exchange-code bitmask to a letter string, from the map of venues
/// the venue stated for this contract's BBO exchange. Each set bit picks the
/// letter the map gives that bit.
///
/// A map not stated yet renders nothing, and a bit it does not name adds
/// nothing.
pub fn render_exchange_mask(
    mask: i64, instrument: InstrumentId, shared: &SharedState,
) -> String {
    if mask == 0 {
        return String::new();
    }
    let components = shared.reference.smart_components_of(instrument);
    let mut out = String::with_capacity(8);
    let mut bits = mask as u64;
    while bits != 0 {
        let bit = bits.trailing_zeros() as i32;
        bits &= bits - 1;
        if let Some(c) = components.iter().find(|c| c.bit_number == bit) {
            out.push_str(&c.exchange_letter);
        }
    }
    out
}

// ── Intermediate dispatch structs ──

/// A single tick event produced by quote change detection.
pub struct TickEvent {
    /// The request this answers.
    pub req_id: i64,
    /// Which tick this is.
    pub tick_type: i32,
    /// What it is.
    pub value: f64,
    /// true = tick_price, false = tick_size
    pub is_price: bool,
}

/// Timestamp tick from quote polling.
pub struct TimestampTick {
    /// The request this answers.
    pub req_id: i64,
    /// When, in nanoseconds since the epoch.
    pub timestamp_ns: i64,
}

/// String-valued tick (e.g. exchange-code letters for tick_types 32/33/84).
pub struct StringTickEvent {
    /// The request this answers.
    pub req_id: i64,
    /// Which tick this is.
    pub tick_type: i32,
    /// What it is.
    pub value: String,
}

/// The number a delayed feed carries a tick under, as the reference client
/// numbers one: the bid, the ask and the last from 66, the sizes and the rest
/// after them, the halt and the timestamp under their own, and a bond's bid and
/// ask yields on 103 and 104.
///
/// A delayed feed has no number for the last yield. A gateway publishes the
/// bid's and the ask's yields on a delayed feed and not the last's, so
/// [`DELAYED_LAST_YIELD_UNSENT`] is kept back rather than numbered here.
fn as_delayed(tick_type: i32) -> i32 {
    match tick_type {
        TICK_BID => 66, TICK_ASK => 67, TICK_LAST => 68,
        TICK_BID_SIZE => 69, TICK_ASK_SIZE => 70, TICK_LAST_SIZE => 71,
        TICK_HIGH => 72, TICK_LOW => 73, TICK_VOLUME => 74, TICK_CLOSE => 75, TICK_OPEN => 76,
        TICK_LAST_TIMESTAMP => 88, TICK_HALTED => 90,
        50 => 103, 51 => 104,
        other => other,
    }
}

/// Every kind a snapshot is made of: bid, ask, last, open, close.
const SNAPSHOT_WHOLE: u16 = 1 | 2 | 4 | 8 | 16;

/// The venue's option model, 13, or 83 on a delayed feed: what a snapshot of a
/// contract a gateway marks as an option also waits for.
const SNAPSHOT_MODEL: u16 = 32;

/// The last trade's time on a delayed feed, 88: what a snapshot on a delayed
/// feed also waits for.
const SNAPSHOT_DELAYED_TIME: u16 = 64;

/// The bid's, the ask's and the last's computations, 10 to 12, or 80 to 82 on
/// a delayed feed: what a snapshot of a contract a gateway marks as an option
/// waits for beside the model.
const SNAPSHOT_SIDES: u16 = 128 | 256 | 512;

/// Where a snapshot on a frozen feed notes it has been sent the model on the
/// terms a gateway sends it there, and on a delayed-frozen one.
const SNAPSHOT_FROZEN_MODEL: u16 = 1024;
const SNAPSHOT_DELAYED_FROZEN_MODEL: u16 = 2048;

/// Where a snapshot notes that it has been sent an option computation of a
/// kind, which it is sent once.
fn snapshot_bit(kind: crate::bridge::OptionTickKind) -> u16 {
    use crate::bridge::OptionTickKind::{Ask, Bid, Last, Model};
    match kind {
        Model => SNAPSHOT_MODEL,
        Bid => 128,
        Ask => 256,
        Last => 512,
    }
}

/// The number an option computation of a kind goes out under.
fn option_tick_type(kind: crate::bridge::OptionTickKind, delayed: bool) -> i32 {
    use crate::bridge::OptionTickKind::{Ask, Bid, Last, Model};
    match (kind, delayed) {
        (Model, false) => MODEL_OPTION_COMPUTATION,
        (Model, true) => DELAYED_MODEL_OPTION_COMPUTATION,
        (Bid, false) => 10,
        (Bid, true) => 80,
        (Ask, false) => 11,
        (Ask, true) => 81,
        (Last, false) => 12,
        (Last, true) => 82,
    }
}

/// Whether every one of a computation's eight figures is stated.
fn every_figure_stated(figures: &[f64; 8]) -> bool {
    figures.iter().all(|figure| *figure != f64::MAX)
}

/// Whether a snapshot is owed an option computation of a kind: one stating
/// every figure, of a kind it has not been sent; and, on a frozen or a
/// delayed-frozen feed (`feed`), the model once whatever it states. Noted as
/// sent where it is.
fn owed_to_snapshot(
    wait: &mut SnapshotWait, kind: crate::bridge::OptionTickKind, figures: &[f64; 8], feed: i32,
) -> bool {
    let bit = snapshot_bit(kind);
    if wait.stated & bit == 0 && every_figure_stated(figures) {
        wait.stated |= bit;
        return true;
    }
    kind == crate::bridge::OptionTickKind::Model && frozen_model_owed(wait, feed)
}

/// Whether a snapshot on a frozen or a delayed-frozen feed is owed the model
/// on the terms a gateway sends it there: once, whatever it states. Sent so,
/// it does not complete the snapshot. Noted as sent where it is.
fn frozen_model_owed(wait: &mut SnapshotWait, feed: i32) -> bool {
    let bit = match feed {
        MDT_FROZEN => SNAPSHOT_FROZEN_MODEL,
        MDT_DELAYED_FROZEN => SNAPSHOT_DELAYED_FROZEN_MODEL,
        _ => return false,
    };
    let owed = wait.stated & bit == 0;
    wait.stated |= bit;
    owed
}

/// Whether a gateway marks a contract of this type as an option, and so holds
/// its snapshot for the option model too: options, futures options and index
/// options, and warrants and the type that extends them.
pub fn marked_as_option(sec_type: &str) -> bool {
    matches!(sec_type, "OPT" | "FOP" | "IOPT" | "WAR" | "EC")
}

/// A snapshot a caller is waiting on.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotWait {
    /// When it was asked for, which its bound runs from.
    pub asked_at: std::time::Instant,
    /// Which of the kinds it waits for it has been sent so far.
    pub stated: u16,
    /// The slot it is served on, whose feed says whether it is delayed.
    pub slot: InstrumentId,
    /// Whether its contract is of a type a gateway marks as an option.
    pub marked: bool,
}

impl SnapshotWait {
    /// One asked for now, nothing stated yet.
    pub fn new(slot: InstrumentId, marked: bool) -> Self {
        Self { asked_at: std::time::Instant::now(), stated: 0, slot, marked }
    }
}

/// The last trade's yield, which a delayed feed states on the same record a
/// live one does and a gateway does not publish from it.
const DELAYED_LAST_YIELD_UNSENT: i32 = 52;

impl ClientCore {
    /// Note this contract's feed as delayed, so a test can read what a caller
    /// who asked for delayed data reads.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn mark_feed_delayed_for_test(&self, instrument: InstrumentId) {
        self.mdt_by_instrument.lock().unwrap().insert(instrument, MDT_DELAYED);
    }

    /// Whether this contract's readings are the delayed feed's.
    ///
    /// The reference client numbers a delayed reading apart from a live one,
    /// so what is delivered under 13 on a live feed is delivered under 83 on a
    /// delayed one — a program that asked for delayed data reads it there.
    pub fn feed_is_delayed(&self, instrument: InstrumentId) -> bool {
        matches!(
            self.mdt_by_instrument.lock().unwrap().get(&instrument),
            Some(&MDT_DELAYED) | Some(&MDT_DELAYED_FROZEN)
        )
    }

    /// The feed a contract is served on as the venue accepted it: 1 live, 2
    /// frozen, 3 delayed, 4 delayed-frozen; live until it states one.
    fn feed_of(&self, instrument: InstrumentId) -> i32 {
        self.mdt_by_instrument.lock().unwrap().get(&instrument).copied().unwrap_or(MDT_REALTIME)
    }

    /// The tick an option computation goes out under, and each request it
    /// goes to with the figures that request is sent.
    ///
    /// Decided per request, as a gateway decides it. The model's is sent when
    /// any of its eight figures differs from the last model tick that request
    /// was sent and at least one figure is stated; the last is replaced
    /// whether or not it is sent, so a tick stating nothing is what the next
    /// one is compared with. The bid's, the ask's and the last's take each
    /// figure they do not state from the last one of their kind that request
    /// was sent, and are sent, and kept as the last, when that differs from
    /// it. A request joining a contract already modelled is sent the model as
    /// it stands when it joins, and the next tick of each other kind.
    ///
    /// A snapshot is sent each kind once, and only a computation stating all
    /// eight figures, and on a frozen feed the model once more whatever it
    /// states; what it has not been sent by its end is sent then
    /// (`check_snapshot_done`).
    pub fn option_tick_owed(
        &self, generation: u64, tick: &crate::bridge::OptionTick,
    ) -> (i32, Vec<(i64, [f64; 8])>) {
        use crate::bridge::OptionTickKind::Model;
        let tick_type = option_tick_type(tick.kind, self.feed_is_delayed(tick.instrument));
        if generation != self.generation_held(tick.instrument) {
            return (tick_type, Vec::new());
        }
        if tick.kind == Model {
            self.models_as_they_stand.lock().unwrap().insert(tick.instrument, (generation, *tick));
        }
        // Marked under the map a withdrawal clears, so a withdrawal lands
        // wholly before or wholly after: marked after it, a number reused on
        // the same option was never sent a tick it had not been sent.
        let feed = self.feed_of(tick.instrument);
        let own = self.ownership();
        let mut sent = self.option_ticks_sent.lock().unwrap();
        let mut snapshots = self.snapshot_reqs.lock().unwrap();
        let owed: Vec<(i64, [f64; 8])> = own.holders.get(&tick.instrument).copied().into_iter()
            .chain(own.following.get(&tick.instrument).into_iter().flatten().copied())
            .filter_map(|req_id| {
                let key = (req_id, tick.kind);
                let snapshot = snapshots.get_mut(&req_id);
                if tick.kind == Model {
                    let changed = sent.insert(key, tick.figures) != Some(tick.figures);
                    let owed = match snapshot {
                        // A snapshot is sent the model once, stating every
                        // figure, and on a frozen feed once whatever it
                        // states.
                        Some(wait) => changed && owed_to_snapshot(wait, tick.kind, &tick.figures, feed),
                        None => changed && tick.figures.iter().any(|figure| *figure != f64::MAX),
                    };
                    return owed.then_some((req_id, tick.figures));
                }
                let last = sent.get(&key).copied().unwrap_or([f64::MAX; 8]);
                let mut figures = tick.figures;
                for (figure, before) in figures.iter_mut().zip(last) {
                    if *figure == f64::MAX {
                        *figure = before;
                    }
                }
                let changed = figures != last;
                if changed {
                    sent.insert(key, figures);
                }
                let owed = match snapshot {
                    // And each side once, stating every figure, whether or
                    // not it moved.
                    Some(wait) => owed_to_snapshot(wait, tick.kind, &figures, feed),
                    None => changed,
                };
                owed.then_some((req_id, figures))
            })
            .collect();
        (tick_type, owed)
    }
}

/// Tick type 13: the option model's computation.
pub(crate) const MODEL_OPTION_COMPUTATION: i32 = 13;

/// The same on a delayed feed, which the reference client numbers apart.
///
/// A program that asked for delayed data reads its model there; delivered
/// under 13 it arrived indistinguishable from a live reading, on a feed the
/// caller had been told was delayed.
pub(crate) const DELAYED_MODEL_OPTION_COMPUTATION: i32 = 83;

/// Result of polling quotes for one instrument.
pub struct QuotePollResult {
    /// Numeric ticks that arrived.
    pub ticks: Vec<TickEvent>,
    /// Ticks the venue states under a number of its own rather than as a price
    /// or a size, delivered on `tick_generic`.
    /// What the venue says about the two prices rather than what they are, as
    /// it states them: one mask for whether each side may be dealt on without
    /// a human, one for pre-open and past-limit. Read per side where each
    /// price is handed over.
    pub eligible_mask: i64,
    /// The second of those, carried beside it.
    pub quote_state_mask: i64,
    pub generic_ticks: Vec<TickEvent>,
    /// Ticks whose value is text.
    pub string_ticks: Vec<StringTickEvent>,
    /// The moment the venue stamped the quote with, if it stated one.
    pub timestamp: Option<TimestampTick>,
    /// Whether this request's feed is delayed, so what follows goes out under
    /// the numbers the reference client gives a delayed feed.
    pub delayed: bool,
    /// true if any tick was delivered (for snapshot detection).
    pub delivered: bool,
    /// The venue's answer to a chargeable snapshot, each tick addressed to
    /// one of the snapshot's own requests and to nobody else watching the
    /// contract. Prices and sizes.
    pub snapshot_ticks: Vec<TickEvent>,
    /// The same answer's text: where each side is quoted, and the moment it
    /// was read.
    pub snapshot_strings: Vec<StringTickEvent>,
}

/// PnL update (account-level).
pub struct PnlUpdate {
    /// The request this answers.
    pub req_id: i64,
    /// What the account has made today.
    pub daily_pnl: f64,
    /// What its positions have made and not realised.
    pub unrealized_pnl: f64,
    /// What it has realised.
    pub realized_pnl: f64,
}

/// PnL single update (per-position).
pub struct PnlSingleUpdate {
    /// The request this answers.
    pub req_id: i64,
    /// How much is held.
    pub pos: f64,
    /// What the account has made today.
    pub daily_pnl: f64,
    /// What its positions have made and not realised.
    pub unrealized_pnl: f64,
    /// What it has realised.
    pub realized_pnl: f64,
    /// What it is.
    pub value: f64,
}

/// A single changed account field.
pub struct AccountFieldUpdate {
    /// Which figure.
    pub key: String,
    /// What it is.
    pub value: String,
    /// What currency it is stated in.
    pub currency: String,
}

/// Batch of account update results.
pub struct AccountUpdateBatch {
    /// Each figure that changed.
    pub fields: Vec<AccountFieldUpdate>,
    /// Whether the account has just been stated whole: true on the pass after
    /// the venue ended the download it was sending, once per download, so a
    /// rebuilt connection's download ends again.
    pub finished: bool,
}

/// Prepared account summary response.
pub struct AccountSummaryBatch {
    /// The request this answers.
    pub req_id: i64,
    /// Each figure answering the request.
    pub entries: Vec<AccountSummaryEntry>,
}

/// One figure answering a summary request.
pub struct AccountSummaryEntry {
    /// The account whose figure this is.
    pub account: String,
    /// Which figure this is, under the venue's name for it. Owned for the
    /// same reason the currency is: the set is the venue's, not a fixed list
    /// known here, and a summary built from such a list reported nothing for
    /// every figure that was not on it.
    pub tag: String,
    /// What it is.
    pub value: String,
    /// As the venue stated it for this figure. Owned rather than borrowed
    /// because it is the venue's word, not one of a fixed set known here.
    pub currency: String,
}

/// A single portfolio position update.
pub struct PortfolioUpdateEntry {
    /// The contract.
    pub con_id: i64,
    /// How much is held.
    pub position: f64,
    /// What it cost on average.
    pub avg_cost: f64,
    /// What it is worth now, each.
    pub market_price: f64,
    /// What the holding is worth.
    pub market_value: f64,
    /// What its positions have made and not realised.
    pub unrealized_pnl: f64,
    /// What it has realised.
    pub realized_pnl: f64,
}

/// A slot's quote as a read took it before its cut, with what rode beside it.
///
/// Taken whole in the read's first step and compared with what the caller was
/// last told only in its last, once the read's records have been delivered —
/// and only where the slot is still held under the occupancy it was taken
/// under, so a quote of a contract that has left the slot is never delivered
/// as the next one's.
pub struct PolledQuote {
    /// The slot.
    pub iid: InstrumentId,
    /// The occupancy the quote was written under.
    pub generation: u64,
    /// The quote.
    pub quote: Quote,
    /// What the venue says about its two prices.
    pub masks: (i64, i64),
    /// The extra series stated since the last read.
    pub series: Vec<crate::types::SeriesTick>,
    /// The answer to a chargeable snapshot, where one came.
    pub snapshot_answer: Option<Vec<crate::types::SeriesTick>>,
}

/// What a read took of the conflated state before its cut: the quotes, and
/// every figure this side compares with what its caller was last told.
///
/// Delivered after the read's records. Where the read takes the session's
/// last record, it is taken again and that is what is delivered: every writer
/// has stopped by then, so it is final.
#[derive(Default)]
pub struct Polled {
    /// Each held slot's quote.
    pub quotes: Vec<PolledQuote>,
    /// The holdings that moved, where anyone watches them.
    pub positions: Vec<PositionInfo>,
    /// Changes to holdings of other accounts.
    pub named_positions: Vec<(String, PositionInfo)>,
    /// The account's running profit, where it moved.
    pub pnl: Vec<PnlUpdate>,
    /// Each position's, where it moved.
    pub pnl_single: Vec<PnlSingleUpdate>,
    /// The account's figures, where subscribed.
    pub account: Option<AccountUpdateBatch>,
    /// And its holdings beside them.
    pub portfolio: Vec<PortfolioUpdateEntry>,
    /// The figures each multi-account request has not been told.
    pub multi: Vec<(i64, Vec<AccountFieldUpdate>)>,
    /// The summaries due.
    pub summaries: Vec<AccountSummaryBatch>,
    /// The maps of venues asked for early, answered or refused.
    pub smart_components: Vec<(i64, Result<Vec<crate::types::SmartComponent>, Refusal>)>,
}

impl Polled {
    /// This, with what a later poll took laid over it: the later value where
    /// both hold one, and what only the earlier drained kept.
    pub fn then(mut self, later: Polled) -> Polled {
        let mut quotes = later.quotes;
        for earlier in self.quotes {
            match quotes.iter_mut().find(|q| q.iid == earlier.iid) {
                Some(q) => {
                    let mut series = earlier.series;
                    series.append(&mut q.series);
                    q.series = series;
                    if q.snapshot_answer.is_none() {
                        q.snapshot_answer = earlier.snapshot_answer;
                    }
                }
                None => quotes.push(earlier),
            }
        }
        for pi in later.positions {
            match self.positions.iter_mut().find(|p| p.con_id == pi.con_id) {
                Some(p) => *p = pi,
                None => self.positions.push(pi),
            }
        }
        for update in later.pnl {
            self.pnl.retain(|u| u.req_id != update.req_id);
            self.pnl.push(update);
        }
        for update in later.pnl_single {
            self.pnl_single.retain(|u| u.req_id != update.req_id);
            self.pnl_single.push(update);
        }
        let account = match (self.account, later.account) {
            (Some(mut a), Some(b)) => {
                for field in b.fields {
                    a.fields.retain(|f| !(f.key == field.key && f.currency == field.currency));
                    a.fields.push(field);
                }
                a.finished |= b.finished;
                Some(a)
            }
            (a, b) => b.or(a),
        };
        for entry in later.portfolio {
            self.portfolio.retain(|e| e.con_id != entry.con_id);
            self.portfolio.push(entry);
        }
        for (req_id, fields) in later.multi {
            match self.multi.iter_mut().find(|(id, _)| *id == req_id) {
                Some((_, held)) => {
                    for field in fields {
                        held.retain(|f| !(f.key == field.key && f.currency == field.currency));
                        held.push(field);
                    }
                }
                None => self.multi.push((req_id, fields)),
            }
        }
        for (account, position) in later.named_positions {
            self.named_positions.retain(|(a, p)| a != &account || p.con_id != position.con_id);
            self.named_positions.push((account, position));
        }
        self.summaries.extend(later.summaries);
        self.smart_components.extend(later.smart_components);
        Polled {
            quotes,
            positions: self.positions,
            named_positions: self.named_positions,
            pnl: self.pnl,
            pnl_single: self.pnl_single,
            account,
            portfolio: self.portfolio,
            multi: self.multi,
            summaries: self.summaries,
            smart_components: self.smart_components,
        }
    }
}

#[derive(Default)]
pub(crate) struct AccountRoutes {
    updates: String,
    multi: HashMap<i64, (String, String)>,
    pub(crate) positions: HashMap<i64, (String, String)>,
    summaries: HashMap<i64, Vec<String>>,
}

/// One summary's last pass: when it ran, and what it stated then, keyed by the
/// figure and the currency it was stated in.
type SummaryPass = (std::time::Instant, HashMap<(String, bool, String, String), String>);

/// Fold the risk levels modelled here to the venue's names. Anything else
/// travels as text in the parameter list; the venue owns that vocabulary.
fn parse_risk_aversion(raw: Option<&str>) -> Option<RiskAversion> {
    match raw?.to_lowercase().as_str() {
        "neutral" => Some(RiskAversion::Neutral),
        "get_done" | "getdone" => Some(RiskAversion::GetDone),
        "aggressive" => Some(RiskAversion::Aggressive),
        "passive" => Some(RiskAversion::Passive),
        _ => None,
    }
}

/// Fold the flag spellings modelled here to the `1`/`0` the venue is known to
/// take. Anything else travels as text, for the reason a risk level does.
fn parse_algo_flag(raw: Option<&str>) -> Option<bool> {
    match raw?.to_lowercase().as_str() {
        "0" | "false" => Some(false),
        "1" | "true" => Some(true),
        _ => None,
    }
}

/// The parameters a strategy this client models reads off the caller's list.
///
/// A strategy that is not here is handed to the venue with the caller's list
/// as written, so it has no set to state. So is one that is here and was given
/// a key outside its set: the set decides whether the list is re-encoded from
/// named fields or forwarded whole, not whether the caller may state a key.
fn algo_param_names(strategy: &str) -> Option<&'static [&'static str]> {
    Some(match strategy {
        "vwap" => &["maxPctVol", "noTakeLiq", "allowPastEndTime", "startTime", "endTime"],
        "twap" => &["allowPastEndTime", "startTime", "endTime"],
        "arrivalpx" | "arrival_price" => &[
            "maxPctVol", "riskAversion", "allowPastEndTime", "forceCompletion",
            "startTime", "endTime",
        ],
        "closepx" | "close_price" => {
            &["maxPctVol", "riskAversion", "forceCompletion", "startTime"]
        }
        "darkice" | "dark_ice" => {
            &["allowPastEndTime", "displaySize", "startTime", "endTime"]
        }
        "pctvol" | "pct_vol" => &["pctVol", "noTakeLiq", "startTime", "endTime"],
        _ => return None,
    })
}

/// Parse algo strategy and TagValue params into internal AlgoParams.
///
/// A key the caller never set is not stated: the venue's own default for it
/// is not known here, and a value sent in its place — `0`, an empty time,
/// Neutral — is a claim the caller did not make.
///
/// A strategy modelled here is re-encoded from the fields it names, and a key
/// it does not name has no field to be re-encoded into. That is a limit of the
/// re-encoding, not of the protocol: there is no tag per parameter — a name and
/// a value travel as a pair in a repeating group, and the venue reads a pair
/// whose name this client never modelled exactly as it reads one it did. So a
/// list carrying such a key is forwarded whole, as the caller wrote it and in
/// the order they wrote it, rather than refused or quietly shortened.
///
/// A value is checked, not re-spelled. A parameter is text on the wire, and
/// the text the caller wrote is what reaches the venue, as the reference
/// client forwards it; the parse here is this client's own check that it
/// reads. Two kinds are sent in the venue's spelling instead, each said where
/// it is read: a known flag goes as `1`/`0`, and a known `riskAversion` as the
/// venue names it. Other spellings travel as written.
pub fn parse_algo_params(strategy: &str, params: &[TagValue]) -> Result<AlgoParams, Refusal> {
    let folded = strategy.to_lowercase();
    // The caller's list as they wrote it: name then value, in their order. The
    // caller's own spelling of the strategy too, since the venue is handed that
    // name and does not know a lower-cased one.
    let as_written = || AlgoParams::Named {
        strategy: strategy.to_string(),
        params: params
            .iter()
            .flat_map(|tv| [tv.tag.clone(), tv.value.clone()])
            .collect(),
    };
    // A value outside the vocabulary folded here takes the route a key outside
    // the set takes: the list goes as the caller wrote it, and the venue answers
    // for a spelling it does not know. Folded to "unset" instead, the parameter
    // was dropped from the order and nothing said so — the caller asked for a
    // risk level or a flag and the order went without one.
    let outside_the_vocabulary = |tv: &TagValue| match tv.tag.as_str() {
        "riskAversion" => parse_risk_aversion(Some(&tv.value)).is_none(),
        "noTakeLiq" | "allowPastEndTime" | "forceCompletion" => {
            parse_algo_flag(Some(&tv.value)).is_none()
        }
        _ => false,
    };
    if let Some(known) = algo_param_names(&folded)
        && params.iter().any(|tv| {
            !known.contains(&tv.tag.as_str()) || outside_the_vocabulary(tv)
        })
    {
        return Ok(as_written());
    }
    let get = |key: &str| -> Option<String> {
        params.iter().find(|tv| tv.tag == key).map(|tv| tv.value.clone())
    };
    let get_num = |key: &str| -> Result<Option<String>, Refusal> {
        let raw = match get(key) {
            None => return Ok(None),
            Some(raw) => raw,
        };
        let v: f64 = raw.parse()
            .map_err(|_| Refusal::validation(format!("Invalid {key} '{raw}': expected a number")))?;
        if !v.is_finite() {
            return Err(Refusal::validation(
                format!("Invalid {key} '{raw}': must be a finite number"),
            ));
        }
        Ok(Some(raw))
    };
    // Every flag reaching here is one the fold above recognised: a spelling it
    // did not took the whole list down the text path.
    let get_bool = |raw: Option<&str>| parse_algo_flag(raw);

    let algo: Result<AlgoParams, Refusal> = match folded.as_str() {
        "vwap" => Ok(AlgoParams::Vwap {
            max_pct_vol: get_num("maxPctVol")?,
            no_take_liq: get_bool(get("noTakeLiq").as_deref()),
            allow_past_end_time: get_bool(get("allowPastEndTime").as_deref()),
            start_time: get("startTime"),
            end_time: get("endTime"),
        }),
        "twap" => Ok(AlgoParams::Twap {
            allow_past_end_time: get_bool(get("allowPastEndTime").as_deref()),
            start_time: get("startTime"),
            end_time: get("endTime"),
        }),
        "arrivalpx" | "arrival_price" => Ok(AlgoParams::ArrivalPx {
            max_pct_vol: get_num("maxPctVol")?,
            risk_aversion: parse_risk_aversion(get("riskAversion").as_deref()),
            allow_past_end_time: get_bool(get("allowPastEndTime").as_deref()),
            force_completion: get_bool(get("forceCompletion").as_deref()),
            start_time: get("startTime"),
            end_time: get("endTime"),
        }),
        "closepx" | "close_price" => Ok(AlgoParams::ClosePx {
            max_pct_vol: get_num("maxPctVol")?,
            risk_aversion: parse_risk_aversion(get("riskAversion").as_deref()),
            force_completion: get_bool(get("forceCompletion").as_deref()),
            start_time: get("startTime"),
        }),
        "darkice" | "dark_ice" => {
            // Stated, not chosen here. A display size is how much of the
            // order the book shows; a default would show a size the caller
            // never asked to show.
            let display_size = match get("displaySize") {
                None => return Err(Refusal::validation(
                    "DarkIce needs a displaySize: it is how much of the order the \
                     book shows, and this client will not choose it for you",
                )),
                Some(raw) => {
                    raw.parse::<u32>().map_err(|_| format!("Invalid displaySize '{raw}': expected a non-negative integer"))?;
                    raw
                }
            };
            Ok(AlgoParams::DarkIce {
                allow_past_end_time: get_bool(get("allowPastEndTime").as_deref()),
                display_size,
                start_time: get("startTime"),
                end_time: get("endTime"),
            })
        }
        "pctvol" | "pct_vol" => Ok(AlgoParams::PctVol {
            pct_vol: get_num("pctVol")?,
            no_take_liq: get_bool(get("noTakeLiq").as_deref()),
            start_time: get("startTime"),
            end_time: get("endTime"),
        }),
        // Anything else goes as the caller wrote it.
        //
        // Refused here instead, a caller could use only the algorithms this
        // match happens to name — five of the thirteen an ordinary session is
        // offered. Which ones an account may use is the venue's answer, stated
        // at logon and enforced by it, and the reference client does not
        // interpret these either.
        _ => return Ok(as_written()),
    };
    algo
}

// ── Order field validation ──

/// Reject a price/amount field that a saturating float-to-int cast would
/// otherwise turn into a different, valid-looking number: NaN becomes 0,
/// +/-Infinity becomes i64::MAX/MIN, and a finite value whose fixed-point
/// form overflows i64 saturates the same way.
pub(crate) fn require_finite_price(field: &str, v: f64) -> Result<(), String> {
    // `i64::MAX as f64` itself rounds up to 2^63 (f64 cannot represent
    // i64::MAX exactly), so a strict `>` lets a scaled value of exactly
    // 2^63 through and the subsequent `as i64` cast saturates to i64::MAX
    // instead of being refused. `>=` excludes that boundary.
    if !v.is_finite() || (v * PRICE_SCALE_F).abs() >= i64::MAX as f64 {
        return Err(format!(
            "{field} must be a finite number representable on the wire, got {v}"
        ));
    }
    Ok(())
}

/// Parse the Adaptive algo's `adaptivePriority` tag. A missing tag defaults
/// to Normal (IB's own default); a present-but-unrecognized value is
/// refused instead of silently defaulting to Normal.
fn adaptive_priority(params: &[TagValue]) -> Result<AdaptivePriority, String> {
    match params.iter().find(|tv| tv.tag == "adaptivePriority") {
        None => Ok(AdaptivePriority::Normal),
        Some(tv) => match tv.value.as_str() {
            "Patient" => Ok(AdaptivePriority::Patient),
            "Normal" => Ok(AdaptivePriority::Normal),
            "Urgent" => Ok(AdaptivePriority::Urgent),
            other => Err(format!(
                "Unknown adaptivePriority '{other}': expected Patient, Normal or Urgent"
            )),
        },
    }
}

// ── Execution storage ──

/// Does a stored execution satisfy an `ExecutionFilter`? Shared by the index
/// form and the snapshot form so the two cannot disagree about what matches.
fn execution_matches(se: &StoredExecution, filter: &ExecutionFilter) -> bool {
    if !filter.symbol.is_empty() && !se.contract.symbol.eq_ignore_ascii_case(&filter.symbol) {
        return false;
    }
    if !filter.sec_type.is_empty() && !se.contract.sec_type.eq_ignore_ascii_case(&filter.sec_type) {
        return false;
    }
    if !filter.exchange.is_empty() && !se.execution.exchange.eq_ignore_ascii_case(&filter.exchange) {
        return false;
    }
    // A stored execution carries the venue's word for the side, and a filter
    // states the order action. Compared as written, a filter for buys matched
    // nothing and the caller read an empty answer as "no fills". Both
    // vocabularies are accepted here, in the one place both surfaces compare,
    // rather than mapped on the way in by one of them and not the other.
    if !filter.side.is_empty() {
        let wanted = match filter.side.to_ascii_uppercase().as_str() {
            "BUY" | "BOT" => "BOT",
            "SELL" | "SSHORT" | "SLD" => "SLD",
            _ => &filter.side,
        };
        if !se.execution.side.eq_ignore_ascii_case(wanted) {
            return false;
        }
    }
    if !filter.acct_code.is_empty() && !se.execution.acct_number.eq_ignore_ascii_case(&filter.acct_code) {
        return false;
    }
    if filter.client_id != 0 && se.execution.client_id != filter.client_id {
        return false;
    }
    // ibapi treats `time` as a lower bound — executions at or after it. The
    // two sides can be punctuated differently ("20260729-10:00:00" against
    // "20260729 10:00:00"), so compare on digits alone; both are yyyyMMdd
    // first, so that ordering is chronological. A bound carrying less
    // precision than the timestamp compares against the same prefix, so a
    // date-only filter keeps that whole day rather than dropping it. An
    // execution the venue stated no time for compares on nothing and is kept:
    // it cannot be placed either side of a bound, and a caller asking what has
    // traded is worse served by a fill they are never shown.
    if !filter.time.is_empty() {
        let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
        let lo = digits(&filter.time);
        let at = digits(&se.execution.time);
        let n = lo.len().min(at.len());
        if at.get(..n).unwrap_or("") < lo.get(..n).unwrap_or("") {
            return false;
        }
    }
    true
}

/// The days a request for executions is answered for, settled as a gateway
/// settles them, or `None` where it is answered from the executions held
/// without regard to the day.
///
/// `last_n_days` counts from 1 to 7 and asks for no window otherwise. A date
/// of 8 or less is dropped. One that is not a day of the calendar refuses the
/// request, as a gateway refuses it while reading it. A date is kept where it
/// falls after the day a week before `today` and not after `today`; the rest
/// are dropped and said in the log, not refused. A date named twice is one
/// day. The window is applied only where it asks for more than today's
/// executions: more than one day back, more than one date, or a date other
/// than today.
pub fn execution_days(
    last_n_days: i32, specific_dates: &[i32], today: jiff::civil::Date,
) -> Result<Option<Vec<jiff::civil::Date>>, Refusal> {
    let n = if (1..=7).contains(&last_n_days) { last_n_days } else { 0 };
    let week_ago = today.saturating_sub(jiff::Span::new().days(7));
    let mut dates = Vec::new();
    let mut dropped = Vec::new();
    for &stated in specific_dates.iter().filter(|d| **d > 8) {
        let day = calendar_day(stated).ok_or_else(|| Refusal::stated(
            crate::error_codes::REQUEST_NOT_READ,
            format!("Error reading request: {stated} in specificDates is not a date"),
        ))?;
        if week_ago < day && day <= today { dates.push(day) } else { dropped.push(day.to_string()) }
    }
    if !dropped.is_empty() {
        log::info!(
            "The dates: [{}] are outside of the acceptable range of 7 days back from {week_ago} and {today} .",
            dropped.join(", "),
        );
    }
    dates.sort();
    dates.dedup();
    let history = n > 1 || dates.len() > 1 || (!dates.is_empty() && !dates.contains(&today));
    if !history {
        return Ok(None);
    }
    let mut days: Vec<jiff::civil::Date> = (0..n)
        .map(|back| today.saturating_sub(jiff::Span::new().days(back)))
        .collect();
    days.extend(dates);
    Ok(Some(days))
}

/// The day a `yyyymmdd` names, read as a gateway reads it, or `None` where its
/// month or day is not one the calendar has.
///
/// ponytail: a year past 9999 is checked on the year its 400-year cycle puts
/// in the last one this calendar counts, and so falls after any window; a log
/// line naming it names that year.
fn calendar_day(stated: i32) -> Option<jiff::civil::Date> {
    let (year, month, day) = (stated / 10_000, stated / 100 % 100, stated % 100);
    let year = if year > 9999 { 9600 + year % 400 } else { year };
    jiff::civil::Date::new(
        i16::try_from(year).ok()?, i8::try_from(month).ok()?, i8::try_from(day).ok()?,
    ).ok()
}

/// A stored execution + commission_and_fees pair for `req_executions` replay.
/// Shared between Rust and Python adapters via `ClientCore`.
#[derive(Clone)]
pub struct StoredExecution {
    /// The contract it is on.
    pub contract: ApiContract,
    /// The fill itself.
    pub execution: ApiExecution,
    /// What the fill cost.
    pub commission_and_fees: ApiCommissionAndFeesReport,
}

/// The session's executions, and where each named one sits.
///
/// Indexed by the venue's id: a fill and its charge arrive as two messages,
/// and the charge finds its execution by name. Scanned instead, every fill
/// and every charge cost a pass over the day so far, on the thread that also
/// delivers the callbacks. Nothing leaves the rows but a `reset`, so an index
/// into them stays good.
#[derive(Default)]
pub struct ExecutionStore {
    rows: Vec<StoredExecution>,
    by_id: HashMap<String, usize>,
}

// ── Order tracking ──

/// A locally tracked order for `req_open_orders` / dispatch status updates.
#[derive(Clone)]
pub struct TrackedOrder {
    /// The contract it is on.
    pub contract: ApiContract,
    /// The order as this client sent it.
    pub order: ApiOrder,
    /// Where it stands.
    pub status: String,
    /// How much has filled.
    pub filled: f64,
    /// How much has not.
    pub remaining: f64,
    /// What everything filled so far averaged, as the venue stated it, and
    /// what the last of it went at.
    ///
    /// Nought where the venue has stated nothing — which is what nought means
    /// on the callback that carries them. Answered as nought on an order the
    /// venue had stated an average for, a caller read "filled three hundred
    /// at an average of nothing", which is not a reading it can tell from a
    /// real one.
    pub avg_fill_price: f64,
    /// What the last fill on it went at.
    pub last_fill_price: f64,
    /// The engine's own slot for the contract.
    pub instrument: InstrumentId,
    /// The terms the venue is known to hold, kept while a replacement of them
    /// is outstanding.
    ///
    /// The record takes the attempt ahead of the venue's answer, because every
    /// later action restates from it. Where the answer is a refusal the attempt
    /// must not stand: without this the record kept terms the venue had said no
    /// to, and the next cancel or replace went out restating them.
    pub before_the_replace: Option<Box<ApiOrder>>,
    /// True once this order's last transition was a genuine Rejected (FIX
    /// 39=8). Rejected and Inactive both stringify to `status == "Inactive"`
    /// (ibapi has no Rejected string), so that string alone cannot tell a
    /// dead order from a parked, reactivatable one — `collect_open_orders`
    /// uses this flag as the discriminator instead of widening
    /// `is_open_status`.
    pub rejected: bool,
    /// Whether this client placed the order, as against learning of it from
    /// the venue — through a status, or by restating one the venue named.
    /// A replace of an order placed here restates the engine's own record;
    /// a replace of any other goes behind the caller's statement of it.
    pub placed_here: bool,
}

// ── ClientCore ──

/// An answer owed to a caller about a display group.
#[derive(Debug, Clone)]
pub enum GroupEvent {
    /// The groups on offer, `|`-separated.
    List(i64, String),
    /// What a group now holds.
    Updated(i64, String),
}

/// Put back what a restatement replaced, and square the outstanding quantity
/// with it. Does nothing where nothing was kept.
fn put_back_the_terms(tracked: &mut TrackedOrder) {
    if let Some(held) = tracked.before_the_replace.take() {
        tracked.order = *held;
        tracked.remaining = (tracked.order.total_quantity - tracked.filled).max(0.0);
    }
}

/// The record of an order as this client has just placed it.
///
/// Written by more than one path — held back, or sent — so the shape of a
/// freshly placed order is stated once.
fn tracked_as_placed(
    contract: ApiContract, order: ApiOrder, instrument: InstrumentId,
) -> TrackedOrder {
    let remaining = order.total_quantity;
    TrackedOrder {
        contract, order, status: "PendingSubmit".into(), filled: 0.0, remaining, instrument,
        // Nothing has filled, so there is no price the venue has stated for
        // one, which is what nought means on the callback that carries them.
        avg_fill_price: 0.0, last_fill_price: 0.0,
        rejected: false, before_the_replace: None, placed_here: true,
    }
}

/// What one historical request asked for.
///
/// Kept because the reply states neither. The range written beside the last
/// bar is the request's own, and the bar times are written in the form the
/// request named.
#[derive(Clone, Default)]
pub struct HistoricalAsk {
    /// 2 for seconds since the epoch, anything else for the venue's spelling.
    pub format_date: i32,
    /// The end the caller named, or empty for the moment of asking.
    pub end_date_time: String,
    /// How far back from that end, in the venue's own units.
    pub duration: String,
    /// The zone the venue stated the series on, once it has, for the bars
    /// that continue it.
    pub zone: String,
    /// Whether its bars are a day long or longer, and so dated by the day.
    pub by_day: bool,
    /// The latest timed daily session supplied with the history.
    pub daily_session: Option<(i64, i64)>,
}

/// What a market-data request's generic tick list asks for.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct GenericTicks<'a> {
    /// The contract's headlines, asked for under 292.
    pub(crate) news: bool,
    /// The providers the headlines entry named, joined the way the venue
    /// separates them — `BRFG*DJNL` — and empty where it named none.
    pub(crate) news_providers: String,
    /// Every other series, by the venue's number for it, once each.
    pub(crate) series: Vec<u32>,
    /// Entries that are not a number the venue knows a series by.
    pub(crate) unread: Vec<&'a str>,
}

/// Read a generic tick list, one comma-separated entry at a time.
///
/// The whole entry, never a number ending in one: `1292` is series 1292 and
/// not the headlines. `292` asks for the headlines from the providers the
/// session already names, and `292:BRFG+DJNL` from the providers named after
/// the colon, joined by `+` as the reference client writes them. `mdoff` is
/// passed over: it is not a series, and the quote is subscribed regardless.
pub(crate) fn parse_generic_tick_list(list: &str) -> GenericTicks<'_> {
    let mut read = GenericTicks::default();
    let mut providers: Vec<&str> = Vec::new();
    for entry in list.split(',').map(str::trim) {
        if entry.is_empty() || entry == "mdoff" {
            continue;
        }
        if let Some(named) = entry
            .strip_prefix("292")
            .filter(|rest| rest.is_empty() || rest.starts_with(':'))
        {
            read.news = true;
            providers.extend(
                named.trim_start_matches(':').split('+').map(str::trim).filter(|p| !p.is_empty()),
            );
            continue;
        }
        match entry.parse::<u32>() {
            Ok(tick) if !read.series.contains(&tick) => read.series.push(tick),
            Ok(_) => {}
            Err(_) => read.unread.push(entry),
        }
    }
    read.news_providers = providers.join("*");
    read
}

/// What a request asking for a contract ended up as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Joined {
    /// It holds the subscription: nobody held the contract, or it took the
    /// slot from the venue's one-shot.
    Took,
    /// It took the slot from the one-shot that held it, which now watches it.
    TookFromTheOneShot,
    /// Somebody else holds the subscription and this one watches it.
    Watching,
}

/// The maps that say who is watching what, held together for one decision.
///
/// Held for as long as the decision takes and no longer. Nothing that can take
/// one of them again — the news record, a callback, anything reached through
/// the engine — may be called while this is alive.
struct Ownership<'a> {
    /// The one request each slot's subscription belongs to.
    holders: std::sync::MutexGuard<'a, HashMap<InstrumentId, i64>>,
    /// Every other request watching that slot.
    following: std::sync::MutexGuard<'a, HashMap<InstrumentId, Vec<i64>>>,
    /// Which slot a request is watching.
    by_req: std::sync::MutexGuard<'a, HashMap<i64, InstrumentId>>,
    /// The requests that asked for the venue's chargeable one-shot.
    one_shot: std::sync::MutexGuard<'a, HashSet<i64>>,
    /// What each request asked for beyond the quote.
    series: std::sync::MutexGuard<'a, HashMap<i64, Vec<u32>>>,
    /// When each slot was taken, in the order of what this client has asked
    /// for.
    taken_on: std::sync::MutexGuard<'a, HashMap<InstrumentId, u64>>,
    /// And which contract the request that took it named.
    took_contract: std::sync::MutexGuard<'a, HashMap<InstrumentId, i64>>,
    /// Which subscription each request is holding, as a figure that changes
    /// every time it takes a new one.
    epoch: std::sync::MutexGuard<'a, HashMap<i64, u64>>,
}

impl Ownership<'_> {
    /// Record a request as watching a slot somebody else holds.
    fn watches(&mut self, instrument: InstrumentId, req_id: i64) {
        let watchers = self.following.entry(instrument).or_default();
        if !watchers.contains(&req_id) {
            watchers.push(req_id);
        }
    }

    /// Hold this contract, or watch whoever took it first.
    ///
    /// The venue's one-shot was sent as a request of its own, so it is not
    /// what a stream is served off: where it holds the slot and a stream
    /// arrives, the stream takes the slot and the one-shot watches beside it.
    /// Left holding, the stream was served off a one-shot that is withdrawn
    /// the moment it completes, and heard nothing after that.
    ///
    /// Either way both are recorded as watching, because what is watching is
    /// what a reading is delivered to: recorded as neither, the caller that
    /// asked for the one-shot was sent nothing at all.
    fn take_or_follow(
        &mut self, instrument: InstrumentId, req_id: i64, series: &[u32], taken_on: u64,
        con_id: i64,
    ) -> Joined {
        // What this request asked for, written down as it becomes one of the
        // watchers rather than before. Written first, a withdrawal deciding
        // which series nobody asks for any more read a list belonging to a
        // request that was not watching anything yet, and either kept a series
        // for a caller that never arrived or withdrew one the arriving caller
        // had just asked for.
        if !series.is_empty() {
            self.series.insert(req_id, series.to_vec());
        }
        let held = self.holders.get(&instrument).copied();
        if !self.one_shot.contains(&req_id)
            && held.is_some_and(|existing| self.one_shot.contains(&existing))
        {
            self.holders.insert(instrument, req_id);
            self.taken_on.insert(instrument, taken_on);
            self.took_contract.insert(instrument, con_id);
            if let Some(displaced) = held {
                self.watches(instrument, displaced);
            }
            self.by_req.insert(req_id, instrument);
            return Joined::TookFromTheOneShot;
        }
        match held {
            Some(existing) if existing != req_id => {
                self.watches(instrument, req_id);
                self.by_req.insert(req_id, instrument);
                Joined::Watching
            }
            _ => {
                self.holders.insert(instrument, req_id);
                self.taken_on.insert(instrument, taken_on);
                self.took_contract.insert(instrument, con_id);
                // Under the same acquisition as the holder map, not after it.
                // Written by the caller once this returned, a slot given back
                // in between was forgotten while this request pointed at
                // nothing — and the mapping landed afterwards, naming a slot
                // whose next contract this caller never asked about.
                self.by_req.insert(req_id, instrument);
                Joined::Took
            }
        }
    }
}

/// What one request has been told, by ledger membership, figure and currency.
pub type AccountFiguresTold = HashMap<(bool, String, String), String>;

pub struct ClientCore {
    /// Whether this session refuses to send anything that changes a position.
    ///
    /// The reference API carries the same control. A research or reporting
    /// program gets the guarantee at the client rather than by discipline.
    /// Set once when the session opens.
    pub readonly: std::sync::atomic::AtomicBool,
    // reqId <-> InstrumentId mapping
    /// Which contract each quote request is on.
    pub req_to_instrument: Mutex<HashMap<i64, InstrumentId>>,
    /// Which registration a number is currently holding, counted.
    ///
    /// A number outlives the subscriptions made under it: a callback may
    /// withdraw what it was told about and ask for something else on the spot,
    /// and the new one can watch the very same contract — so neither the
    /// number nor what it watches tells the two apart. This does.
    registration_epoch: Mutex<HashMap<i64, u64>>,
    epochs: std::sync::atomic::AtomicU64,
    /// Which request owns each contract's quotes. One per contract:
    /// later callers follow it rather than opening a second.
    pub instrument_to_req: Mutex<HashMap<InstrumentId, i64>>,
    /// The other requests watching a contract that is already subscribed.
    ///
    /// One contract holds one subscription on the wire, and the same quote is
    /// handed to every caller that asked for it. Two parts of one program may
    /// watch the same contract.
    pub instrument_followers: Mutex<HashMap<InstrumentId, Vec<i64>>>,
    /// The engine slot each contract id was given.
    pub con_id_to_instrument: Mutex<HashMap<i64, InstrumentId>>,
    /// What each display group currently holds.
    ///
    /// A group is a way for several callers on one session to agree on a
    /// contract. Nothing about one crosses the wire, so they are kept here and
    /// served to callers from here.
    display_groups: Mutex<HashMap<i32, String>>,
    /// Which group each subscribing request follows.
    group_subscriptions: Mutex<HashMap<i64, i32>>,
    /// Group answers waiting to be delivered on the next dispatch, so a caller
    /// hears them where it hears everything else.
    pending_group_events: Mutex<Vec<GroupEvent>>,
    // Change detection for quote polling
    /// The most recent quote per contract, so a caller asking twice is
    /// answered the same way twice.
    pub last_quotes: Mutex<HashMap<InstrumentId, [i64; 16]>>,
    /// Requests that asked for a snapshot rather than a stream, and when each
    /// last heard something.
    ///
    /// A snapshot ends when the venue has finished sending it. There is no
    /// marker for that on this protocol, so it ends when the ticks stop:
    /// ending at the first one cancelled the subscription on whatever arrived
    /// first — often the previous close — and the bid and ask that the caller
    /// asked for never came.
    /// Snapshots being waited on: when each was asked for, and which of the
    /// kinds one is made of the venue has stated so far.
    pub snapshot_reqs: Mutex<HashMap<i64, SnapshotWait>>,
    /// The requests that asked for the venue's one-shot snapshot.
    ///
    /// One of those is a request of its own and not a stream: the venue
    /// answers it once, under a request type of its own, and it is withdrawn
    /// as soon as it completes. Nothing follows one — a stream that did was
    /// never sent, and when the one-shot was withdrawn the follower was
    /// promoted onto the one-shot's own row and heard no quotes at all.
    chargeable_snapshot_reqs: Mutex<std::collections::HashSet<i64>>,
    /// Which series each request asked for, by the venue's number for each.
    ///
    /// A subscription is shared here and its series are not: a caller joining
    /// a contract brings its own list, and what it brought goes with it when
    /// it withdraws. Without a record of who asked for what, the series a
    /// joiner added were served for as long as the subscription it joined
    /// outlived it — counted against the allowance, and asked for again by
    /// every rebuild after a reconnect.
    series_by_req: Mutex<HashMap<i64, Vec<u32>>>,
    /// When each slot was taken, in the order of what this client has asked
    /// for.
    ///
    /// Read against the number a slot was given back under. A slot the engine
    /// frees goes to the next contract that needs one, so a release read after
    /// that named a slot this client had just been given again: every record
    /// of the contract now on it was forgotten, the venue went on streaming
    /// it, and no request could be found to deliver it to or to withdraw it.
    slot_taken_on: Mutex<HashMap<InstrumentId, u64>>,
    /// The occupancy each slot is held under, as the engine's records have
    /// said it so far.
    ///
    /// Written where the records that name it are delivered, so it stands
    /// where they stand in the session's order. A quote, and every record
    /// queued under a slot, carries the occupancy it was written under; one
    /// that does not match this is the contract that left the slot, and is
    /// not delivered as the one that took it.
    slot_generation: Mutex<HashMap<InstrumentId, u64>>,
    /// And which contract the request that took each slot named.
    ///
    /// Written down as the slot is taken, because it is the caller's own and
    /// does not change. Looked up in the contract cache instead, the answer was
    /// whichever entry happened to point at that slot — an earlier contract's,
    /// where a release had not been read yet — and a withdrawal carrying it was
    /// refused as being about another contract.
    slot_took_contract: Mutex<HashMap<InstrumentId, i64>>,

    pub(crate) account_routes: Mutex<AccountRoutes>,

    // PnL subscription state
    /// Each profit request and the account whose figures it reports.
    pub pnl_req_id: Mutex<std::collections::BTreeMap<i64, String>>,
    /// Which contract each single-position profit request is on.
    pub pnl_single_reqs: Mutex<HashMap<i64, (String, i64)>>, // req_id → account, con_id
    /// The last running profit stated: daily, unrealised, realised.
    pub last_pnl: Mutex<HashMap<i64, [i64; 3]>>,
    // Per-req_id change detection for pnl_single: [pos, daily, unrealized, realized,
    // value] scaled.
    /// The same per position.
    pub last_pnl_single: Mutex<HashMap<i64, [i64; 5]>>,

    // Account summary subscription state (req_id, tags)
    /// The first summary subscription and the tags it asked for.
    pub account_summary_req: Mutex<Option<(i64, Vec<String>)>>,
    /// The venue serves two concurrent summary subscriptions.
    account_summary_other_req: Mutex<Option<(i64, Vec<String>)>>,
    /// When each summary last ran and what it stated, so only changed values
    /// are delivered at the venue's three-minute interval.
    last_account_summary: Mutex<HashMap<i64, SummaryPass>>,

    // News bulletin subscription
    /// Whether broadcast notices were asked for.
    pub bulletin_subscribed: AtomicBool,

    // Account updates subscription
    /// Whether the account's own figures were asked for.
    pub account_updates_subscribed: AtomicBool,
    /// What has already been delivered, by ledger membership, figure and
    /// currency, so each is delivered once and again when it changes.
    pub last_stated_account: Mutex<AccountFiguresTold>,
    /// The same record for the multi-account subscription, per request.
    ///
    /// Kept apart from the one above: the two are asked for and withdrawn
    /// separately, and sharing the record let whichever ran first take a
    /// figure's change and leave the other with nothing to report.
    ///
    /// And kept per request rather than once for all of them. Watchers are not
    /// in step: one opened a minute after another has been told nothing the
    /// first was told, and a single record cannot be true for both. Shared, a
    /// second ask marked every figure delivered for everyone and the watcher
    /// already standing was never told the move that ask overtook — and taking
    /// the record out of the ask instead only moved the fault, because the
    /// first dispatch then found every figure undelivered and said it all
    /// again to a caller that had just been given it.
    pub last_stated_account_multi: Mutex<HashMap<i64, AccountFiguresTold>>,
    /// The multi-account requests that asked for the ledger and net
    /// liquidation alone.
    pub ledger_only_multi: Mutex<std::collections::HashSet<i64>>,
    /// Whether the caller has been told the account is fully stated.
    pub account_end_sent: AtomicBool,
    /// Its positions as last stated.
    pub last_portfolio: Mutex<Option<Vec<PositionInfo>>>,

    // Execution replay store
    /// Fills held for a caller who asks for them again.
    pub executions: Mutex<ExecutionStore>,

    // Open order tracking
    /// Every order this client placed and the venue has not finished.
    pub open_orders: Mutex<HashMap<u64, TrackedOrder>>,

    /// Every number holding a book.
    ///
    /// Kept here rather than in the engine because the refusal has to reach
    /// the caller before anything is sent, and because both surfaces read it.
    pub depth_reqs: std::sync::Arc<Mutex<HashSet<i64>>>,

    /// Every number the venue has already worked an order under this session.
    ///
    /// An order that finishes leaves the book, so its number stops naming
    /// anything -- and a placement under it read as a new order rather than
    /// as the duplicate it is. The venue refuses a repeated number only while
    /// it is still working one, so a caller retrying after a fill was given a
    /// second live order instead of a refusal.
    pub spent_order_ids: Mutex<HashSet<u64>>,
    attached_orders: Mutex<attached_orders::AttachedState>,

    // Market data type callback tracking
    /// Which feed subscriptions default to.
    pub market_data_type: AtomicI32,
    /// Which feeds the session has turned on: frozen, delayed and
    /// delayed-frozen.
    market_data_feeds: AtomicI32,
    /// Which requests have already been told which feed they are on.
    pub mdt_sent: Mutex<HashMap<i64, i32>>,
    /// Requests already told the parameters of their market-data subscription.
    tick_req_params_sent: Mutex<HashSet<i64>>,
    /// The last option computation of each kind each request was sent,
    /// figure by figure.
    option_ticks_sent: Mutex<HashMap<(i64, crate::bridge::OptionTickKind), [f64; 8]>>,
    /// Each option's model tick as it stands, under the subscription it was
    /// built for: what a request joining the option is sent at once.
    models_as_they_stand: Mutex<HashMap<InstrumentId, (u64, crate::bridge::OptionTick)>>,
    /// The type sent with each instrument's subscription. Every watcher reads
    /// that feed, even when it asked for another type or takes over as holder.
    mdt_by_instrument: Mutex<HashMap<InstrumentId, i32>>,
    /// Which requests asked for their bar times as seconds since the epoch.
    ///
    /// The venue states a time in one form and the client formats it for the
    /// caller. A caller handed the other form is reading a date as a number or
    /// a number as a date. Only the request that asked is affected, so this is
    /// kept per request rather than for the session.
    /// What each historical request asked for: the form its bar times are
    /// wanted in, and the end and duration its range is derived from. The
    /// reply states neither, and the range stated beside the last bar is the
    /// request's own, so the request is what has to be kept.
    historical_asks: Mutex<HashMap<i64, HistoricalAsk>>,
    // Historical data keepUpToDate: req_ids that have completed initial batch.
    // Subsequent bars for these req_ids dispatch as historical_data_update.
    // Cleared when a request is made under the id again
    // `historical_request_is_new`.
    /// Which historical requests have finished their first batch, so
    /// a later bar under the same id is a continuation rather than a new answer.
    pub hist_initial_complete: Mutex<HashSet<u32>>,

    // News subscription state
    /// Every provider this account may read.
    pub news_providers: Mutex<String>,

    // Contract cache for enrichment
    /// What the venue has said about each contract, kept so a second
    /// request need not ask again.
    pub contract_cache: Mutex<HashMap<i64, ApiContract>>,
    /// Contracts the venue has named, under the description that was asked
    /// about rather than the one it answered with.
    ///
    /// An order may name its contract by description, and the venue only takes
    /// orders that name it by id, so the description has to be looked up. Asked
    /// again for every order, a program that places a hundred on one contract
    /// sends a hundred lookups for a name that has not changed since the first.
    named_by_description: Mutex<HashMap<String, ApiContract>>,
}

impl Default for ClientCore {
    fn default() -> Self {
        Self::new()
    }
}

/// Why neither question can be answered without the venue having spoken.
pub(crate) const OPTION_MODEL_UNSTATED: &str =
    "the venue has not stated its own model for this contract on this session. Ask for the \
     option's model first — a market-data subscription on the option carries it — and both \
     questions can then be answered against what it said";

/// Years between now and a stated expiry, as `yyyymmdd`.
pub(crate) fn years_to_expiry(expiry: &str) -> Option<f64> {
    let expiry_day = crate::protocol::datetime::day_number(expiry)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let today = now / 86_400;
    let days = expiry_day - today;
    (days > 0).then(|| days as f64 / 365.0)
}


/// The reference client's protocol level this client implements.
///
/// The newest of that client's `MIN_SERVER_VER_*` gates whose feature is
/// carried here. The number was never the venue's — it is the level of the
/// process a program is talking to, which in that client is its gateway and
/// here is this client — so every surface that answers the question answers
/// with this, and a program is told one thing about one client.
///
/// What it overstates is listed on `EClient::server_version`, which is the
/// call a program reads it through.
pub const PROTOCOL_LEVEL: i32 = 217;

/// A request a gateway reads a free-form option list on: its name for the
/// request, and the words its later check of `manual` puts in front of a bad
/// value. `manual` is the one key any such request takes, and the two option
/// calculations take no key at all.
#[derive(Debug)]
pub struct OptionList {
    /// The request as a gateway names it in a refusal.
    pub request: &'static str,
    /// What the later check of `manual` calls the request; `None` where the
    /// request takes no key, and so makes no later check.
    pub checked_as: Option<&'static str>,
}

/// An order's options.
pub const ORDER_OPTIONS: OptionList =
    OptionList { request: "PlaceOrder(3)", checked_as: Some("Order") };
/// A quote subscription's options.
pub const MKT_DATA_OPTIONS: OptionList =
    OptionList { request: "ReqMktData(1)", checked_as: Some("Market data") };
/// A book's options.
pub const MKT_DEPTH_OPTIONS: OptionList =
    OptionList { request: "ReqMktDepth(10)", checked_as: Some("Market data") };
/// Historical bars' options.
pub const CHART_OPTIONS: OptionList =
    OptionList { request: "ReqHistoricalData(20)", checked_as: Some("Historical data") };
/// A scan's options.
pub const SCANNER_OPTIONS: OptionList =
    OptionList { request: "ReqScannerSubscription(22)", checked_as: Some("Historical data") };
/// Real-time bars' options.
pub const REAL_TIME_BARS_OPTIONS: OptionList =
    OptionList { request: "ReqRealTimeBars(50)", checked_as: Some("Historical data") };
/// A news article's options.
pub const NEWS_ARTICLE_OPTIONS: OptionList =
    OptionList { request: "ReqNewsArticle(84)", checked_as: Some("Historical data") };
/// Historical news' options.
pub const HISTORICAL_NEWS_OPTIONS: OptionList =
    OptionList { request: "ReqHistoricalNews(86)", checked_as: Some("Historical data") };
/// Historical ticks' options.
pub const HISTORICAL_TICKS_OPTIONS: OptionList =
    OptionList { request: "ReqHistoricalTicks(96)", checked_as: Some("Market data") };
/// An implied-volatility calculation's options, of which it takes none.
pub const IMPL_VOL_OPTIONS: OptionList =
    OptionList { request: "ReqCalcImpliedVolatility(54)", checked_as: None };
/// An option-price calculation's options, of which it takes none.
pub const OPT_PRC_OPTIONS: OptionList =
    OptionList { request: "ReqCalcOptionPrice(55)", checked_as: None };

pub use crate::types::ExerciseStates;

/// What the session an order goes out on says about it, which a gateway reads
/// while it checks one: the account it logged in with, every account the login
/// holds, the ones its logon names as its own, whether it is an advisor's, and
/// the features the venue enabled at logon.
#[derive(Debug, Default, Clone)]
pub struct OrderSession {
    /// The account the session logged in with.
    pub account: String,
    /// Every account the login holds, those its family links to it included.
    pub accounts: Vec<String>,
    /// The accounts the logon names as the login's own.
    pub logon_accounts: Vec<String>,
    /// Whether the login is an advisor's.
    pub advisor: bool,
    /// The features the venue enabled at logon.
    pub features: Vec<String>,
}

/// Whether a login holds several accounts, as a gateway decides it: the venue
/// lets accounts be added to it as it runs, the first account its logon names
/// is an introducing broker's master, or its logon names more than one account
/// that is not a group. The accounts a family links to the login are not
/// counted. One rule for orders and for the account requests alike.
pub(crate) fn holds_several_accounts(logon_accounts: &[String], features: &[String]) -> bool {
    // A group's code has a `G` second or third.
    let group = |a: &str| a.len() > 3 && (a.as_bytes()[1] == b'G' || a.as_bytes()[2] == b'G');
    let master = |a: &str| {
        let a = a.as_bytes();
        a.len() > 2 && (a[0] == b'I' || (a[0] == b'D' && a[1] == b'I'))
    };
    features.iter().any(|f| f == "DYNACCTADD")
        || logon_accounts.first().is_some_and(|a| master(a) && !group(a))
        || logon_accounts.iter().filter(|a| !group(a)).count() > 1
}

impl OrderSession {
    /// A login holding one account, with nothing enabled.
    pub fn single(account: &str) -> Self {
        Self {
            account: account.to_string(),
            accounts: vec![account.to_string()],
            logon_accounts: vec![account.to_string()],
            ..Self::default()
        }
    }

    /// Whether the venue enabled a feature at logon.
    pub fn enables(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    /// Whether the login holds several accounts, as a gateway decides it
    /// ([`holds_several_accounts`]).
    pub fn holds_several_accounts(&self) -> bool {
        holds_several_accounts(&self.logon_accounts, &self.features)
    }

    /// Whether the login holds this account.
    pub fn holds(&self, account: &str) -> bool {
        account == self.account || self.accounts.iter().any(|a| a == account)
    }
}

impl ClientCore {
    /// An empty one.
    pub fn new() -> Self {
        Self {
            readonly: std::sync::atomic::AtomicBool::new(false),
            req_to_instrument: Mutex::new(HashMap::new()),
            registration_epoch: Mutex::new(HashMap::new()),
            epochs: std::sync::atomic::AtomicU64::new(0),
            instrument_to_req: Mutex::new(HashMap::new()),
            instrument_followers: Mutex::new(HashMap::new()),
            con_id_to_instrument: Mutex::new(HashMap::new()),
            display_groups: Mutex::new(HashMap::new()),
            group_subscriptions: Mutex::new(HashMap::new()),
            pending_group_events: Mutex::new(Vec::new()),
            last_quotes: Mutex::new(HashMap::new()),
            snapshot_reqs: Mutex::new(HashMap::new()),
            chargeable_snapshot_reqs: Mutex::new(std::collections::HashSet::new()),
            series_by_req: Mutex::new(HashMap::new()),
            slot_taken_on: Mutex::new(HashMap::new()),
            slot_generation: Mutex::new(HashMap::new()),
            slot_took_contract: Mutex::new(HashMap::new()),
            account_routes: Mutex::new(AccountRoutes::default()),
            pnl_req_id: Mutex::new(std::collections::BTreeMap::new()),
            pnl_single_reqs: Mutex::new(HashMap::new()),
            last_pnl: Mutex::new(HashMap::new()),
            last_pnl_single: Mutex::new(HashMap::new()),
            account_summary_req: Mutex::new(None),
            account_summary_other_req: Mutex::new(None),
            last_account_summary: Mutex::new(HashMap::new()),
            bulletin_subscribed: AtomicBool::new(false),
            account_updates_subscribed: AtomicBool::new(false),
            last_stated_account: Mutex::new(HashMap::new()),
            last_stated_account_multi: Mutex::new(HashMap::new()),
            ledger_only_multi: Mutex::new(std::collections::HashSet::new()),
            account_end_sent: AtomicBool::new(false),
            last_portfolio: Mutex::new(None),
            executions: Mutex::new(ExecutionStore::default()),
            open_orders: Mutex::new(HashMap::new()),
            spent_order_ids: Mutex::new(HashSet::new()),
            attached_orders: Mutex::new(attached_orders::AttachedState::default()),
            depth_reqs: std::sync::Arc::new(Mutex::new(HashSet::new())),
            market_data_type: AtomicI32::new(1),
            market_data_feeds: AtomicI32::new(0),
            mdt_sent: Mutex::new(HashMap::new()),
            tick_req_params_sent: Mutex::new(HashSet::new()),
            option_ticks_sent: Mutex::new(HashMap::new()),
            models_as_they_stand: Mutex::new(HashMap::new()),
            mdt_by_instrument: Mutex::new(HashMap::new()),
            historical_asks: Mutex::new(HashMap::new()),
            hist_initial_complete: Mutex::new(HashSet::new()),
            // Empty until something states them. Which providers an account
            // may read is the venue's answer, given at logon; a pair of codes
            // standing in for it asked for news from providers the account
            // may not be entitled to and left out the ones it is.
            news_providers: Mutex::new(String::new()),
            contract_cache: Mutex::new(HashMap::new()),
            named_by_description: Mutex::new(HashMap::new()),
        }
    }

    /// Clear all per-session state so the owning client can reconnect.
    /// Refuse this session anything that changes a position.
    pub fn set_readonly(&self, on: bool) {
        self.readonly.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether this client refuses anything that would trade.
    pub fn is_readonly(&self) -> bool {
        self.readonly.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The refusal a read-only session gives, naming the call that was made.
    ///
    /// Loud rather than silent: a program that believes it placed an order and
    /// did not is worse off than one that stops.
    pub fn refuse_if_readonly(&self, what: &str) -> Result<(), String> {
        if self.is_readonly() {
            return Err(format!("this session is read-only; {what} was not sent"));
        }
        Ok(())
    }

    /// Forget everything this session held, so the next one starts clean.
    pub fn reset(&self) {
        self.req_to_instrument.lock().unwrap().clear();
        self.instrument_to_req.lock().unwrap().clear();
        self.instrument_followers.lock().unwrap().clear();
        self.con_id_to_instrument.lock().unwrap().clear();
        self.last_quotes.lock().unwrap().clear();
        self.snapshot_reqs.lock().unwrap().clear();
        // And which of them were the venue's one-shot. Kept, a number reused
        // for an ordinary stream on the next session read as a one-shot, and
        // the callers on that contract were served as though it were one.
        self.chargeable_snapshot_reqs.lock().unwrap().clear();
        // And what each of them had asked for. Kept, the next session's
        // request under the same number is read as asking for the series the
        // last one named.
        self.series_by_req.lock().unwrap().clear();
        self.slot_taken_on.lock().unwrap().clear();
        self.slot_generation.lock().unwrap().clear();
        self.slot_took_contract.lock().unwrap().clear();
        // And which subscription each number was holding. Kept, the next
        // session's first withdrawal under a number reads a figure from the
        // session before it.
        self.registration_epoch.lock().unwrap().clear();
        *self.account_routes.lock().unwrap() = AccountRoutes::default();
        self.pnl_req_id.lock().unwrap().clear();
        self.pnl_single_reqs.lock().unwrap().clear();
        self.last_pnl.lock().unwrap().clear();
        self.last_pnl_single.lock().unwrap().clear();
        *self.account_summary_req.lock().unwrap() = None;
        *self.account_summary_other_req.lock().unwrap() = None;
        self.last_account_summary.lock().unwrap().clear();
        self.bulletin_subscribed.store(false, Ordering::Relaxed);
        self.account_updates_subscribed.store(false, Ordering::Relaxed);
        self.last_stated_account.lock().unwrap().clear();
        self.last_stated_account_multi.lock().unwrap().clear();
        self.ledger_only_multi.lock().unwrap().clear();
        self.account_end_sent.store(false, Ordering::Release);
        *self.last_portfolio.lock().unwrap() = None;
        *self.executions.lock().unwrap() = ExecutionStore::default();
        self.open_orders.lock().unwrap().clear();
        // A new session draws its numbers from the venue again, so what the
        // last one spent says nothing about this one.
        self.spent_order_ids.lock().unwrap().clear();
        self.attached_orders.lock().unwrap().reset();
        self.depth_reqs.lock().unwrap().clear();
        self.market_data_type.store(1, Ordering::Relaxed);
        self.market_data_feeds.store(0, Ordering::Relaxed);
        self.mdt_sent.lock().unwrap().clear();
        self.tick_req_params_sent.lock().unwrap().clear();
        self.option_ticks_sent.lock().unwrap().clear();
        self.models_as_they_stand.lock().unwrap().clear();
        self.mdt_by_instrument.lock().unwrap().clear();
        self.historical_asks.lock().unwrap().clear();
        self.hist_initial_complete.lock().unwrap().clear();
        self.news_providers.lock().unwrap().clear();
        self.contract_cache.lock().unwrap().clear();
        // What the venue named for a description belongs to the session that
        // asked. Kept across a reconnect — or a login as somebody else — the
        // next order goes out under an id this session was never given.
        self.named_by_description.lock().unwrap().clear();
        // A group this session joined, and what it was told about it. Kept,
        // the next session is called back about a group under a request id it
        // never subscribed with.
        self.display_groups.lock().unwrap().clear();
        self.group_subscriptions.lock().unwrap().clear();
        self.pending_group_events.lock().unwrap().clear();
    }

    // ── Registration helpers ──

    /// Which slot this contract holds, as far as this client knows.
    ///
    /// `0` means the contract carries no conId and answers for no one: the
    /// engine resolves those by descriptor, so only it can say which slot
    /// they got.
    ///
    /// Takes the session's state because it drops what the engine has given
    /// back before it answers. Every reader needs that and one of them will
    /// always forget, so the drop happens here rather than at each of them: a
    /// freed slot goes to the next contract that needs one, and an answer from
    /// here after that names a contract the caller never asked about.
    pub(crate) fn cached_instrument(&self, shared: &SharedState, con_id: i64) -> Option<InstrumentId> {
        self.forget_released_slots(shared);
        if con_id == 0 {
            return None;
        }
        self.con_id_to_instrument.lock().unwrap().get(&con_id).copied()
    }

    /// Cache the engine's answer for later lookups. Caching it under `0` would
    /// point every conId-less contract at the first one's slot.
    fn cache_instrument(&self, con_id: i64, instrument: InstrumentId) {
        if con_id != 0 {
            self.con_id_to_instrument.lock().unwrap().insert(con_id, instrument);
        }
    }

    /// Whether somebody already holds this contract, in which case this
    /// request watches theirs.
    ///
    /// Asks only. A contract nobody holds is not taken here: this runs as a
    /// question about a cached contract, and a subscription that goes on to
    /// fail would leave a holder recorded for a request that never started —
    /// which nothing then cancels. [`take_or_follow`](Self::take_or_follow) is
    /// where it is taken.
    pub(crate) fn follows_existing_subscription(
        &self, instrument: InstrumentId, req_id: i64, series: &[u32],
    ) -> bool {
        let mut own = self.ownership();
        let held = own.holders.get(&instrument).copied();
        match held {
            // Nothing follows the venue's chargeable one-shot. It is answered
            // once and withdrawn as soon as it completes, so a stream that
            // followed one was never sent — and when the one-shot went, the
            // follower was promoted onto its row and heard no quotes at all.
            Some(existing) if existing != req_id && own.one_shot.contains(&existing) => false,
            Some(existing) if existing != req_id => {
                own.watches(instrument, req_id);
                own.by_req.insert(req_id, instrument);
                // With the join, not before it. See `take_or_follow`.
                if !series.is_empty() {
                    own.series.insert(req_id, series.to_vec());
                }
                drop(own);
                self.stamp_registration(req_id);
                true
            }
            _ => false,
        }
    }

    /// The maps that say who is watching what, taken together.
    ///
    /// Four maps answer one question between them, and every decision about a
    /// subscription reads some of them and writes others. Taken one at a time,
    /// two decisions interleave inside one answer: a withdrawal that read the
    /// watchers before it took the holder map took a subscription down under a
    /// request that had just joined it; a request put back on a new slot was
    /// put back after its caller had withdrawn it; and a one-shot that gave up
    /// its kind before it gave up the slot handed its finite burst to a stream
    /// that was never sent. So they are taken together, always in this order,
    /// and nothing between here and the end of a decision takes any of them
    /// again.
    fn ownership(&self) -> Ownership<'_> {
        Ownership {
            holders: self.instrument_to_req.lock().unwrap(),
            following: self.instrument_followers.lock().unwrap(),
            by_req: self.req_to_instrument.lock().unwrap(),
            one_shot: self.chargeable_snapshot_reqs.lock().unwrap(),
            series: self.series_by_req.lock().unwrap(),
            taken_on: self.slot_taken_on.lock().unwrap(),
            took_contract: self.slot_took_contract.lock().unwrap(),
            epoch: self.registration_epoch.lock().unwrap(),
        }
    }

    /// Hold this contract, or follow whoever took it first.
    ///
    /// `instrument_to_req` maps one request per instrument: a second holder
    /// would clobber the first's reverse mapping and orphan it silently — no
    /// ticks, no error. Deciding and taking happen under one acquisition, so
    /// two callers subscribing the same unheld contract cannot both take it
    /// and leave the loser cancelling the winner's feed.
    ///
    /// Answers whether this request ended up a follower.
    pub(crate) fn take_or_follow(
        &self, instrument: InstrumentId, req_id: i64, series: &[u32], asked_on: u64, con_id: i64,
    ) -> bool {
        let joined =
            self.ownership().take_or_follow(instrument, req_id, series, asked_on, con_id);
        // The request that takes a slot nobody held is written down by whoever
        // asked for it, once the venue has answered; the other two are already
        // watching something and are stamped here.
        if joined != Joined::Took {
            self.stamp_registration(req_id);
        }
        joined == Joined::Watching
    }


    /// A gateway states these parameters once per request. The bid/ask and
    /// last subscriptions are acknowledged separately, and a follower can
    /// join while the first acknowledgement is still waiting for delivery.
    ///
    /// Only a request still watching is told. Marked after it was withdrawn,
    /// a number reused for a new request would never be told that request's
    /// parameters. Read under the map a withdrawal clears, so a withdrawal
    /// lands wholly before or wholly after.
    pub fn should_send_tick_req_params(&self, req_id: i64) -> bool {
        let watching = self.req_to_instrument.lock().unwrap();
        watching.contains_key(&req_id) && self.tick_req_params_sent.lock().unwrap().insert(req_id)
    }


    /// Forget everyone recorded as watching a slot the engine has taken back.
    ///
    /// The slot goes to the next contract that needs one, and the requests
    /// that named it went on naming it — so the next contract found one of
    /// them already holding its slot: it arrived as that request's follower,
    /// and its quotes went to a caller whose own subscription was over.
    ///
    /// Called only from [`ClientCore::forget_released_slots`], because the
    /// engine giving the slot back is the one thing that means this. A
    /// subscription can be refused and stand — one acknowledged with an
    /// increment prices cannot be counted in is refused to its caller and
    /// keeps its slot — and forgetting on the refusal would strand it.
    fn forget_watchers_of(&self, instrument: InstrumentId, released_at: u64) -> bool {
        // One acquisition for the lot. See `ownership`.
        {
            let mut own = self.ownership();
            // Not a slot this client has since been given again. The release
            // names the occupancy that ended, so what is compared is which
            // occupancy this is and not which of two decisions came first.
            // Where the release names none — a slot given back for a reason
            // that never reached a subscription — there is nothing to hold on
            // to and the records go.
            if own.taken_on.get(&instrument).is_some_and(|taken| *taken != released_at) {
                return false;
            }
            own.taken_on.remove(&instrument);
            let held = own.holders.remove(&instrument);
            let watchers = own.following.remove(&instrument).unwrap_or_default();
            let watching: Vec<i64> = held.into_iter().chain(watchers).collect();
            for req_id in &watching {
                // Only where it still points here. A request that has since
                // been pointed somewhere else is watching that, not this.
                if own.by_req.get(req_id) == Some(&instrument) {
                    own.by_req.remove(req_id);
                }
                own.one_shot.remove(req_id);
                own.series.remove(req_id);
                own.epoch.remove(req_id);
            }
            own.took_contract.remove(&instrument);
            // And everything else filed under the slot or under the numbers
            // that were watching it, under this same acquisition. Cleared
            // after it was released, a registration that took the slot in
            // between had its own mode, its own snapshot and its own cache
            // entry removed by a release that had already decided it did not
            // cover them — and a snapshot so removed never ended and was never
            // withdrawn.
            self.mdt_by_instrument.lock().unwrap().remove(&instrument);
            self.last_quotes.lock().unwrap().remove(&instrument);
            self.con_id_to_instrument.lock().unwrap().retain(|_, held| *held != instrument);
            for req_id in &watching {
                self.mdt_sent.lock().unwrap().remove(req_id);
                self.tick_req_params_sent.lock().unwrap().remove(req_id);
                self.option_ticks_sent.lock().unwrap().retain(|(sent_to, _), _| sent_to != req_id);
                // The slot going back ends the request, and a number that
                // outlives its request with its marks still standing is read as
                // the request it was: reused for an ordinary stream it was
                // withdrawn as a snapshot the moment it had both sides of a
                // quote, or read as the venue's one-shot by every later join,
                // or counted as still asking for a series its caller has no
                // subscription to hear.
                self.snapshot_reqs.lock().unwrap().remove(req_id);
            }
        };
        true
    }

    /// Every request watching a contract, the one holding the subscription
    /// first.
    ///
    /// Read under one acquisition, because the holder and the watchers are one
    /// answer. Read separately, a withdrawal running between the two left a
    /// list that never existed: the request that had just handed the
    /// subscription on was told what the venue said and the request that had
    /// just taken it was not.
    pub fn watchers_of(&self, instrument: InstrumentId) -> Vec<i64> {
        let own = self.ownership();
        own.holders
            .get(&instrument)
            .copied()
            .into_iter()
            .chain(own.following.get(&instrument).cloned().unwrap_or_default())
            .collect()
    }

    /// Every other request watching a contract, so one quote reaches them all.
    pub fn followers_of(&self, instrument: InstrumentId) -> Vec<i64> {
        self.instrument_followers
            .lock()
            .unwrap()
            .get(&instrument)
            .cloned()
            .unwrap_or_default()
    }

    /// Forget the slots the engine has given back.
    ///
    /// The cache below answers "which slot does this contract hold" without
    /// asking the engine, which is what keeps a placement off a round trip. A
    /// slot the engine has freed goes to the next contract that needs one, so
    /// an answer from here after that names another contract altogether: the
    /// order was recorded against the new occupant and its fill moved that
    /// contract's position.
    pub fn forget_released_slots(&self, shared: &SharedState) {
        let released = shared.market.take_released_slots();
        if released.is_empty() {
            return;
        }
        // Everything that named the slot. The cache alone was forgotten, so
        // the requests that had been watching it were still recorded against
        // it when the next contract took it.
        //
        // Each release is read against when this client last took that slot: a
        // slot given back goes to the next contract that needs one, and a
        // release read after that named a slot this client had just been given
        // again. Forgotten then, the contract now on it was streaming with no
        // request able to hear it or withdraw it.
        for (slot, released_at) in released {
            self.forget_watchers_of(slot, released_at);
        }
    }

    /// A request is being made under this id, so whatever a request under it
    /// finished before is over.
    ///
    /// Bars answering a fresh request were delivered as though they continued
    /// the last one — as updates, with no completion — because the id had
    /// been marked finished and nothing unmarked it. A caller looping over
    /// contracts under one id was answered once and never again.
    pub fn historical_request_is_new(&self, req_id: u32) {
        self.hist_initial_complete.lock().unwrap().remove(&req_id);
    }

    // ── Subscription management ──

    /// Ask the engine for a market-data subscription.
    ///
    /// What needs no venue is checked here and the request is handed over.
    /// The engine registers the contract, serves the request off the
    /// subscription the contract already has or asks for one, and says in the
    /// session's order which slot it is served on, or why it is not — so a
    /// number already watching something, and a contract the venue will not
    /// serve, are refused there. Nothing waits here. `spread_scan` rides the
    /// request where it is a spread scan, and `calculation` where it is opened
    /// to bring a calculation its model.
    pub fn register_mkt_data(
        &self,
        shared: &SharedState,
        control_tx: &Sender<ControlCommand>,
        req_id: i64,
        con_id: i64,
        symbol: &str,
        exchange: &str,
        sec_type: &str,
        currency: &str,
        filters: &crate::types::SecDefFilters,
        snapshot: bool,
        regulatory_snapshot: bool,
        generic_tick_list: &str,
        mode_9887: i32,
        spread_scan: Option<String>,
        calculation: Option<Box<crate::types::Calculation>>,
        delayed_allowed: bool,
    ) -> Result<(), Refusal> {
        Self::validate_contract_expiry(&filters.last_trade_date_or_contract_month)?;
        // A quote feed the engine has given up on serves nothing more this
        // session: there is no connection to write the request to and no
        // reconnect coming to replay it, and a caller told it had a
        // subscription waited out the session for a first tick.
        if let Some(why) = shared.market.market_data_over() {
            return Err(Refusal::not_connected(format!(
                "market data is unavailable for the rest of this session: {why}",
            )));
        }
        // The chargeable snapshot is one burst by construction, so it ends the
        // way an ordinary snapshot does and the caller hears the same end.
        let snapshot = snapshot || regulatory_snapshot;
        // News subscription if generic_tick_list names 292, bare or with the
        // providers to ask. The whole entry, not its last three characters:
        // "1292" is not 292, and matching on a suffix subscribes to news the
        // caller did not ask for.
        let asked = parse_generic_tick_list(generic_tick_list);
        // Each remaining entry is asked for. The number a caller states is the
        // venue's own number for the series, so there is nothing to translate:
        // it goes out as a subscription of its own under that number, the way
        // the option model, the trading status and the venue map already do.
        //
        // An entry that is not a number is not one of the venue's series, and
        // saying so is better than sending it and having the whole request
        // refused for the sake of one bad word in the list.
        if !asked.unread.is_empty() {
            log::warn!(
                "the generic tick list named {}, which is not a number the venue \
                 knows a series by, so nothing was asked for it",
                asked.unread.join(", "),
            );
        }
        // The providers the entry itself named, else those a caller has named
        // for the session, else what the logon said this account may read. The
        // venue separates codes with a star.
        let news = asked.news.then(|| {
            let named = self.news_providers.lock().unwrap().clone();
            if !asked.news_providers.is_empty() {
                asked.news_providers.clone()
            } else if named.is_empty() {
                shared.reference.news_providers()
                    .iter()
                    .map(|p| p.code.as_str())
                    .collect::<Vec<_>>()
                    .join("*")
            } else {
                named
            }
        });
        let delayed_mode = (delayed_allowed && matches!(mode_9887, 1 | 3) && !regulatory_snapshot)
            .then_some(mode_9887);
        let mode_9887 = if delayed_mode.is_some() { 0 } else { mode_9887 };
        // The frozen feeds the session has on, for a request that takes the
        // session's types: a gateway watches the market's status for it.
        let feeds = if delayed_allowed && !regulatory_snapshot {
            self.market_data_feeds.load(Ordering::Relaxed)
        } else {
            0
        };
        shared.admit(control_tx, ControlCommand::Subscribe {
            req_id,
            contract: ContractRef {
                con_id, symbol: symbol.to_string(), exchange: exchange.to_string(),
                sec_type: sec_type.to_string(), currency: currency.to_string(),
                last_trade_date: filters.last_trade_date_or_contract_month.clone(),
                strike: filters.strike, right: filters.right.clone(),
                multiplier: filters.multiplier.clone(),
            },
            filters: filters.clone(),
            mode_9887,
            delayed_mode,
            frozen: feeds & FEED_FROZEN != 0,
            delayed_frozen: feeds & FEED_DELAYED_FROZEN != 0,
            regulatory_snapshot,
            snapshot,
            generic_ticks: asked.series,
            news,
            spread_scan,
            calculation,
        })
    }

    /// Write down a market-data request the engine has taken, where its record
    /// stands in the session's order: which slot it is served on, and whether
    /// it holds the subscription there or watches the one somebody else holds.
    ///
    /// Written here rather than at the call, because the engine is the first
    /// to know which slot a contract named by symbol holds, and a request
    /// withdrawn before this is read is withdrawn in the same order.
    ///
    /// A request that starts on an option the model already works out is owed
    /// the model tick as it stands: returned, with the number it goes out
    /// under, for the caller to send that request alone.
    pub fn note_mkt_data_taken(
        &self, _shared: &SharedState, taken: &crate::bridge::MarketDataTaken,
    ) -> Option<(i32, i64, crate::bridge::OptionTick)> {
        let crate::bridge::MarketDataTaken {
            req_id, slot, generation, con_id, ref series, snapshot, asked_at, one_shot, data_type, marked,
        } = *taken;
        self.cache_instrument(con_id, slot);
        // Written down before it can be followed: what it asked for decides
        // whether it may be followed at all.
        if one_shot {
            self.chargeable_snapshot_reqs.lock().unwrap().insert(req_id);
        }
        if snapshot {
            self.snapshot_reqs.lock().unwrap().insert(req_id, SnapshotWait {
                asked_at, ..SnapshotWait::new(slot, marked)
            });
        }
        // A contract already being watched needs no second subscription: this
        // request watches the one that is up, and hears the same quotes under
        // its own number.
        //
        // Except a chargeable snapshot, which is a request of its own and not
        // a share of somebody's stream. Following one instead sends nothing,
        // bills nothing, and lets an account with no entitlement hear an end
        // it was never refused — off a stream it did not ask for.
        if !one_shot && self.follows_existing_subscription(slot, req_id, series) {
            self.last_quotes.lock().unwrap().remove(&slot);
            // A request that starts on an option the model already works out
            // is sent the model tick as it stands, as a gateway sends it when
            // a request starts, rather than at the model's next change. Sent
            // to it alone and now: queued again behind the model's newer
            // ticks, it went to every watcher after them.
            let (built_for, tick) = self.models_as_they_stand.lock().unwrap().get(&slot).copied()?;
            if built_for != self.generation_held(slot) {
                return None;
            }
            let stated = tick.figures.iter().any(|figure| *figure != f64::MAX);
            let fresh = self.option_ticks_sent.lock().unwrap()
                .insert((req_id, crate::bridge::OptionTickKind::Model), tick.figures) != Some(tick.figures);
            let owed = match self.snapshot_reqs.lock().unwrap().get_mut(&req_id) {
                Some(wait) => fresh && owed_to_snapshot(wait, tick.kind, &tick.figures, self.feed_of(slot)),
                None => fresh && stated,
            };
            if !owed {
                return None;
            }
            return Some((option_tick_type(tick.kind, self.feed_is_delayed(slot)), req_id, tick));
        }
        let _ = self.take_or_follow(slot, req_id, series, generation, con_id);
        self.stamp_registration(req_id);
        // A request beside it may already hold the subscription. Its mode
        // still describes the feed everyone on this instrument receives.
        self.mdt_by_instrument.lock().unwrap().entry(slot).or_insert(data_type);
        None
    }

    /// Take a request number for a book, or say it already holds one.
    ///
    /// Depth is routed by records the engine keeps, so neither surface could
    /// see that a number already held a book: two contracts' rows arrived
    /// interleaved under one number with nothing to tell them apart, the
    /// withdrawal named only the later contract and left the earlier one being
    /// served, and a reconnect brought back one book where there had been two.
    pub fn hold_the_book(&self, req_id: i64, shared: &SharedState) -> Result<(), Refusal> {
        self.forget_books_let_go(shared);
        if !self.depth_reqs.lock().unwrap().insert(req_id) {
            return Err(Refusal::stated(
                DUPLICATE_TICKER_ID,
                format!(
                    "request {req_id} is already holding a book: withdraw it before \
                     asking for another under the same number",
                ),
            ));
        }
        Ok(())
    }

    /// Check the contract month or date before requesting the contract.
    pub fn validate_contract_expiry(expiry: &str) -> Result<(), Refusal> {
        if expiry.is_empty() || expiry.eq_ignore_ascii_case("NOEXP") {
            return Ok(());
        }
        let valid = matches!(expiry.len(), 6 | 8)
            && expiry.bytes().all(|b| b.is_ascii_digit())
            && expiry[..4].parse::<i16>().ok().is_some_and(|year| {
                if !(1978..=3000).contains(&year) { return false; }
                let month = expiry[4..6].parse::<i8>().unwrap();
                let day = if expiry.len() == 8 { expiry[6..8].parse().unwrap() } else { 1 };
                jiff::civil::Date::new(year, month, day).is_ok()
            });
        if valid {
            Ok(())
        } else {
            Err(Refusal::stated(10372,
                "lastTradeDateOrContractMonth: The date entered is invalid. The correct format is yyyyMM for a contract month or yyyyMMdd for a date. E.g.: 202607 or 20260724.",
            ))
        }
    }

    /// What a gateway refuses in a request for a book before it looks the
    /// contract up: no exchange named, a combination, or no rows.
    ///
    /// Each is refused as a gateway refuses it, with its reason. An empty
    /// exchange is not read as the smart destination: a gateway asks the
    /// caller to name one.
    pub fn validate_depth_request(
        exchange: &str, sec_type: &str, num_rows: i32, expiry: &str,
    ) -> Result<(), Refusal> {
        if exchange.trim().is_empty() {
            return Err(Refusal::validation("Please enter exchange."));
        }
        Self::validate_contract_expiry(expiry)?;
        if sec_type.trim().eq_ignore_ascii_case("BAG") {
            return Err(Refusal::validation("Market depth does not support combos."));
        }
        if num_rows <= 0 {
            return Err(Refusal::validation(
                "Market depth rows requested must be greater than zero.",
            ));
        }
        Ok(())
    }

    /// Give the number back, or say it was holding no book.
    pub fn release_the_book(&self, req_id: i64, shared: &SharedState) -> Result<(), Refusal> {
        self.forget_books_let_go(shared);
        if !self.depth_reqs.lock().unwrap().remove(&req_id) {
            return Err(Refusal::stated(
                NO_SUCH_BOOK,
                format!("no book is held under request {req_id}"),
            ));
        }
        Ok(())
    }

    /// Forget the numbers whose book the engine let go before it was asked
    /// for: the venue named no single contract for it.
    fn forget_books_let_go(&self, shared: &SharedState) {
        let let_go = shared.market.take_books_let_go();
        if !let_go.is_empty() {
            let mut held = self.depth_reqs.lock().unwrap();
            for req_id in let_go {
                held.remove(&i64::from(req_id));
            }
        }
    }

    /// Whether this number is watching a contract at all.
    ///
    /// Read before a withdrawal, which cannot tell "nothing was held" from
    /// "a follower stopped following" by what it returns -- both are nothing
    /// to send.
    pub fn holds_mkt_data(&self, req_id: i64) -> bool {
        self.req_to_instrument.lock().unwrap().contains_key(&req_id)
    }

    /// Which contract's slot a number is watching, if it is watching one.
    pub fn watching(&self, req_id: i64) -> Option<InstrumentId> {
        self.req_to_instrument.lock().unwrap().get(&req_id).copied()
    }

    /// Say this number now holds a subscription of its own, distinct from any
    /// it held before.
    fn stamp_registration(&self, req_id: i64) {
        let n = self.in_order();
        self.registration_epoch.lock().unwrap().insert(req_id, n);
    }

    /// The next number in the order of everything this client asks for.
    ///
    /// One rising count, taken as a decision is made and carried on the
    /// command that acts on it. The engine keeps the number a subscription
    /// began under and reads it again on a withdrawal: a withdrawal decided
    /// before that subscription began is not about it.
    pub(crate) fn in_order(&self) -> u64 {
        // From one, because zero is what a command says when it names no
        // occupancy at all — a withdrawal carried through a move, or a session
        // closing. Counted from zero, the first subscription of every session
        // was the one occupancy that could not be named.
        self.epochs.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }

    /// Which subscription a number is holding, as a figure that changes every
    /// time it takes a new one.
    ///
    /// Read to tell one subscription under a number from the next. Neither the
    /// number nor the contract can do that: a callback is free to withdraw
    /// what it was just told about and ask for the same contract again, and
    /// what runs after the callback must not then act on the number alone.
    pub fn registration_of(&self, req_id: i64) -> Option<u64> {
        self.registration_epoch.lock().unwrap().get(&req_id).copied()
    }

    /// Forget a market-data request, where its withdrawal stands in the
    /// session's order.
    ///
    /// A request watching somebody else's subscription stops watching it, and
    /// the subscription stays up for the rest. A request that held it hands
    /// it to the next one watching rather than taking the quotes away from
    /// them. What goes to the venue is the engine's to decide: it took the
    /// request before its withdrawal, and knows who else is on the contract.
    pub fn unregister_mkt_data(&self, req_id: i64) {
        // Whatever this id was waiting to finish, it is not waiting any
        // more. Left behind, the same id handed out again for an ordinary
        // stream reads as a snapshot and is withdrawn as soon as it has both
        // sides of a quote.
        self.snapshot_reqs.lock().unwrap().remove(&req_id);
        {
            // Which contract this number was watching, whether it held the
            // subscription, who takes it over and whether it was the venue's
            // one-shot are one question, answered under one acquisition.
            let mut own = self.ownership();
            own.one_shot.remove(&req_id);
            own.epoch.remove(&req_id);
            own.series.remove(&req_id);
            self.tick_req_params_sent.lock().unwrap().remove(&req_id);
            self.option_ticks_sent.lock().unwrap().retain(|(sent_to, _), _| *sent_to != req_id);
            if let Some(instrument) = own.by_req.remove(&req_id) {
                let mut nobody_left = true;
                if let Some(watchers) = own.following.get_mut(&instrument) {
                    let was_following = watchers.contains(&req_id);
                    watchers.retain(|&id| id != req_id);
                    let next = if was_following { None } else { watchers.first().copied() };
                    if let Some(next) = next {
                        watchers.retain(|&id| id != next);
                    }
                    if watchers.is_empty() {
                        own.following.remove(&instrument);
                    }
                    if was_following || next.is_some() {
                        if let Some(next) = next {
                            own.holders.insert(instrument, next);
                        }
                        nobody_left = false;
                    }
                }
                if nobody_left {
                    own.holders.remove(&instrument);
                    own.taken_on.remove(&instrument);
                    own.took_contract.remove(&instrument);
                    // And everything else the slot leaves behind, under the
                    // same acquisition that decided it goes.
                    self.last_quotes.lock().unwrap().remove(&instrument);
                    self.mdt_by_instrument.lock().unwrap().remove(&instrument);
                    self.con_id_to_instrument.lock().unwrap()
                        .retain(|_, iid| *iid != instrument);
                }
            }
        }
        self.mdt_sent.lock().unwrap().remove(&req_id);
    }

    /// A caller is withdrawing a request: whatever it was registered as is
    /// over from here, so work decided against that registration — a
    /// snapshot's own withdrawal once its callback has run — is not done
    /// against whatever the caller asks for next under the same number.
    pub fn withdrawing(&self, req_id: i64) {
        self.registration_epoch.lock().unwrap().remove(&req_id);
    }

    /// Drop the client-side conId cache entries for an instrument id. The
    /// engine may reclaim and reuse the slot after an unsubscribe;
    /// a stale cache entry would silently point the old conId at whatever
    /// contract inherits the id. A later request for that conId simply
    /// re-registers.
    pub fn forget_instrument(&self, instrument: InstrumentId) {
        self.con_id_to_instrument.lock().unwrap().retain(|_, iid| *iid != instrument);
    }

    /// Name the providers to ask for news from, overriding what the logon
    /// said this account may read. An empty string returns to that.
    pub fn set_news_providers(&self, providers: &str) {
        *self.news_providers.lock().unwrap() = providers.to_string();
    }

    // ── Contract cache ──

    /// How a contract was asked about, as one string.
    ///
    /// Built from what the caller wrote, not from what the venue answered: a
    /// caller who says `SMART` gets `SMART` back on the next order, and looking
    /// the answer up under the exchange the venue routed it to would never
    /// match.
    pub fn description_key(c: &ApiContract) -> String {
        Self::description_key_of(
            &c.symbol, &c.sec_type, &c.exchange,
            &crate::types::model::contract_identity(
                &c.last_trade_date_or_contract_month, c.strike, &c.right,
                &c.multiplier, &c.currency,
            ),
            &c.primary_exchange, &c.local_symbol, &c.trading_class,
            &c.sec_id_type, &c.sec_id, &c.currency,
        )
    }

    /// The same key from the parts, for surfaces that carry their own contract
    /// type rather than this one.
    ///
    /// Everything the lookup narrows on is in here. Two descriptions that
    /// differ only in a field the key leaves out are one key, and the second
    /// order goes out under the first one's contract.
    pub fn description_key_of(
        symbol: &str, sec_type: &str, exchange: &str, identity: &str,
        primary_exchange: &str, local_symbol: &str, trading_class: &str,
        sec_id_type: &str, sec_id: &str, currency: &str,
    ) -> String {
        // The currency verbatim, beside the identity that has already folded
        // it. A contract's identity treats saying nothing and saying USD as
        // the same thing, which is right for the slot an order is placed
        // through and wrong here: the lookup sends the currency as a filter,
        // so a description that stated none can be answered with a listing in
        // another one — and under a shared key, the next order that does say
        // USD would be placed on it.
        format!(
            "{symbol}|{sec_type}|{exchange}|{identity}|{primary_exchange}|\
             {local_symbol}|{trading_class}|{sec_id_type}|{sec_id}|{currency}"
        )
    }

    /// The contract the venue named for a description, if it has named one.
    pub fn named_for(&self, key: &str) -> Option<ApiContract> {
        self.named_by_description.lock().unwrap().get(key).cloned()
    }

    /// Remember what the venue named a description, for the next order on it.
    pub fn remember_named(&self, key: String, contract: ApiContract) {
        self.named_by_description.lock().unwrap().insert(key, contract);
    }

    /// The account's holdings, each given a moment for its definition to land.
    ///
    /// A holding arrives as a contract id and a quantity, and its definition
    /// is fetched separately; handed over before that lands it names no
    /// instrument a caller can identify. The set is read inside the wait and
    /// answered as read: waiting on one set and answering another hands back
    /// a holding that arrived between the two, which no lookup has named.
    /// `pause` is how the wait sleeps, so a caller holding an interpreter can
    /// let it go.
    pub fn named_positions(&self, shared: &SharedState, pause: impl Fn(std::time::Duration)) -> Vec<PositionInfo> {
        let from = std::time::Instant::now();
        let mut held = shared.portfolio.position_infos();
        while from.elapsed() < std::time::Duration::from_secs(2)
            && held.iter().any(|pi| {
                pi.position != 0.0 && pi.symbol.is_empty() && self.get_contract(pi.con_id, shared).is_none()
            })
        {
            pause(std::time::Duration::from_millis(20));
            held = shared.portfolio.position_infos();
        }
        held
    }

    /// The account's holdings, asked as a question: read once the account has
    /// finished stating them, as `req_positions` reads them, and named as
    /// [`named_positions`](Self::named_positions) names them. Where it had not
    /// finished within the ten seconds `req_positions` gives it, what this
    /// session already held is answered and the log says so. Refused under
    /// 504 where the session ends first. Nothing is subscribed.
    pub fn held_positions(
        &self, shared: &SharedState, pause: impl Fn(std::time::Duration),
    ) -> Result<Vec<PositionInfo>, Refusal> {
        for _ in 0..1000 {
            if shared.portfolio.account_download_complete() || shared.reference.session_over().is_some() {
                break;
            }
            pause(std::time::Duration::from_millis(10));
        }
        if let Some(why) = shared.reference.session_over() {
            return Err(Refusal::not_connected(format!("the session is over: {why}")));
        }
        if !shared.portfolio.account_download_complete() {
            log::warn!(
                "the account had not finished stating its holdings within the wait, so what \
                 follows is what this session already held rather than what the account holds",
            );
        }
        Ok(self.named_positions(shared, pause))
    }

    /// Cache a contract for later enrichment.
    pub fn cache_contract(&self, con_id: i64, contract: ApiContract) {
        self.contract_cache.lock().unwrap().insert(con_id, contract);
    }

    /// Look up a contract: merge local cache with shared reference for richest data.
    pub fn get_contract(&self, con_id: i64, shared: &SharedState) -> Option<ApiContract> {
        let local = self.contract_cache.lock().unwrap().get(&con_id).cloned();
        let shared_ref = shared.reference.get_contract(con_id);
        match (local, shared_ref) {
            (Some(mut l), Some(s)) => {
                // Enrich local with shared reference fields (secdef has richer data)
                if l.local_symbol.is_empty() { l.local_symbol = s.local_symbol; }
                if l.trading_class.is_empty() { l.trading_class = s.trading_class; }
                if l.primary_exchange.is_empty() { l.primary_exchange = s.primary_exchange; }
                Some(l)
            }
            (Some(l), None) => Some(l),
            (None, Some(s)) => Some(s),
            (None, None) => None,
        }
    }

    /// Ask the engine for a tick-by-tick stream.
    ///
    /// The engine names the contract where the caller described it, refuses a
    /// second stream under a number already carrying one, and asks the venue.
    /// Nothing waits here.
    pub fn register_tbt(
        &self,
        shared: &SharedState,
        control_tx: &Sender<ControlCommand>,
        req_id: i64,
        contract: ContractRef,
        filters: SecDefFilters,
        tbt_type: TbtType,
        number_of_ticks: u32,
        ignore_size: bool,
    ) -> Result<(), Refusal> {
        shared.admit(control_tx, ControlCommand::SubscribeTbt {
            contract,
            req_id,
            tbt_type,
            number_of_ticks,
            ignore_size,
            filters,
        })
    }

    /// Look up req_id for an instrument.
    pub fn req_id_for_instrument(&self, instrument: InstrumentId) -> i64 {
        self.instrument_to_req.lock().unwrap()
            .get(&instrument).copied().unwrap_or(-1)
    }

    // ── Display groups ──

    /// The groups this client offers: seven, numbered from one. Nothing about
    /// a group crosses the wire, so the number is this client's own.
    const DISPLAY_GROUPS: i32 = 7;

    /// What a group holds when nothing has been put in it.
    const NO_CONTRACT: &'static str = "none";

    /// Ask which display groups exist.
    pub fn query_display_groups(&self, req_id: i64) {
        let groups = (1..=Self::DISPLAY_GROUPS)
            .map(|g| g.to_string())
            .collect::<Vec<_>>()
            .join("|");
        self.pending_group_events.lock().unwrap().push(GroupEvent::List(req_id, groups));
    }

    /// Follow a group. The caller is told what the group holds now, not only
    /// what it changes to, or a caller that subscribes to a settled group
    /// hears nothing at all.
    pub fn subscribe_to_group_events(&self, req_id: i64, group_id: i32) {
        self.group_subscriptions.lock().unwrap().insert(req_id, group_id);
        let held = self.display_groups.lock().unwrap()
            .get(&group_id)
            .cloned()
            .unwrap_or_else(|| Self::NO_CONTRACT.to_string());
        self.pending_group_events.lock().unwrap().push(GroupEvent::Updated(req_id, held));
    }

    /// Stop the from group events.
    pub fn unsubscribe_from_group_events(&self, req_id: i64) {
        self.group_subscriptions.lock().unwrap().remove(&req_id);
    }

    /// Put a contract in the group the request follows, and tell everyone else
    /// following it. The caller that made the change is told too, which is what
    /// keeps two callers holding the same group in step.
    pub fn update_display_group(&self, req_id: i64, contract_info: &str) -> Result<(), String> {
        let subs = self.group_subscriptions.lock().unwrap();
        let Some(&group_id) = subs.get(&req_id) else {
            return Err(format!(
                "request {req_id} follows no display group, so there is none to put a \
                 contract in: subscribe to a group first"
            ));
        };
        let followers: Vec<i64> = subs.iter()
            .filter(|(_, g)| **g == group_id)
            .map(|(r, _)| *r)
            .collect();
        drop(subs);
        let value = if contract_info.is_empty() { Self::NO_CONTRACT } else { contract_info };
        self.display_groups.lock().unwrap().insert(group_id, value.to_string());
        let mut pending = self.pending_group_events.lock().unwrap();
        for r in followers {
            pending.push(GroupEvent::Updated(r, value.to_string()));
        }
        Ok(())
    }

    /// Take every group events waiting, leaving none.
    pub fn drain_group_events(&self) -> Vec<GroupEvent> {
        self.pending_group_events.lock().unwrap().drain(..).collect()
    }

    pub(crate) fn select_account_updates(&self, account: &str) {
        self.account_routes.lock().unwrap().updates = account.to_string();
    }

    pub(crate) fn updates_account(&self, shared: &SharedState) -> String {
        shared.account_name(&self.account_routes.lock().unwrap().updates)
    }

    pub(crate) fn select_multi_account(&self, req_id: i64, account: &str, model: &str) {
        self.account_routes.lock().unwrap().multi.insert(req_id, (account.to_string(), model.to_string()));
    }

    pub(crate) fn multi_account(&self, shared: &SharedState, req_id: i64) -> String {
        shared.account_name(self.account_routes.lock().unwrap().multi.get(&req_id).map(|(account, _)| account.as_str()).unwrap_or(""))
    }

    pub(crate) fn select_positions_account(&self, req_id: i64, account: &str, model: &str) {
        self.account_routes.lock().unwrap().positions.insert(req_id, (account.to_string(), model.to_string()));
    }

    pub(crate) fn positions_account(&self, shared: &SharedState, req_id: i64) -> String {
        shared.account_name(self.account_routes.lock().unwrap().positions.get(&req_id).map(|(account, _)| account.as_str()).unwrap_or(""))
    }

    pub(crate) fn multi_model(&self, req_id: i64) -> String {
        self.account_routes.lock().unwrap().multi.get(&req_id).map(|(_, model)| model.clone()).unwrap_or_default()
    }

    pub(crate) fn multi_watchers(&self) -> Vec<i64> {
        let mut ids: Vec<_> = self.account_routes.lock().unwrap().multi.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub(crate) fn forget_multi_account(&self, req_id: i64) {
        self.account_routes.lock().unwrap().multi.remove(&req_id);
    }

    pub(crate) fn positions_model(&self, req_id: i64) -> String {
        self.account_routes.lock().unwrap().positions.get(&req_id).map(|(_, model)| model.clone()).unwrap_or_default()
    }

    pub(crate) fn positions_watchers(&self) -> Vec<i64> {
        let mut ids: Vec<_> = self.account_routes.lock().unwrap().positions.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub(crate) fn forget_positions_account(&self, req_id: i64) {
        self.account_routes.lock().unwrap().positions.remove(&req_id);
    }

    /// A model or a group whose membership has not been stated stays unapplied.
    pub(crate) fn note_account_selection(shared: &SharedState, name: &str) {
        if !name.is_empty() { shared.note_unapplied_account_selection(name); }
    }

    // ── PnL subscription management ──

    /// Ask for the running profit under a request number of its own.
    pub fn subscribe_pnl(&self, req_id: i64, account: &str) -> Result<(), Refusal> {
        let mut requests = self.pnl_req_id.lock().unwrap();
        if requests.contains_key(&req_id) {
            return Err(Refusal::stated(crate::error_codes::DUPLICATE_TICKER_ID, "Duplicate ticker id"));
        }
        self.last_pnl.lock().unwrap().remove(&req_id);
        requests.insert(req_id, account.to_string());
        Ok(())
    }

    /// Stop reporting the profit under this request number.
    pub fn unsubscribe_pnl(&self, req_id: i64) {
        self.pnl_req_id.lock().unwrap().remove(&req_id);
        self.last_pnl.lock().unwrap().remove(&req_id);
    }

    /// Ask for the pnl single.
    pub fn subscribe_pnl_single(&self, req_id: i64, con_id: i64, account: &str) -> Result<(), Refusal> {
        let mut requests = self.pnl_single_reqs.lock().unwrap();
        if requests.contains_key(&req_id) {
            return Err(Refusal::stated(crate::error_codes::DUPLICATE_TICKER_ID, "Duplicate ticker id"));
        }
        requests.insert(req_id, (account.to_string(), con_id));
        // What was last reported under this number belonged to whatever it
        // watched before. Kept, a number pointed at another contract inherited
        // the last one's day — or, where the two happened to agree, reported
        // nothing at all until something moved, because a figure is only sent
        // when it differs from what was sent before. The withdrawal beside
        // this one clears it for the same reason; taking the number without
        // withdrawing it did not.
        self.last_pnl_single.lock().unwrap().remove(&req_id);
        Ok(())
    }

    /// Stop the pnl single.
    pub fn unsubscribe_pnl_single(&self, req_id: i64) {
        self.pnl_single_reqs.lock().unwrap().remove(&req_id);
        self.last_pnl_single.lock().unwrap().remove(&req_id);
    }

    // ── Account summary subscription management ──

    /// What a gateway refuses an account summary for, checked in the order it
    /// checks them, each under its own words.
    ///
    /// The group is exactly `All` or `AllNonProp` on a login that is not an
    /// advisor's; an advisor's own group names are its to state, and are not
    /// refused here. `All` is refused where the venue says the login may not
    /// ask for it, unless it also says requests for every position are let
    /// through.
    pub fn check_account_summary(shared: &SharedState, group: &str, tags: &str) -> Result<(), Refusal> {
        if tags.is_empty() {
            return Err(Refusal::validation("Tags cannot be null"));
        }
        if group.is_empty() {
            return Err(Refusal::validation("Group name cannot be null"));
        }
        let advisor = shared.reference.advisor();
        if !advisor && group != "All" && group != "AllNonProp" {
            return Err(Refusal::validation("Group name is invalid"));
        }
        let features = shared.reference.enabled_features();
        let has = |token: &str| features.iter().any(|f| f == token);
        if group.eq_ignore_ascii_case("All") && !has("APIREQALLPOS") {
            if !advisor && has("NOALL") {
                return Err(Refusal::validation("ALL account is not supported"));
            }
            if has("DYNACCTADD") {
                return Err(Refusal::stated(
                    crate::error_codes::ALL_NOT_FOR_DYNAMIC_ACCOUNTS,
                    "This API request for All is not supported for Dynamic Account Addition",
                ));
            }
        }
        Ok(())
    }

    /// Whether a gateway counts this login as holding several accounts, by the
    /// rule an order is checked under.
    pub fn login_holds_several_accounts(shared: &SharedState) -> bool {
        let (logon_accounts, _) = shared.reference.login();
        holds_several_accounts(&logon_accounts, &shared.reference.enabled_features())
    }

    /// Whether a gateway takes any account named, rather than only one the
    /// login held at logon: a login the venue adds accounts to, where it does
    /// not say the API may not take them.
    fn takes_accounts_added_later(shared: &SharedState) -> bool {
        let features = shared.reference.enabled_features();
        let has = |token: &str| features.iter().any(|f| f == token);
        has("DYNACCTADD") && !has("NOAPIDYNADD")
    }

    /// The account `reqAccountUpdates` names, checked as a gateway checks it.
    ///
    /// A login holding one account is answered for it whatever is named, and
    /// the name is ignored, as a gateway ignores it. On a login holding
    /// several, a subscription names one it holds; `All`, where the login may
    /// ask for every account; or `AllNonProp`, where the venue offers it and
    /// the logon names accounts it leaves out.
    pub fn check_account_updates(
        shared: &SharedState, accounts: &[String], subscribe: bool, code: &str,
    ) -> Result<(), Refusal> {
        if !Self::login_holds_several_accounts(shared) {
            if !code.is_empty() {
                log::debug!("Account code is ignored for non multiple account customers.");
            }
            return Ok(());
        }
        if !subscribe {
            return Ok(());
        }
        if code.is_empty() {
            return Err(Refusal::validation("The account code is required for this operation."));
        }
        let features = shared.reference.enabled_features();
        let has = |token: &str| features.iter().any(|f| f == token);
        if code == "All" && !shared.reference.advisor() && has("NOALL") {
            return Err(Refusal::validation("ALL account is not supported"));
        }
        let held = accounts.iter().any(|a| a == code)
            || code == "All"
            || (code == "AllNonProp" && has("ALLNONPROP") && shared.reference.all_non_prop_leaves_out());
        if !held && !Self::takes_accounts_added_later(shared) {
            return Err(Refusal::validation(format!("Invalid account code '{code}'.")));
        }
        Ok(())
    }

    /// The account an execution filter names, checked as a gateway checks it.
    ///
    /// A login holding one account is answered for it whatever is named, and
    /// the name is ignored, as a gateway ignores it; on one holding several,
    /// an account the login does not hold is refused.
    pub fn check_execution_account(
        shared: &SharedState, accounts: &[String], filter: &mut ExecutionFilter,
    ) -> Result<(), Refusal> {
        if filter.acct_code.is_empty() {
            return Ok(());
        }
        if !Self::login_holds_several_accounts(shared) {
            log::debug!("The accountCode is ignored for single account customers");
            filter.acct_code.clear();
            return Ok(());
        }
        if !Self::takes_accounts_added_later(shared) && !accounts.contains(&filter.acct_code) {
            return Err(Refusal::validation(format!("Invalid account code {}.", filter.acct_code)));
        }
        Ok(())
    }

    /// The account a profit request names, checked in a gateway's order.
    /// Blank or unavailable accounts and prohibited all-account selections
    /// are refused in its words.
    pub fn check_pnl_account(
        shared: &SharedState, accounts: &[String], session: &str, account: &str,
    ) -> Result<(), Refusal> {
        if account.trim().is_empty() {
            return Err(Refusal::validation("Account must not be empty"));
        }
        let features = shared.reference.enabled_features();
        let has = |token: &str| features.iter().any(|f| f == token);
        let every = account.eq_ignore_ascii_case("All");
        let no_all = !shared.reference.advisor() && has("NOALL");
        if !Self::takes_accounts_added_later(shared) {
            let held = account == session
                || accounts.iter().any(|a| a == account)
                || (every && Self::login_holds_several_accounts(shared) && !no_all);
            if !held {
                return Err(Refusal::validation("Invalid account code"));
            }
        }
        if every && has("DYNACCTADD") {
            return Err(Refusal::validation(
                "This API request for All is not supported for Dynamic Account Addition",
            ));
        }
        if every && no_all {
            return Err(Refusal::validation("ALL account is not supported"));
        }
        Ok(())
    }

    /// Ask for the account summary.
    ///
    /// Both subscriptions stay until cancelled, including after their first
    /// answers; a third is refused under 322, as a gateway refuses it.
    pub fn subscribe_account_summary(&self, req_id: i64, tags: &str, accounts: Vec<String>) -> Result<(), Refusal> {
        let tag_list: Vec<String> = tags.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut slot = self.account_summary_req.lock().unwrap();
        let mut other = self.account_summary_other_req.lock().unwrap();
        let target = if slot.as_ref().is_some_and(|(id, _)| *id == req_id) {
            &mut *slot
        } else if other.as_ref().is_some_and(|(id, _)| *id == req_id) {
            &mut *other
        } else if slot.is_none() {
            &mut *slot
        } else if other.is_none() {
            &mut *other
        } else {
            // The limit a gateway sets, under the number and in the words it
            // reports it with.
            return Err(Refusal::stated(
                crate::error_codes::REQUEST_NOT_PROCESSED,
                "Maximum number of account summary requests exceeded; desubscribe to previous \
                 request first",
            ));
        };
        self.account_routes.lock().unwrap().summaries.insert(req_id, accounts);
        *target = Some((req_id, tag_list));
        self.last_account_summary.lock().unwrap().remove(&req_id);
        Ok(())
    }

    /// Stop the account summary.
    pub fn unsubscribe_account_summary(&self, req_id: i64) {
        let mut req = self.account_summary_req.lock().unwrap();
        let mut other = self.account_summary_other_req.lock().unwrap();
        if req.as_ref().map(|(r, _)| *r) == Some(req_id) {
            *req = None;
        }
        if other.as_ref().map(|(r, _)| *r) == Some(req_id) {
            *other = None;
        }
        self.last_account_summary.lock().unwrap().remove(&req_id);
        self.account_routes.lock().unwrap().summaries.remove(&req_id);
    }

    // ── Account updates subscription management ──

    /// Ask for the account updates, or stop asking.
    ///
    /// What was last stated is forgotten either way: asked again, the account
    /// is restated in full and its end said again, as the reference client
    /// answers a second request. Forgotten on the withdrawal alone, a second
    /// ask found every figure already stated and answered with nothing, and
    /// the end a caller was waiting on never came.
    pub fn subscribe_account_updates(&self, subscribe: bool) {
        self.account_updates_subscribed.store(subscribe, Ordering::Release);
        self.last_stated_account.lock().unwrap().clear();
        self.account_end_sent.store(false, Ordering::Release);
        *self.last_portfolio.lock().unwrap() = None;
    }

    // ── Market data type tracking ──

    /// Store the requested market data type.
    ///
    /// The caller names the type once; subscriptions made after this carry
    /// the feeds it turns on.
    ///
    /// A type turns feeds on rather than naming one, as a gateway takes it: 2
    /// turns frozen data on; 3 and 4 turn delayed data on, 4 with
    /// delayed-frozen and 3 without; only 1 turns frozen data off, and it
    /// turns all three off.
    pub fn set_market_data_type(&self, mdt: i32) {
        if !matches!(mdt, MDT_REALTIME | MDT_FROZEN | MDT_DELAYED | MDT_DELAYED_FROZEN) {
            // Kept out rather than kept: the feeds stay as they were whatever
            // this names, and the callback that reports a subscription's type
            // reads what is stored — so a number nobody recognises, stored,
            // reaches the caller as the venue's word for data that is not on
            // it.
            log::warn!("req_market_data_type({mdt}) names no known type; the feeds stay as they were");
            return;
        }
        self.market_data_type.store(mdt, Ordering::Relaxed);
        let _ = self.market_data_feeds.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |feeds| {
            Some(match mdt {
                MDT_FROZEN => feeds | FEED_FROZEN,
                MDT_DELAYED => (feeds | FEED_DELAYED) & !FEED_DELAYED_FROZEN,
                MDT_DELAYED_FROZEN => feeds | FEED_DELAYED | FEED_DELAYED_FROZEN,
                _ => 0,
            })
        });
    }

    /// The per-subscription mode the session's feeds imply. A gateway asks
    /// the live feed first whatever the type, so this is the delayed feed a
    /// refusal falls back to where delayed data is on, 1, and otherwise 0. The
    /// frozen feeds are served where the market's status is watched. A
    /// calculation's subscription, which falls back to nothing, asks this
    /// feed directly.
    pub fn subscription_mode(&self) -> i32 {
        if self.market_data_feeds.load(Ordering::Relaxed) & FEED_DELAYED != 0 { 1 } else { 0 }
    }

    /// Check if the `market_data_type` callback should fire for this req_id.
    /// Returns `Some(type)` on the first call per req_id that has data, `None`
    /// until the feed changes. Every watcher is told the type accepted for the subscription
    /// it follows, because joining it sends no request to change the feed.
    pub fn check_mdt_needed(&self, req_id: i64, has_data: bool) -> Option<i32> {
        if !has_data { return None; }
        let data_type = self.watching(req_id)
            .and_then(|instrument| self.mdt_by_instrument.lock().unwrap().get(&instrument).copied())
            .unwrap_or_else(|| self.market_data_type.load(Ordering::Relaxed));
        let previous = self.mdt_sent.lock().unwrap().insert(req_id, data_type);
        (previous != Some(data_type)).then_some(data_type)
    }

    /// Record the feed accepted by the venue, in delivery order.
    pub fn note_mkt_data_type(&self, instrument: InstrumentId, data_type: i32) {
        self.mdt_by_instrument.lock().unwrap().insert(instrument, data_type);
    }

    // ── Bulletin subscription management ──

    /// Ask for the bulletins.
    pub fn subscribe_bulletins(&self) {
        self.bulletin_subscribed.store(true, Ordering::Release);
    }

    /// Stop the bulletins.
    pub fn unsubscribe_bulletins(&self) {
        self.bulletin_subscribed.store(false, Ordering::Release);
    }

    /// Whether broadcast notices are being received.
    pub fn bulletins_subscribed(&self) -> bool {
        self.bulletin_subscribed.load(Ordering::Acquire)
    }

    // ── Execution replay store ──

    /// Store an execution for later replay via `req_executions`.
    ///
    /// Once per execution. The venue restates the day's executions at every
    /// logon, so the same one can arrive again; it is known again by the id
    /// it carries. One carrying no id is known by its content, as the engine
    /// knows it: an absent id is the shape a replay takes, and kept on every
    /// replay a caller summing the day's volume doubled it on every rebuilt
    /// connection. The cumulative quantity is what tells two otherwise
    /// identical prints of one order apart.
    pub fn push_execution(&self, contract: ApiContract, execution: ApiExecution, commission_and_fees: ApiCommissionAndFeesReport) {
        let mut store = self.executions.lock().unwrap();
        if execution.exec_id.is_empty() {
            let same = |stored: &StoredExecution| {
                let e = &stored.execution;
                e.exec_id.is_empty()
                    && e.order_id == execution.order_id
                    && e.time == execution.time
                    && e.shares == execution.shares
                    && e.price == execution.price
                    && e.cum_qty == execution.cum_qty
            };
            if store.rows.iter().any(same) {
                return;
            }
        } else if store.by_id.contains_key(&execution.exec_id) {
            return;
        } else {
            let at = store.rows.len();
            store.by_id.insert(execution.exec_id.clone(), at);
        }
        store.rows.push(StoredExecution { contract, execution, commission_and_fees });
    }

    /// File the executions the venue restated, announcing none of them.
    ///
    /// A restated execution books nothing — the quantity is already held, or
    /// the order was never this session's — so it never becomes a fill, and
    /// the fill path was the only way into the record `req_executions` answers
    /// from. After a restart that record was empty, and a caller asking was
    /// told, silently, that nothing had filled. Called from the dispatch pass
    /// on either surface: a caller that asks is answered, and one that did
    /// not hears nothing.
    pub fn record_restated_executions(&self, shared: &SharedState) {
        for (contract, mut execution) in shared.orders.drain_restated_executions() {
            if execution.order_id >= 0 {
                let wire = execution.order_id as u64;
                self.learn_order_identity(shared, wire);
                execution.order_id = self.api_order_id(wire);
            }
            // Unsolicited, as a live fill is stored, and costed the same way:
            // what it cost arrives on a record of its own, if it arrives.
            self.push_execution(contract, execution, ApiCommissionAndFeesReport::default());
        }
    }

    /// Note what an execution cost against the execution itself, so a replay
    /// of it carries the charge the venue stated rather than the nothing it
    /// was stored with.
    pub fn record_charge(&self, charge: &ApiCommissionAndFeesReport) {
        // A charge naming no execution stamps none. Matched on the empty name,
        // it was written onto every execution stored without one.
        if charge.exec_id.is_empty() {
            return;
        }
        let mut store = self.executions.lock().unwrap();
        if let Some(at) = store.by_id.get(&charge.exec_id).copied() {
            store.rows[at].commission_and_fees = charge.clone();
        }
    }

    /// Executions matching `filter`, cloned out under one short lock.
    ///
    /// Callers replay these into user callbacks, and a callback may re-enter
    /// any path that locks `executions` — re-requesting from `exec_details` is
    /// an ordinary ibapi pattern, and the dispatch thread pushes fills through
    /// the same mutex. Handing back indices to be dereferenced later also
    /// raced `reset()`, which clears the vector. Snapshotting closes both.
    pub fn snapshot_executions(&self, filter: &ExecutionFilter) -> Vec<StoredExecution> {
        let store = self.executions.lock().unwrap();
        store.rows.iter().filter(|se| execution_matches(se, filter)).cloned().collect()
    }

    /// The executions a `reqExecutions` is answered with: those the filter
    /// matches, on the days it asks for, counted on the session's clock —
    /// and the days asked for that start before what the session holds.
    ///
    /// Each day is a day on `zone`, which is the zone the session announced
    /// at logon, as a gateway counts days on its own. An execution whose time
    /// cannot be read is kept, as the time bound keeps one.
    ///
    /// The account is checked as [`check_execution_account`](Self::check_execution_account)
    /// checks it. A day asked for that starts before the executions the
    /// session opened with is answered with what is held of it, and named in
    /// the second list, so the caller can be told rather than handed a short
    /// answer in silence.
    ///
    /// ponytail: answered from the executions the session holds, which reach
    /// back to midnight six days before the logon in UTC, or to the logon's
    /// own day for a session set to today's executions. A gateway asks the
    /// venue for each day the window names; how that answer ends has not
    /// been seen, so it is not asked here.
    pub fn executions_for_request(
        &self, shared: &SharedState, accounts: &[String], filter: &ExecutionFilter, now: jiff::Timestamp,
    ) -> Result<(Vec<StoredExecution>, Vec<jiff::civil::Date>), Refusal> {
        let zone = shared.settings().timezone.clone();
        let clock = crate::protocol::datetime::clock_named(&zone).unwrap_or_else(|| {
            log::warn!("the session's time zone {zone} cannot be read, so days are counted on UTC");
            jiff::tz::TimeZone::UTC
        });
        let today = now.to_zoned(clock.clone()).date();
        // The days as the request is read, then the account as it is checked:
        // a gateway reads a request before it checks one.
        let days = execution_days(filter.last_n_days, &filter.specific_dates, today)?;
        let mut filter = filter.clone();
        Self::check_execution_account(shared, accounts, &mut filter)?;
        let rows = self.snapshot_executions(&filter);
        let Some(days) = days else {
            return Ok((rows, Vec::new()));
        };
        let held_from = shared.reference.executions_held_from();
        let mut unheld: Vec<jiff::civil::Date> = days.iter().copied()
            .filter(|day| held_from.is_some_and(|from| {
                day.to_zoned(clock.clone()).is_ok_and(|start| start.timestamp().as_second() < from)
            }))
            .collect();
        unheld.sort();
        let rows = rows.into_iter()
            .filter(|se| {
                let Some(at) = crate::protocol::datetime::ib_datetime_to_unix(&se.execution.time)
                    .and_then(|secs| jiff::Timestamp::from_second(secs).ok())
                else {
                    return true;
                };
                days.contains(&at.to_zoned(clock.clone()).date())
            })
            .collect();
        Ok((rows, unheld))
    }

    /// What a caller is told of the days an execution request asked for that
    /// start before what the session holds.
    pub fn unheld_days_notice(unheld: &[jiff::civil::Date]) -> Option<String> {
        if unheld.is_empty() {
            return None;
        }
        let named: Vec<String> = unheld.iter().map(ToString::to_string).collect();
        Some(format!(
            "the executions this session holds do not reach back to the start of {}, so \
             what follows for those days is only what it holds of them",
            named.join(", "),
        ))
    }

    // ── Open order tracking ──

    /// The parent this client recorded when it placed the order, if any.
    ///
    /// The engine reads no parent from an execution report, so for an order
    /// this client placed its own record is the only source. An order placed
    /// elsewhere keeps whatever the engine reports.
    pub(crate) fn tracked_parent_id(&self, order_id: u64) -> Option<i64> {
        let parent = self.open_orders.lock().unwrap()
            .get(&order_id)
            .map(|t| t.order.parent_id)
            .filter(|p| *p > 0);
        parent.map(|wire| self.api_order_id(wire as u64))
    }

    /// Check if an order with this ID is currently tracked (for modify detection).
    pub fn is_order_tracked(&self, order_id: u64) -> bool {
        self.open_orders.lock().unwrap().contains_key(&order_id)
    }

    /// Whether this client placed the order. An order it learnt of from the
    /// venue is tracked here too, once a status has arrived or a replace has
    /// restated it, and is not this.
    pub fn placed_here(&self, order_id: u64) -> bool {
        self.open_orders.lock().unwrap().get(&order_id).is_some_and(|t| t.placed_here)
    }

    /// Whether this id names an order the venue is working, as this side's
    /// record stands: what decides a refusal at the call is a placement or a
    /// change. The engine, which keeps what does not transmit, decides it for
    /// the order itself.
    ///
    /// The venue's own book is asked beside this client's. An order the venue
    /// replayed at connect was not placed here, so it is in no local book, and
    /// asked of the local book alone a replace of one read as a first
    /// placement: it went out as a new order under a number the venue is
    /// already working, and the record of the order it named was overwritten
    /// on the way.
    pub fn is_working_at_the_venue(&self, order_id: u64, venue: Option<&SharedState>) -> bool {
        self.is_order_tracked(order_id)
            || venue.is_some_and(|v| v.orders.venue_is_working(order_id))
    }

    /// Whether a replace names the contract the venue says the order is on.
    ///
    /// A replace names the order and not the contract, so one naming another
    /// contract is refused rather than recorded against it. A combination is
    /// not one contract: what identifies it is the set of legs it is built
    /// from, and after a description is named the id on it is one leg's
    /// underlying — two different combinations on one underlying carry the
    /// same id and register the same slot, so comparing ids could never tell
    /// them apart.
    ///
    /// Where one side states neither an id nor legs there is nothing to
    /// compare, and the replace stands. **No record the venue writes states
    /// legs today** — neither the connect replay nor an execution report does
    /// — so in practice a combination reaches that escape and is not compared
    /// at all. The comparison below is what to do when one does; changing what
    /// the venue's record carries is a question about the wire, not about
    /// this.
    pub(crate) fn names_the_same_contract(known: &ApiContract, stated: &ApiContract) -> bool {
        if !known.combo_legs.is_empty() || !stated.combo_legs.is_empty() {
            if known.combo_legs.is_empty() || stated.combo_legs.is_empty() {
                return true;
            }
            // The legs, whatever order they are stated in: a combination is
            // the same combination however it is written down.
            let legs = |c: &ApiContract| {
                let mut v: Vec<(i64, i32, String)> = c.combo_legs.iter()
                    .map(|l| (l.con_id, l.ratio, l.action.to_ascii_uppercase()))
                    .collect();
                v.sort();
                v
            };
            return legs(known) == legs(stated);
        }
        known.con_id == 0 || stated.con_id == 0 || known.con_id == stated.con_id
    }

    /// The slot a tracked order was placed on, if it is tracked.
    ///
    /// A replace names the order and not the contract, so this is the
    /// instrument a replacement stays on whatever contract it carries.
    pub fn tracked_instrument(&self, order_id: u64) -> Option<InstrumentId> {
        self.open_orders.lock().unwrap().get(&order_id).map(|t| t.instrument)
    }

    /// The order a tracked id was submitted with, if it is tracked.
    pub fn tracked_order(&self, order_id: u64) -> Option<ApiOrder> {
        let order = self.open_orders.lock().unwrap().get(&order_id).map(|t| t.order.clone());
        order.map(|mut order| {
            order.order_id = self.api_order_id(order_id);
            if order.parent_id > 0 { order.parent_id = self.api_order_id(order.parent_id as u64); }
            order
        })
    }

    /// The price a replace names, read from the field the shape's own submit
    /// reads it from: a trailing stop limit's limit offset, and every other
    /// type's limit price. Unset names nothing.
    ///
    /// One place, so the two bindings cannot diverge on it.
    pub fn replace_price(order: &ApiOrder) -> i64 {
        let named = if order.order_type_named() == Some("TRAIL LIMIT")
            && order.lmt_price_offset != f64::MAX
        {
            order.lmt_price_offset
        } else {
            order.lmt_price
        };
        Self::price_or_unset(named)
    }

    /// The trigger a replace names: the auxiliary price, as the submit reads
    /// it. Unset names nothing.
    pub fn replace_trigger(order: &ApiOrder) -> i64 {
        Self::price_or_unset(order.aux_price)
    }

    /// Why a modify of `order_id` cannot be sent, if it cannot.
    ///
    /// A replace is the caller's statement of the order, restated whole: its
    /// type, its prices and everything it carries go out as its own placement
    /// would state them. So a change of type, a relative or pegged order, a
    /// minimum quantity and a trail moved from a percentage to an amount are
    /// all sent, as a gateway sends them.
    ///
    /// What is refused is what a gateway refuses. A one-cancels-all group
    /// cannot be changed by a replace where both the order and the change name
    /// one. The way a group cancels cannot be changed at all, whether or not
    /// either names a group, and a way that is not one of the four reads as
    /// the default, reduce on fill without block. The venue can lift both at
    /// logon. A parent or a group named on an order that had none is taken,
    /// and the order goes on under the links it was placed with.
    ///
    /// A what-if preview under the number of an order is refused as well: the
    /// preview is not an order the venue holds, so a replace has nothing to act
    /// on, and previewing beside a working order under its number is not
    /// something this client does yet.
    ///
    /// One place, so the two bindings cannot diverge on either the rule or the
    /// wording. The resting order is this client's own record where it placed
    /// the order, and otherwise the venue's statement of it.
    pub fn modify_refusal(&self, order_id: u64, incoming: &ApiOrder, venue: Option<&SharedState>) -> Option<Refusal> {
        let tracked = self.tracked_order(order_id)
            .or_else(|| venue.and_then(|v| v.orders.get_order_info(order_id)).map(|info| info.order));
        Self::modify_refusal_of(tracked, incoming, venue)
    }

    /// [`modify_refusal`](Self::modify_refusal), against the resting order
    /// as its caller holds it: the engine's own record where it placed the
    /// order, and otherwise the venue's statement of it.
    pub fn modify_refusal_of(
        tracked: Option<ApiOrder>, incoming: &ApiOrder, venue: Option<&SharedState>,
    ) -> Option<Refusal> {
        if incoming.what_if || tracked.as_ref().is_some_and(|t| t.what_if) {
            return Some(Refusal::stated(
                CHANGE_CANNOT_CHANGE_TYPE,
                "a what-if order cannot be modified: a preview is not an order the venue \
                 holds, so there is nothing on the book for a replace to act on",
            ));
        }
        let tracked = tracked?;
        if venue.is_some_and(|v| v.reference.enables("NOAPIOCASTRICT")) {
            return None;
        }
        if !tracked.oca_group.is_empty()
            && !incoming.oca_group.is_empty()
            && tracked.oca_group != incoming.oca_group
        {
            return Some(Refusal::stated(OCA_GROUP_REVISION, "OCA group revision is not allowed"));
        }
        let way = |t: i32| if (1..=4).contains(&t) { t } else { 3 };
        if way(tracked.oca_type) != way(incoming.oca_type) {
            return Some(Refusal::stated(OCA_TYPE_REVISION, "OCA group type revision is not allowed"));
        }
        None
    }

    /// The venue has taken the replacement outstanding on this order, so the
    /// terms kept against a refusal are spent.
    ///
    /// Read off a status before this — the venue working the order again —
    /// which is not the same fact: a fill landing between the attempt and the
    /// answer took the more advanced status and the acknowledgement behind it
    /// was never announced, so the copy outlived the replacement the venue had
    /// taken and the next refusal put back terms from before it.
    pub fn settle_replacement(&self, order_id: u64) {
        if let Some(tracked) = self.open_orders.lock().unwrap().get_mut(&order_id) {
            tracked.before_the_replace = None;
        }
    }

    /// Put back the terms a restatement replaced, where the attempt did not
    /// stand: the venue refused it, or it never left this process.
    pub fn undo_restatement(&self, order_id: u64) {
        if let Some(tracked) = self.open_orders.lock().unwrap().get_mut(&order_id) {
            put_back_the_terms(tracked);
        }
    }

    /// Restate the terms of an order the venue is already working.
    ///
    /// A replace does not place an order, so what the order has done stands:
    /// recorded as a new one, a partly filled order came back from
    /// `req_open_orders` as pending with nothing filled and its whole quantity
    /// outstanding, and a replace the venue then refused left it reading that
    /// way for the rest of the session. The terms follow the attempt, because
    /// every later action restates from here; the execution history does not.
    ///
    /// Falls back to recording it afresh where nothing is held under the id,
    /// which is a caller replacing an order this client did not place.
    pub fn restate_order(
        &self, venue: Option<&SharedState>, order_id: u64, contract: ApiContract,
        mut order: ApiOrder, instrument: InstrumentId,
    ) {
        order.parent_id = self.wire_parent_id(order.parent_id);
        let mut orders = self.open_orders.lock().unwrap();
        match orders.get_mut(&order_id) {
            Some(tracked) => {
                // Kept against a refusal. Only the first of a run of
                // replacements records it: the second states terms the venue
                // has not answered for either — a change held back never
                // reaches it at all — and falling back to those would put back
                // an attempt rather than what the venue holds.
                if tracked.before_the_replace.is_none() {
                    tracked.before_the_replace = Some(Box::new(tracked.order.clone()));
                }
                tracked.remaining = (order.total_quantity - tracked.filled).max(0.0);
                tracked.contract = contract;
                // What the wire keeps across a replace, kept here too, in both
                // directions — for an order placed here. The engine restates
                // the parent link, the group and its type from the record of
                // the placement whatever the replace states, so a caller's
                // empty value there does not detach the order and a caller's
                // new one does not attach it, which is what a gateway does
                // with either; a group or a type changed outright is refused
                // before this.
                // An order the venue named is restated from the caller's
                // statement of it on every replace, so there the record
                // follows the caller, as the venue does. The client is the
                // record's either way: a replace states no client, and where
                // the record names none the venue's answer is read in its
                // place wherever the client is reported.
                let kept = (tracked.order.client_id, tracked.order.parent_id, tracked.order.oca_group.clone(), tracked.order.oca_type);
                let placed_here = tracked.placed_here;
                tracked.order = order;
                tracked.order.client_id = kept.0;
                if placed_here {
                    tracked.order.parent_id = kept.1;
                    tracked.order.oca_group = kept.2;
                    tracked.order.oca_type = kept.3;
                }
            }
            None => {
                // A caller replacing an order the venue replayed at connect:
                // this client did not place it, so what the venue has said is
                // the only account of what it has done. Written without that,
                // the record read as a fresh order — nothing filled, its whole
                // quantity outstanding, a status of its own invention, and
                // slot zero, which is a real slot and not this order's, so the
                // next replace was refused for naming another contract.
                let known = venue.and_then(|v| v.orders.get_order_info(order_id));
                let filled = known.as_ref().map_or(0.0, |i| i.order.filled_quantity);
                let status = known.as_ref().map_or_else(
                    || "PendingSubmit".to_string(), |i| i.order_state.status.clone(),
                );
                // And what the venue holds, for a refusal to put back. The
                // client the order went out under comes from there too: this
                // client did not place it, so the caller's own object says
                // nothing about whose order it is.
                let before_the_replace = known.map(|i| {
                    order.client_id = i.order.client_id;
                    Box::new(i.order)
                });
                let remaining = (order.total_quantity - filled).max(0.0);
                orders.insert(order_id, TrackedOrder {
                    contract, order, status, filled, remaining,
                    avg_fill_price: 0.0, last_fill_price: 0.0,
                    instrument, rejected: false, before_the_replace,
                    placed_here: false,
                });
            }
        }
    }

    /// Write what the engine did with an order a caller placed into this
    /// side's record of it, where it stands in the session's order.
    pub fn keep_the_book(&self, shared: &SharedState, entry: crate::bridge::OrderBook) {
        match entry {
            crate::bridge::OrderBook::Taken(taken) => {
                let crate::bridge::TakenOrder { order_id, contract, mut order, instrument, restated } = *taken;
                // The order as a gateway holds and states it: under its type's
                // own name, on the account it went out for, and with the
                // number it went to the venue under as its permanent id, from
                // the moment it is sent. For an order the venue named at
                // connect, that is the number the venue holds it under, not
                // this session's number for it.
                if let Some(named) = order.order_type_named() {
                    order.order_type = named.to_string();
                }
                order.account = shared.account_name(&order.account);
                order.perm_id = shared.orders.perm_id(order_id).unwrap_or(order_id as i64);
                self.learn_order_identity(shared, order_id);
                self.cache_contract(contract.con_id, contract.clone());
                if restated {
                    self.restate_order(Some(shared), order_id, contract, order, instrument);
                } else {
                    self.track_order(order_id, contract, order, instrument);
                }
            }
            crate::bridge::OrderBook::Forgotten(order_id) => {
                self.open_orders.lock().unwrap().remove(&order_id);
            }
            crate::bridge::OrderBook::RevisionForgotten(order_id) => self.undo_restatement(order_id),
            crate::bridge::OrderBook::RevisionRefused(reject) => {
                self.retire_rejected(&reject);
            }
        }
    }

    /// Track a newly placed order.
    pub fn track_order(&self, order_id: u64, contract: ApiContract, mut order: ApiOrder, instrument: InstrumentId) {
        order.parent_id = self.wire_parent_id(order.parent_id);
        self.open_orders.lock().unwrap()
            .insert(order_id, tracked_as_placed(contract, order, instrument));
    }

    /// Note that the venue has finished with a number.
    ///
    /// Recorded where an order reaches a state it cannot leave, so a later
    /// placement under the number is read as the duplicate it is rather than
    /// as a new order. Not recorded where a placement never left this client:
    /// nothing at the venue ever carried that number, and it is the caller's
    /// to use again.
    pub fn note_the_number_is_spent(&self, order_id: u64) {
        self.spent_order_ids.lock().unwrap().insert(order_id);
    }

    /// Whether the venue has already worked an order under this number.
    pub fn the_number_is_spent(&self, order_id: u64) -> bool {
        self.spent_order_ids.lock().unwrap().contains(&order_id)
    }

    /// Update a tracked order after a fill. Removes the order if fully filled.
    pub fn update_order_fill(&self, order_id: u64, status: &str, filled: f64, remaining: f64) {
        // Through the one place an order leaves the book, so what was staged
        // against it goes with it here as on every other path.
        if remaining == 0.0 {
            self.note_the_number_is_spent(order_id);
            return self.untrack_order(order_id);
        }
        if let Some(o) = self.open_orders.lock().unwrap().get_mut(&order_id) {
            o.status = status.into();
            o.filled = filled;
            o.remaining = remaining;
        }
    }

    /// Stop tracking an order the venue has said does not exist.
    ///
    /// A cancel rejected as UnknownOrder retires the engine's record, and the
    /// client's own record has to go with it — the open-order snapshot unions
    /// the two, so leaving this one behind kept reporting the order the
    /// rejection was about.
    pub fn untrack_order(&self, order_id: u64) {
        self.open_orders.lock().unwrap().remove(&order_id);
    }

    /// An order's permanent id and its parent.
    ///
    /// The parent is the one this client recorded where it placed the order:
    /// the engine reads no parent from a report, but a client that placed the
    /// order was told. An order it did not place keeps the engine's answer.
    pub(crate) fn perm_and_parent_stated(
        &self,
        order_id: u64,
        report: Option<&crate::bridge::RichOrderInfo>,
        status: Option<&OrderUpdate>,
    ) -> (i64, i64) {
        let (perm_id, engine_parent) = match (report, status) {
            (Some(info), _) if info.order.perm_id != 0 || info.order.parent_id != 0 => {
                (info.order.perm_id, info.order.parent_id)
            }
            (_, Some(u)) => (u.perm_id, u.parent_id),
            (Some(info), None) => (info.order.perm_id, info.order.parent_id),
            (None, None) => (0, 0),
        };
        (perm_id, self.tracked_parent_id(order_id).unwrap_or(engine_parent))
    }

    /// The client that placed an order: this client's record where it placed
    /// it, the report's word otherwise, as that report stated it.
    pub(crate) fn client_stated(
        &self, order_id: u64, report: Option<&crate::bridge::RichOrderInfo>,
    ) -> i32 {
        self.open_orders.lock().unwrap().get(&order_id)
            .map(|t| t.order.client_id)
            .filter(|c| *c != 0)
            .or_else(|| report.map(|info| info.order.client_id))
            .unwrap_or(0)
    }

    /// Which client placed an order, as the venue states it on tag 109.
    ///
    /// Zero where this session has no record of the order, which is also what
    /// the venue states for an order it names no client for.
    pub(crate) fn placing_client(&self, shared: &SharedState, order_id: u64) -> i32 {
        // What this client placed, first. The record here knows the client the
        // order went out under from the moment it went; the venue's book knows
        // nothing about it until the venue names it, so asked of that alone an
        // order this session had just placed was reported under client zero.
        // A record naming no client — a bracket's leg is recorded with none —
        // defers to the venue, which is what zero means here.
        if let Some(client) = self.open_orders.lock().unwrap().get(&order_id)
            .map(|t| t.order.client_id)
            .filter(|c| *c != 0)
        {
            return client;
        }
        // And for an order this client did not place, what the venue says.
        shared.orders.get_order_info(order_id).map_or(0, |info| info.order.client_id)
    }

    /// What tells two contracts on one underlying apart.
    ///
    /// Written on the model now, since it is derived from a contract and needs
    /// nothing of this client's. Kept here because it is the name a program
    /// written against this client already calls.
    pub fn contract_identity(
        last_trade_date: &str, strike: f64, right: &str, multiplier: &str, currency: &str,
    ) -> String {
        crate::types::model::contract_identity(
            last_trade_date, strike, right, multiplier, currency,
        )
    }

    /// Retire the client's record of an order the venue rejected as unknown,
    /// and say what that rejection reports.
    ///
    /// Reason 1 is UnknownOrder: the venue has said the order does not exist,
    /// and the engine has already retired its record. The client's own record
    /// has to go with it, or the open-order snapshot keeps reporting the order
    /// the rejection was about.
    ///
    /// The code says the cancel was refused, and which of the two ways. 202
    /// means an order that was cancelled, so reporting it for a refused cancel
    /// states the opposite and invites a replacement against an order still
    /// working. 10147 means "not found", which is one reason among several.
    pub(crate) fn retire_rejected(&self, reject: &CancelReject) -> (i64, String) {
        if reject.reason_code == 1 {
            self.untrack_order(reject.order_id);
        } else {
            let mut orders = self.open_orders.lock().unwrap();
            if let Some(tracked) = orders.get_mut(&reject.order_id) {
                // The record took the cancel ahead of the venue's answer, and
                // the answer is that the order stands. Left as it was, the
                // order read as leaving for the rest of the session —
                // `req_open_orders` said so — while the venue went on working
                // it, and no later message corrected it, because a refusal is
                // the last thing this order draws. What it goes back to is the
                // engine's own book, not a guess from a status this record has
                // already overwritten.
                if let Some(status) = reject.still_working {
                    tracked.status =
                        crate::types::order_status::order_status_str(status).into();
                }
                // And the terms, where it was the modification that was
                // refused. The record took the attempt ahead of the answer, so
                // a refusal that put back only the status left it stating a
                // price nothing had accepted — and every later cancel and
                // replace restates from the record. Independent of the status
                // above: a refusal from this side of the wire knows the change
                // did not go without knowing where the order stands. A refused
                // cancellation changed no terms, and rolling them back on one
                // undid a replacement the venue may since have taken — and a
                // refusal of a change the venue has already answered is not
                // about the change now outstanding, so it puts nothing back.
                if reject.reject_type == 2 && reject.answers_a_live_change {
                    put_back_the_terms(tracked);
                }
            }
        }
        // 10147 is the order the venue could not find; 10148 is the order it
        // found and would not act on. The reason it stated picks between them.
        let code = if reject.reason_code == 1 { 10147 } else { 10148 };
        let what = if reject.reject_type == 1 { "cancel" } else { "modify" };
        // A refusal from this side of the wire carries no reason of the
        // venue's, and the sentinel that says so is not a reason to hand a
        // caller. Said as a number, "reason: -1" reads as one the venue
        // stated.
        let stated = if reject.reason_code < 0 {
            String::new()
        } else {
            format!(" by the venue (reason: {})", reject.reason_code)
        };
        (code, format!("Order {} {what} rejected{stated}", reject.order_id))
    }

    /// Update a tracked order status from an order update event.
    ///
    /// Takes the pre-stringification `OrderStatus` rather than the ibapi string
    /// so a Rejected transition stays distinct from a genuinely-Inactive one:
    /// both stringify to "Inactive", and only a genuine Inactive is
    /// reactivatable and belongs back in the open-order snapshot.
    ///
    /// Upserts, because an order recovered from an earlier session was never
    /// submitted by this client and so has no entry here. Doing nothing for it
    /// left `collect_open_orders` unable to tell it had just been withdrawn. A fresh
    /// entry seeds contract and order from the same enriched
    /// cache `collect_open_orders` reads, rather than leaving them blank.
    pub fn update_order_status(&self, shared: &SharedState, order_id: u64, status: OrderStatus, filled: f64, remaining: f64, instrument: InstrumentId) {
        self.learn_order_identity(shared, order_id);
        let mut orders = self.open_orders.lock().unwrap();
        let o = orders.entry(order_id).or_insert_with(|| {
            let (contract, order) = match shared.orders.get_order_info(order_id) {
                Some(info) => (info.contract, info.order),
                None => (ApiContract::default(), ApiOrder::default()),
            };
            TrackedOrder {
                contract, order, status: String::new(), rejected: false, filled: 0.0, remaining: 0.0,
                avg_fill_price: 0.0, last_fill_price: 0.0,
                instrument, before_the_replace: None, placed_here: false,
            }
        });
        o.status = order_status_str(status).into();
        o.rejected = status == OrderStatus::Rejected;
        o.filled = filled;
        o.remaining = remaining;
        drop(orders);
        // An order that is done is dropped, the way one that fills already is.
        // Only a fill removed it before, and a fill is not how most orders end:
        // a cancelled or rejected one reports a quantity still outstanding and
        // produces none, so it stayed for the life of the session and the cost
        // of listing what is open grew with every cancel. Filled is here too:
        // a fill removes the record on its own path, but the status can arrive
        // without one, and left standing it reads the id as a working order's
        // — so the next order placed under it becomes a modify of an order
        // that is done. Nothing reads it after this — what is open is
        // answered by status, and these are not open. `Inactive` is not among
        // them: it returns to working when whatever holds the order clears.
        if matches!(status, OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Rejected) {
            self.note_the_number_is_spent(order_id);
            self.untrack_order(order_id);
        }
    }

    /// Collect open orders: merge local tracking with shared state.
    /// Returns (order_id, contract, order, status, filled, remaining) for non-terminal
    /// orders.
    pub fn collect_open_orders(&self, shared: &SharedState) -> Vec<(u64, TrackedOrder)> {
        let mut result: Vec<(u64, TrackedOrder)> = Vec::new();

        // Drain shared order cache first to enrich local tracking
        let shared_orders = shared.orders.drain_open_orders();
        for (wire, _) in &shared_orders { self.learn_order_identity(shared, *wire); }
        {
            let mut orders = self.open_orders.lock().unwrap();
            for (oid, info) in &shared_orders {
                if let Some(o) = orders.get_mut(oid) {
                    // The account the venue states the order is on, which on a
                    // login holding one account is that account whatever the
                    // order named.
                    if !info.order.account.is_empty() {
                        o.order.account = info.order.account.clone();
                    }
                    if o.order.perm_id == 0 {
                        o.order.perm_id = info.order.perm_id;
                    }
                }
            }
        }

        // The orders whose status this client has withdrawn: the venue stated
        // one this client cannot name, and it was passed on as unknown. The
        // cached view can still read working, and the union below re-imported
        // it as such, contradicting the callback the caller had already been
        // given. Scoped to withdrawn statuses so the cache can still carry a
        // genuinely newer one — a terminal local status the cache has since
        // superseded still wins. An order the engine holds in doubt across a
        // drop is not one of these: it keeps the status it was last given.
        let status_withdrawn: std::collections::HashSet<u64> = self.open_orders.lock().unwrap()
            .iter()
            .filter(|(_, o)| o.status == "Unknown")
            .map(|(&id, _)| id)
            .collect();

        // Local tracked orders (non-terminal, or genuinely-Inactive and
        // still reactivatable —), enriched from secdef cache
        {
            // The client the venue names, read into the answer where the
            // record names none, as the fill's is — and into the answer
            // alone: written into the record, what a later replace kept
            // depended on whether a read had happened in between.
            let named_client: HashMap<u64, i32> = shared_orders.iter()
                .map(|(oid, info)| (*oid, info.order.client_id))
                .collect();
            // And what the venue said its fills went at, read into the answer
            // the same way: the record here is what this client sent, and the
            // prices belong to what the venue did with it.
            let stated_fills: HashMap<u64, (f64, f64)> = shared_orders.iter()
                .map(|(oid, info)| (*oid, (info.last_exec.avg_price, info.last_exec.price)))
                .collect();
            let orders = self.open_orders.lock().unwrap();
            for (&oid, o) in orders.iter() {
                // A margin preview states what an order would cost; nothing
                // reaches the book, so it is not among what is working. Its
                // record exists while the question runs and is read as an
                // open order otherwise — exposure the account does not have.
                if o.order.what_if {
                    continue;
                }
                // And not one the venue has already finished. The record
                // here is this client's own account of the order and a
                // terminal report moves it on the dispatch pass, so a caller
                // asking in between was handed an order the venue had
                // finished with — as working, with a quantity outstanding
                // that is not outstanding. What the bridge remembers finishing
                // is the wire's own statement and outranks the local record.
                if shared.orders.recently_completed(oid) {
                    continue;
                }
                if is_open_status(&o.status) || (o.status == "Inactive" && !o.rejected) {
                    let contract = if o.contract.con_id != 0 {
                        self.get_contract(o.contract.con_id, shared).unwrap_or_else(|| o.contract.clone())
                    } else {
                        o.contract.clone()
                    };
                    let mut order = o.order.clone();
                    if order.client_id == 0 {
                        order.client_id = named_client.get(&oid).copied().unwrap_or(0);
                    }
                    let (avg_fill_price, last_fill_price) =
                        stated_fills.get(&oid).copied().unwrap_or((0.0, 0.0));
                    result.push((oid, TrackedOrder {
                        contract,
                        order,
                        status: o.status.clone(),
                        filled: o.filled,
                        remaining: o.remaining,
                        avg_fill_price,
                        last_fill_price,
                        instrument: o.instrument,
                        rejected: o.rejected, before_the_replace: None, placed_here: o.placed_here,
                    }));
                }
            }
        }

        // Add shared-only entries not already present from local
        for (oid, info) in shared_orders {
            if !is_open_or_reactivatable(&info.order_state.status, &info.order_state.completed_status) {
                continue;
            }
            if status_withdrawn.contains(&oid) {
                continue;
            }
            if !result.iter().any(|(id, _)| *id == oid) {
                let contract = if info.contract.con_id != 0 {
                    shared.reference.get_contract(info.contract.con_id).unwrap_or(info.contract)
                } else {
                    info.contract
                };
                // The order carries its own filled quantity; reporting zero
                // here made a partially filled order that this client did not
                // place read as untouched.
                let filled = info.order.filled_quantity;
                let remaining = (info.order.total_quantity - filled).max(0.0);
                result.push((oid, TrackedOrder {
                    contract,
                    order: info.order,
                    status: info.order_state.status.clone(),
                    filled,
                    remaining,
                    avg_fill_price: info.last_exec.avg_price,
                    last_fill_price: info.last_exec.price,
                    instrument: 0,
                    rejected: false, before_the_replace: None, placed_here: false,
                }));
            }
        }

        for (wire, tracked) in &mut result {
            tracked.order.order_id = self.api_order_id(*wire);
            if tracked.order.parent_id > 0 { tracked.order.parent_id = self.api_order_id(tracked.order.parent_id as u64); }
        }
        result
    }

    // ── Dispatch preparation methods ──

    /// Poll quotes for a single instrument and return tick events.
    /// Updates last_quotes internally.
    pub fn poll_instrument_ticks(
        &self,
        shared: &SharedState,
        iid: InstrumentId,
        req_id: i64,
    ) -> QuotePollResult {
        self.ticks_from(shared, Self::poll_quote(shared, iid), req_id)
    }

    /// Take a slot's quote, with the occupancy it was written under and what
    /// rode beside it. The first step of a read; nothing is compared yet.
    pub fn poll_quote(shared: &SharedState, iid: InstrumentId) -> PolledQuote {
        let (quote, generation) = shared.market.quote_with_generation(iid);
        PolledQuote {
            iid,
            generation,
            quote,
            masks: shared.market.quote_attribute_masks(iid),
            series: shared.market.drain_series_ticks(iid),
            snapshot_answer: shared.market.take_snapshot_answer(iid),
        }
    }

    /// The ticks a polled quote makes against what the caller was last told,
    /// the record of what it was told moved on to it.
    pub fn ticks_from(
        &self,
        shared: &SharedState,
        polled: PolledQuote,
        req_id: i64,
    ) -> QuotePollResult {
        let PolledQuote { iid, quote: q, masks, series: stated_series, snapshot_answer, .. } = polled;
        let (eligible_mask, quote_state_mask) = masks;
        let fields = [
            q.bid, q.ask, q.last, q.bid_size, q.ask_size, q.last_size,
            q.high, q.low, q.volume, q.close, q.open, q.timestamp_ns as i64,
            q.bid_exch_mask, q.ask_exch_mask, q.last_exch_mask, q.halted,
        ];

        // Single lock acquisition for both read and write of last_quotes.
        let mut map = self.last_quotes.lock().unwrap();
        let last = map.get(&iid).copied().unwrap_or([0i64; 16]);
        // Numbered as the feed this instrument is subscribed to: a delayed feed
        // goes out under the delayed numbers, which is what the caller was
        // told to expect on `market_data_type`.
        let delayed = matches!(
            self.mdt_by_instrument.lock().unwrap().get(&iid),
            Some(&MDT_DELAYED) | Some(&MDT_DELAYED_FROZEN)
        );
        let numbered = |tick_type: i32| if delayed { as_delayed(tick_type) } else { tick_type };

        let mut ticks = Vec::new();
        let mut delivered = false;

        // Price ticks: (field_index, tick_type)
        const PRICE_TICKS: &[(usize, i32)] = &[
            (0, TICK_BID), (1, TICK_ASK), (2, TICK_LAST),
            (6, TICK_HIGH), (7, TICK_LOW), (9, TICK_CLOSE), (10, TICK_OPEN),
        ];
        for &(idx, tt) in PRICE_TICKS {
            if fields[idx] != last[idx] {
                ticks.push(TickEvent {
                    req_id, tick_type: numbered(tt),
                    value: fields[idx] as f64 / PRICE_SCALE_F,
                    is_price: true,
                });
                delivered = true;
            }
        }

        // Size ticks: (field_index, tick_type)
        const SIZE_TICKS: &[(usize, i32)] = &[
            (3, TICK_BID_SIZE), (4, TICK_ASK_SIZE), (5, TICK_LAST_SIZE), (8, TICK_VOLUME),
        ];
        for &(idx, tt) in SIZE_TICKS {
            if fields[idx] != last[idx] {
                ticks.push(TickEvent {
                    req_id, tick_type: numbered(tt),
                    value: fields[idx] as f64 / QTY_SCALE as f64,
                    is_price: false,
                });
                delivered = true;
            }
        }

        // Timestamp tick
        let timestamp = if fields[11] != last[11] && fields[11] != 0 {
            Some(TimestampTick { req_id, timestamp_ns: fields[11] })
        } else {
            None
        };

        // Exchange-code string ticks, rendered from the contract's own map of
        // venues. Emit a delta record
        // when the bitmask changes; dispatch resolves the letter string.
        let mut string_ticks = Vec::new();
        // What is cached for this instrument, which is the quote as it stands
        // except where a field could not be rendered yet.
        let mut cached = fields;
        const EXCH_TICKS: &[(usize, i32)] = &[
            (12, TICK_BID_EXCHANGE), (13, TICK_ASK_EXCHANGE), (14, TICK_LAST_EXCHANGE),
        ];
        for &(idx, tt) in EXCH_TICKS {
            if fields[idx] != last[idx] {
                let letters = render_exchange_mask(fields[idx], iid, shared);
                // A mask with bits set and no letters to show for them is
                // one the venue has not named its exchanges for yet. Caching
                // it as delivered leaves it equal to the next mask, so it is
                // never rendered again once the names arrive and the quote's
                // exchange is lost for the life of the subscription.
                if letters.is_empty() && fields[idx] != 0 {
                    cached[idx] = last[idx];
                    continue;
                }
                string_ticks.push(StringTickEvent {
                    req_id, tick_type: tt, value: letters,
                });
                delivered = true;
            }
        }

        // A halt changes what every other tick in this quote means: the prices
        // standing are the ones from before the venue stopped, not a market
        // anyone can deal on. It arrives on the trading-status tick and was
        // written into the quote, compared here, and then cached without being
        // sent anywhere — so the one transition worth hearing about was
        // consumed and could never be delivered again.
        //
        // The venue states it under a number of its own, so it goes out on
        // `tick_generic` as the reference client delivers it.
        let mut generic_ticks = Vec::new();
        if fields[15] != last[15] {
            generic_ticks.push(TickEvent {
                req_id, tick_type: numbered(TICK_HALTED),
                value: fields[15] as f64,
                is_price: false,
            });
            delivered = true;
        }

        map.insert(iid, cached);
        drop(map);

        // The extra series the caller asked for. They arrive on records of
        // their own rather than as quote fields, so they are queued as they
        // are decoded and handed over here, beside the quote they were asked
        // for alongside. Each already knows which of the four callbacks
        // carries it, because the venue's own record says.
        for series in stated_series {
            match series.value {
                // A yield is numbered as the feed is, the way the prices it
                // travels with are: a delayed feed's go out on 103 and 104.
                // Every other series keeps the number it was published under.
                crate::types::SeriesValue::Price(_)
                    if delayed && series.tick_type == DELAYED_LAST_YIELD_UNSENT =>
                {
                    shared.market.note_unread_wire_under(
                        "market data",
                        "a delayed last yield".to_string(),
                        "a last yield on a delayed feed, which has no number to be delivered \
                         under"
                            .to_string(),
                    );
                    continue;
                }
                crate::types::SeriesValue::Price(value) => ticks.push(TickEvent {
                    req_id,
                    tick_type: if matches!(series.tick_type, 50 | 51) {
                        numbered(series.tick_type)
                    } else {
                        series.tick_type
                    },
                    value,
                    is_price: true,
                }),
                crate::types::SeriesValue::Size(value) => ticks.push(TickEvent {
                    req_id, tick_type: series.tick_type, value, is_price: false,
                }),
                crate::types::SeriesValue::Generic(value) => generic_ticks.push(TickEvent {
                    req_id, tick_type: series.tick_type, value, is_price: false,
                }),
                crate::types::SeriesValue::Text(value) => string_ticks.push(StringTickEvent {
                    req_id, tick_type: series.tick_type, value,
                }),
            }
            delivered = true;
        }

        // The venue's answer to a chargeable snapshot is one message, handed to
        // the snapshot's own requests and to no stream watching the same
        // contract, and a gateway ends the snapshot as soon as it has
        // published it. Published under the numbers a gateway's snapshot
        // carries, which are not renumbered for a delayed feed.
        let mut snapshot_ticks = Vec::new();
        let mut snapshot_strings = Vec::new();
        if let Some(answer) = snapshot_answer {
            let one_shots = self.chargeable_snapshot_reqs.lock().unwrap().clone();
            let asking: Vec<i64> = self.watchers_of(iid).into_iter()
                .filter(|id| one_shots.contains(id))
                .collect();
            let mut waiting = self.snapshot_reqs.lock().unwrap();
            for id in asking {
                for said in &answer {
                    match &said.value {
                        crate::types::SeriesValue::Price(value) => snapshot_ticks.push(TickEvent {
                            req_id: id, tick_type: said.tick_type, value: *value, is_price: true,
                        }),
                        crate::types::SeriesValue::Size(value) => snapshot_ticks.push(TickEvent {
                            req_id: id, tick_type: said.tick_type, value: *value, is_price: false,
                        }),
                        crate::types::SeriesValue::Text(value) => {
                            snapshot_strings.push(StringTickEvent {
                                req_id: id, tick_type: said.tick_type, value: value.clone(),
                            })
                        }
                        crate::types::SeriesValue::Generic(_) => {}
                    }
                }
                // Its answer is the whole of it.
                if let Some(wait) = waiting.get_mut(&id) {
                    wait.stated = u16::MAX;
                }
            }
        }

        QuotePollResult {
            delayed, ticks, generic_ticks, string_ticks, timestamp, delivered,
            snapshot_ticks, snapshot_strings,
            eligible_mask,
            quote_state_mask,
        }
    }

    /// Whether a snapshot has just finished arriving.
    ///
    /// Answers true once, for the pump that sees the snapshot complete. A
    /// request that is not a snapshot is never one of these.
    ///
    /// Note that the venue has stated one of the kinds a snapshot is made of.
    ///
    /// What counts is that a tick of the kind ARRIVED, not what it carried: a
    /// currency pair states its last as minus one and a contract that has not
    /// opened states its open as nothing, and both of those are the venue
    /// answering. Waiting for a figure above zero instead waits out the clock
    /// on every one of them.
    pub fn note_snapshot_tick(&self, req_id: i64, tick_type: i32) {
        // Numbered as the feed the request was made under, which is what the
        // caller was told to expect: a delayed or frozen subscription states
        // its bid, ask, last, close and open under numbers of their own. Only
        // the realtime ones were read here, so a snapshot on either of those
        // feeds could never be completed by anything the venue said — it ran to
        // the sweep every time, however promptly the venue answered.
        let bit = match tick_type {
            1 | 66 => 1u16,  // bid
            2 | 67 => 2,     // ask
            4 | 68 => 4,     // last
            14 | 76 => 8,    // open
            9 | 75 => 16,    // close
            88 => SNAPSHOT_DELAYED_TIME,
            _ => return,
        };
        if let Some(wait) = self.snapshot_reqs.lock().unwrap().get_mut(&req_id) {
            wait.stated |= bit;
        }
    }

    /// A snapshot ends when it has been sent every kind one is made of, or
    /// when long enough has passed since it was asked for: `None` while it
    /// runs, and at its end the option computations it is sent then.
    ///
    /// Both are a gateway's: it holds a snapshot until the bid, the ask, the
    /// last, the open and the close have each been delivered, and sweeps
    /// anything still waiting eleven seconds after the REQUEST — not eleven
    /// since the last thing heard. On a contract it marks as an option it also
    /// waits for the option model (13, or 83 on a delayed feed) and the bid's,
    /// the ask's and the last's computations (10 to 12, or 80 to 82), and on a
    /// delayed feed for the last trade's time (88).
    ///
    /// At the end it sends a snapshot each option computation it has not been
    /// sent, as it last stood for that request, where any figure is stated:
    /// the bid's, the ask's, the last's, then the model's, each with its
    /// number and whether it was worked from prices. On a frozen or a
    /// delayed-frozen feed a side is sent then only stating every figure, and
    /// the model also where it was not sent on the terms of such a feed,
    /// whatever it states.
    ///
    /// Waiting on the quiet instead, as this did, ends a snapshot on a pause
    /// rather than on an answer, and a contract the venue never says anything
    /// about was never swept at all: the clock only started on the first
    /// delivery, so one that got none waited for ever.
    pub fn check_snapshot_done(&self, req_id: i64) -> Option<Vec<(i32, [f64; 8], bool)>> {
        use crate::bridge::OptionTickKind::{Ask, Bid, Last, Model};
        /// How long after asking the reference client gives up waiting for the
        /// rest of a snapshot.
        const GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(11);

        let delayed;
        let mut wait = {
            let mut waiting = self.snapshot_reqs.lock().unwrap();
            let wait = waiting.get(&req_id).copied()?;
            delayed = self.feed_is_delayed(wait.slot);
            let mut whole = SNAPSHOT_WHOLE;
            if wait.marked {
                whole |= SNAPSHOT_MODEL | SNAPSHOT_SIDES;
            }
            if delayed {
                whole |= SNAPSHOT_DELAYED_TIME;
            }
            if wait.stated & whole != whole && wait.asked_at.elapsed() < GIVE_UP_AFTER {
                return None;
            }
            waiting.remove(&req_id);
            wait
        };
        if !wait.marked {
            return Some(Vec::new());
        }
        let price_based = self.models_as_they_stand.lock().unwrap()
            .get(&wait.slot)
            .is_some_and(|(_, model)| model.price_based);
        let feed = self.feed_of(wait.slot);
        let frozen = matches!(feed, MDT_FROZEN | MDT_DELAYED_FROZEN);
        let sent = self.option_ticks_sent.lock().unwrap();
        let any_stated = |figures: &[f64; 8]| figures.iter().any(|figure| *figure != f64::MAX);
        let mut owed: Vec<(i32, [f64; 8], bool)> = [Bid, Ask, Last].into_iter()
            .filter(|kind| wait.stated & snapshot_bit(*kind) == 0)
            .filter_map(|kind| {
                let figures = sent.get(&(req_id, kind))
                    .filter(|figures| if frozen { every_figure_stated(figures) } else { any_stated(figures) })?;
                Some((option_tick_type(kind, delayed), *figures, false))
            })
            .collect();
        let model = sent.get(&(req_id, Model)).copied().unwrap_or([f64::MAX; 8]);
        if (wait.stated & SNAPSHOT_MODEL == 0 && any_stated(&model)) || frozen_model_owed(&mut wait, feed) {
            owed.push((option_tick_type(Model, delayed), model, price_based));
        }
        Some(owed)
    }

    /// Snapshot the current instrument→req_id mapping, each with every other
    /// request watching that contract.
    ///
    /// Both under one acquisition. Read apart — the holders here, the watchers
    /// again per contract as each quote is delivered — a withdrawal running in
    /// between left a quote going to the request that had just handed the
    /// subscription on and not to the one that had just taken it.
    pub fn snapshot_instruments(&self) -> Vec<(InstrumentId, i64, Vec<i64>)> {
        let own = self.ownership();
        own.holders
            .iter()
            .map(|(&iid, &req_id)| {
                (iid, req_id, own.following.get(&iid).cloned().unwrap_or_default())
            })
            .collect()
    }

    /// A slot is held from here under `generation`, as the engine's record of
    /// it says. Written where that record is delivered.
    pub fn note_slot_taken(&self, slot: InstrumentId, generation: u64) {
        self.slot_generation.lock().unwrap().insert(slot, generation);
    }

    /// The occupancy a slot was held under has ended.
    pub fn note_slot_released(&self, slot: InstrumentId, generation: u64) {
        let mut held = self.slot_generation.lock().unwrap();
        if held.get(&slot) == Some(&generation) {
            held.remove(&slot);
        }
    }

    /// The occupancy a slot is held under as this side has read it; nought
    /// where nothing has named one.
    pub fn generation_held(&self, slot: InstrumentId) -> u64 {
        self.slot_generation.lock().unwrap().get(&slot).copied().unwrap_or(0)
    }

    /// Take the conflated state, as a read's first step: every held slot's
    /// quote, and each figure that has moved since the caller was last told.
    ///
    /// Taken before the read's cut, so a change that followed a record's push
    /// is delivered no earlier than that record. `positions_watched` says
    /// whether anyone is watching the holdings, and `multi_watchers` which
    /// multi-account requests stand: nothing is drained that nobody reads.
    pub fn poll_conflated(
        &self, shared: &SharedState, positions_watched: bool, multi_watchers: &[i64],
    ) -> Polled {
        let quotes = self
            .snapshot_instruments()
            .into_iter()
            .map(|(iid, ..)| Self::poll_quote(shared, iid))
            .collect();
        let positions = if positions_watched {
            shared.portfolio.drain_position_changes()
        } else {
            Vec::new()
        };
        let account = self.prepare_account_updates(shared);
        let portfolio = if account.is_some() {
            self.prepare_portfolio_updates(shared)
        } else {
            Vec::new()
        };
        let multi = multi_watchers
            .iter()
            .map(|req_id| (*req_id, self.account_figures_that_moved(shared, *req_id)))
            .filter(|(_, moved)| !moved.is_empty())
            .collect();
        // One batch per summary due. Two may be open at once.
        let mut summaries = Vec::new();
        while summaries.len() < 2
            && let Some(batch) = self.prepare_account_summary(shared, "")
        {
            summaries.push(batch);
        }
        Polled {
            quotes,
            positions,
            named_positions: if positions_watched {
                shared.account_portfolios().into_iter().filter(|(_, p)| !std::sync::Arc::ptr_eq(p, &shared.portfolio))
                    .flat_map(|(a, p)| p.drain_position_changes().into_iter().map(move |pi| (a.clone(), pi))).collect()
            } else { Vec::new() },
            pnl: self.poll_pnl(shared),
            pnl_single: self.poll_pnl_single(shared),
            account,
            portfolio,
            multi,
            summaries,
            smart_components: shared
                .reference
                .drain_smart_component_answers(std::time::Instant::now()),
        }
    }

    /// What the venue last marked a contract at, which is its price at
    /// midnight rather than its price now. Read at the point of use; text that
    /// is not a usable price leaves the contract unmarked, rather than valuing
    /// it at whatever the characters happened to come to.
    fn midnight_price(portfolio: &crate::bridge::PortfolioState, con_id: i64) -> Option<Price> {
        let raw = portfolio.venue_price(con_id)?;
        let price = raw.trim().parse::<f64>().ok().filter(|p| p.is_finite())?;
        Some(crate::types::price_from_f64(price)).filter(|&p| p != 0)
    }

    /// Poll PnL and return update if values changed.
    /// Formula: dailyPnL = Σ(qtyNow × priceNow - valueAtMidnight + moneyTraded),
    /// where the venue states the midnight value and the price it last marked
    /// the contract at, and the client falls back to qtyMidnight × prevClose
    /// and a live quote for whatever the venue did not state.
    /// For positions opened intraday (no seed), synthesizes
    /// moneyTraded = -qtyNow × avgCost so the formula collapses to unrealized P&L.
    pub fn poll_pnl(&self, shared: &SharedState) -> Vec<PnlUpdate> {
        let requests = self.pnl_req_id.lock().unwrap().clone();
        requests.into_iter().filter_map(|(req_id, account)| self.poll_pnl_request(shared, req_id, &account)).collect()
    }

    fn poll_pnl_request(&self, shared: &SharedState, req_id: i64, account: &str) -> Option<PnlUpdate> {
        let portfolio = shared.portfolio_for(account);
        // Nothing until the venue has stated the account whole on this
        // connection. A trading-connection drop leaves the quotes flowing
        // while the book is stale, and the sum below multiplied the pre-drop
        // quantities by live prices on every tick: a holding the account
        // closed during the outage went on being valued, and its profit
        // reported, until the download arrived.
        if !portfolio.account_download_complete() {
            return None;
        }

        let seeds: HashMap<i64, MidnightSeed> = portfolio.midnight_seeds()
            .into_iter().map(|s| (s.con_id, s)).collect();
        let positions = portfolio.position_infos();

        let mut con_ids: HashSet<i64> = seeds.keys().copied().collect();
        for pi in &positions {
            con_ids.insert(pi.con_id);
        }
        // An account holding nothing still has a P&L: what it realised today
        // is already in the venue's figures. Returning here reported
        // nothing at all to a caller that had asked to be told.

        self.forget_released_slots(shared);
        let con_id_map = self.con_id_to_instrument.lock().unwrap();
        let mut total_daily: f64 = 0.0;
        let mut total_unrealized: f64 = 0.0;
        let mut total_realized: f64 = 0.0;
        let mut priced = 0usize;
        let mut unpriceable = 0usize;

        for con_id in con_ids {
            let seed = seeds.get(&con_id);
            let pi = portfolio.position_info(con_id);

            // Realized P&L is stated outright by the row and does not depend on
            // knowing either quantity, so it accrues before the guards below.
            total_realized += seed.map(|s| s.realized_pnl).unwrap_or(0.0);

            // A position held at midnight and not currently sizeable is not
            // a flat one: pricing the absence as zero reports the whole
            // overnight holding as sold. It is counted as unpriceable rather
            // than merely skipped, because a total missing one position is not
            // a smaller correct answer.
            if seed.is_some() && pi.is_none() {
                unpriceable += 1;
                continue;
            }
            let qty_now = pi.as_ref().map(|p| p.position).unwrap_or(0.0);
            let avg_cost = pi.as_ref().map(|p| p.avg_cost).unwrap_or(0);
            // Likewise for the overnight leg: a seed row whose quantity did not
            // parse says the position is not intraday, only that its size is
            // unknown, so there is nothing to price it against.
            let Some(qty_midnight) = seed.map_or(Some(0.0), |s| s.qty_midnight) else {
                unpriceable += 1;
                continue;
            };

            // A price is per unit and a contract may be worth many of them.
            // Multiplied by nothing, an option holding was valued at a
            // hundredth of what it is worth and the account total with it, so
            // such a position is left to the venue's figures rather than
            // valued from a price this arithmetic cannot use.
            let carries_multiplier = pi.as_ref().is_some_and(position_is_multiplied);
            let has_size = qty_now != 0.0 || qty_midnight != 0.0;
            if carries_multiplier && has_size {
                unpriceable += 1;
                continue;
            }

            let quote = con_id_map.get(&con_id).map(|&iid| shared.market.quote(iid));
            let prev_close = quote.map_or(0, |q| q.close);
            let Some(price_now) = quote.map(|q| q.last).filter(|&p| p != 0) else {
                // A position with size and no live price is one this total
                // is missing, which is what `unpriceable` counts. Skipping it
                // without counting reports the rest of the account as the
                // whole of it.
                if has_size {
                    unpriceable += 1;
                }
                continue;
            };
            // What the position was worth at midnight. The venue states the
            // mark it closed the contract at, and that is what the overnight
            // leg is valued against; a locally derived previous close is used
            // only where the venue said nothing.
            let prev_close = Self::midnight_price(&portfolio, con_id).unwrap_or(prev_close);
            if seed.and_then(|s| s.cost_midnight).is_none() && prev_close == 0 && qty_midnight != 0.0 {
                // Nothing to value the overnight leg against, so this position
                // is missing from the total too.
                unpriceable += 1;
                continue;
            }

            // A position with no seed row and no cost cannot be sized at all.
            // The opening cash synthesized below is `-qty*avgCost`, which is
            // nought where the cost is unknown, and the midnight value is
            // nought too because there is nothing to seed it from — so the
            // whole of what the position is worth now was booked as the day's
            // profit. The feed states a cost often rather than always, so this
            // is reached with the venue's own rows. Counted as one that could
            // not be priced, which is what it is: the unrealized sum below
            // already declines to use an unknown basis, and counting it as
            // priced kept the venue's own figures from standing in.
            if seed.is_none() && avg_cost == 0 {
                unpriceable += 1;
                continue;
            }

            // moneyTradedSinceMidnight (wire 6822) is signed net cash: SELL
            // positive, BUY negative. An intraday-only position
            // has no seed row, so synthesize the opening trade's net cash:
            // -qty*avgCost (cash paid to open a long, received to open a short).
            let money_traded = match seed {
                Some(s) => s.money_traded,
                None => -(qty_now * avg_cost as f64 / PRICE_SCALE_F),
            };

            let mv_now = qty_now * price_now as f64 / PRICE_SCALE_F;
            // The venue states what the position was worth at midnight. That
            // beats sizing the overnight leg against a previous close the
            // client has to find for itself, which it has for no contract it
            // never quoted.
            let mv_midnight = seed.and_then(|s| s.cost_midnight)
                .unwrap_or_else(|| qty_midnight * prev_close as f64 / PRICE_SCALE_F);
            // Daily P&L = value change since midnight plus today's net cash.
            total_daily += mv_now - mv_midnight + money_traded;

            if avg_cost != 0 {
                total_unrealized +=
                    qty_now * price_now.saturating_sub(avg_cost) as f64 / PRICE_SCALE_F;
            }
            priced += 1;
        }

        // No position carried a live quote (a req_pnl-only client never populates
        // con_id_to_instrument, so every position above hits `continue`). Fall back
        // to the venue's account-level P&L, which the venue pushes independently
        // of any market-data subscription. Without this the quote-derived totals stay
        // [0,0,0] and no callback ever fires.
        // A position that could not be priced makes the client-side sum an
        // incomplete account total, not a smaller correct one — and the realized
        // figure has already accrued for it, so the three would not even agree
        // with each other. The venue's account-level numbers are complete
        // by construction, so one unpriceable position sends the whole account
        // to them rather than reporting a partial sum as if it were the total.
        if priced == 0 || unpriceable > 0 {
            let acct = portfolio.account();
            total_daily = acct.daily_pnl as f64 / PRICE_SCALE_F;
            total_unrealized = acct.unrealized_pnl as f64 / PRICE_SCALE_F;
            total_realized = acct.realized_pnl as f64 / PRICE_SCALE_F;
        }

        let pnl = [
            crate::types::price_from_f64(total_daily),
            crate::types::price_from_f64(total_unrealized),
            crate::types::price_from_f64(total_realized),
        ];
        let mut last = self.last_pnl.lock().unwrap();
        if last.get(&req_id) == Some(&pnl) {
            return None;
        }
        last.insert(req_id, pnl);
        Some(PnlUpdate {
            req_id,
            daily_pnl: total_daily,
            unrealized_pnl: total_unrealized,
            realized_pnl: total_realized,
        })
    }

    /// Poll per-position PnL and return updates whose values changed.
    /// Routes the quote lookup by con_id (not first-non-zero across all subscriptions),
    /// computes daily/realized from the matching midnight seed, and synthesizes
    /// money_traded = qty_now × avg_cost for intraday-opened positions.
    pub fn poll_pnl_single(&self, shared: &SharedState) -> Vec<PnlSingleUpdate> {
        let reqs = self.pnl_single_reqs.lock().unwrap().clone();
        // As for the account's own profit: nothing from a book the download
        // has not restated on this connection.
        if reqs.is_empty() {
            return Vec::new();
        }

        self.forget_released_slots(shared);
        let con_id_map = self.con_id_to_instrument.lock().unwrap();
        let mut last_cache = self.last_pnl_single.lock().unwrap();
        let mut results = Vec::new();

        for (req_id, (account, con_id)) in reqs {
            let portfolio = shared.portfolio_for(&account);
            if !portfolio.account_download_complete() { continue; }
            let seeds: HashMap<i64, MidnightSeed> = portfolio.midnight_seeds()
                .into_iter().map(|s| (s.con_id, s)).collect();
            let Some(pi) = portfolio.position_info(con_id) else { continue; };
            let qty_now = pi.position;
            let avg_cost = pi.avg_cost;

            let quote = con_id_map.get(&con_id).map(|&iid| shared.market.quote(iid));
            // The venue's mark for the position, stated whether or not
            // anything here subscribed to the contract. A quote is per unit
            // and a contract may be worth many of them, so an option or a
            // future is valued from the venue's figure rather than from a
            // price this arithmetic cannot use. This subscription does not
            // depend on a market-data subscription.
            let stated_mark = (pi.market_price != 0).then_some(pi.market_price);
            let price_now = match quote.map(|q| q.last).filter(|&p| p != 0) {
                Some(live) if !position_is_multiplied(&pi) => live,
                _ => match stated_mark {
                    Some(mark) => mark,
                    None => continue,
                },
            };
            let unit_value = if pi.market_value != 0 {
                pi.market_value as f64 / PRICE_SCALE_F
            } else if qty_now != 0.0 && position_is_multiplied(&pi) {
                // The venue states a value that already carries the contract's
                // multiplier and a price that does not. With no value stated,
                // multiplying the quantity by the price alone values a
                // multiplied contract at a hundredth of what it is worth — and
                // that is the branch an update carrying only the price takes,
                // because an absent value reads as zero. Both neighbours here
                // test for this; this one did not.
                //
                // Only where something is held. A position closed today keeps
                // its row, and its value really is nothing — skipped for
                // carrying a multiplier, the caller would never hear the close
                // or the realized figure that came with it.
                continue;
            } else {
                qty_now * price_now as f64 / PRICE_SCALE_F
            };
            let seed = seeds.get(&con_id);
            // An unparseable overnight quantity leaves nothing to price the
            // day's change against. Unlike the whole-account total, that costs
            // this callback only its daily figure: the position, its value, the
            // unrealized and the realized are all still known, and suppressing
            // the callback would leave every one of them stale on the caller's
            // side rather than reporting one it cannot compute.
            let qty_midnight = seed.map_or(Some(0.0), |s| s.qty_midnight);
            // As in poll_pnl, the venue's mark is what the overnight leg is
            // valued against, so the two callbacks value the same position from
            // the same figures.
            let prev_close = Self::midnight_price(&portfolio, con_id)
                .unwrap_or_else(|| quote.map_or(0, |q| q.close));
            // What the venue says the position was worth at midnight, which the
            // client otherwise has to size from the overnight quantity and a
            // previous close it may not hold.
            let stated_midnight = seed.and_then(|s| s.cost_midnight);
            if stated_midnight.is_none() && prev_close == 0 && qty_midnight.unwrap_or(0.0) != 0.0 {
                continue;
            }

            // moneyTradedSinceMidnight (wire 6822) is signed net cash: SELL
            // positive, BUY negative. Synthesize the opening
            // trade's net cash for an intraday-only position (no seed row).
            let money_traded = match seed {
                Some(s) => s.money_traded,
                None => -(qty_now * avg_cost as f64 / PRICE_SCALE_F),
            };

            let mv_now = unit_value;
            // Held at the value last reported when the overnight size is
            // unknown, rather than recomputed from an assumption that would be
            // wrong in a specific direction: treating the absence as flat
            // reports the whole holding as sold, and treating it as no seed at
            // all reports the day's move as the position's entire unrealized.
            //
            // And sized only where a price can size it. What is subtracted
            // here is a value the venue stated, which already carries the
            // contract's multiplier; the overnight quantity times a previous
            // close does not, so on an option or a future the day's change
            // came out wrong by the multiplier — a hundredfold on an equity
            // option. The two neighbours that do the same arithmetic test for
            // this; with a multiplier on the row and no value stated for it,
            // there is nothing here that can be used, and the arm below holds
            // what was last reported.
            //
            // Nothing held overnight is the exception, and it is not a
            // rounding one: a position opened today was worth nothing at
            // midnight whatever it is worth a unit of, and no multiplier can
            // change that. Refused along with the rest, every intraday option
            // and future reported no day's profit at all — and reported it for
            // as long as the session ran, because what the arm below holds is
            // then that nought.
            let midnight_value = stated_midnight.or_else(|| {
                qty_midnight
                    .filter(|&q| q == 0.0 || !position_is_multiplied(&pi))
                    .map(|q| q * prev_close as f64 / PRICE_SCALE_F)
            });
            // A position with no seed row and no cost is in the same case as an
            // unknown overnight size: the opening cash synthesized above is
            // nought where the cost is unknown, and the midnight value is
            // nought because there is nothing to seed it from, so the whole of
            // what the position is worth now would go out as the day's profit.
            // The feed states a cost often rather than always. Held at what was
            // last reported, as the arm below already holds it.
            let basis_unknown = seed.is_none() && avg_cost == 0;
            let daily = match midnight_value {
                Some(mv_midnight) if !basis_unknown => mv_now - mv_midnight + money_traded,
                _ => last_cache.get(&req_id)
                    .map_or(0.0, |prev| prev[1] as f64 / PRICE_SCALE_F),
            };
            // The venue states what the position has made and not realised,
            // and it is the only figure that is right for a contract worth
            // more than one unit of its own price.
            let unrealized = if pi.unrealized_stated {
                pi.unrealized_pnl as f64 / PRICE_SCALE_F
            } else if avg_cost != 0 && !position_is_multiplied(&pi) {
                qty_now * price_now.saturating_sub(avg_cost) as f64 / PRICE_SCALE_F
            } else { 0.0 };
            let realized = seed.map(|s| s.realized_pnl).unwrap_or(0.0);
            let value = mv_now;

            // The quantity whole, fractions included: held as a whole number,
            // a holding that moved inside one unit read as unchanged.
            let snapshot: [i64; 5] = [
                crate::types::qty_from_f64(qty_now),
                crate::types::price_from_f64(daily),
                crate::types::price_from_f64(unrealized),
                crate::types::price_from_f64(realized),
                crate::types::price_from_f64(value),
            ];
            if last_cache.get(&req_id) == Some(&snapshot) {
                continue;
            }
            last_cache.insert(req_id, snapshot);

            results.push(PnlSingleUpdate {
                req_id,
                pos: qty_now,
                daily_pnl: daily,
                unrealized_pnl: unrealized,
                realized_pnl: realized,
                value,
            });
        }
        results
    }

    /// The account figures to deliver, as the venue stated them.
    ///
    /// Built from the venue's statements rather than from this client's
    /// typed copy, which exists before the venue has stated anything and would
    /// otherwise report every figure as zero in no currency.
    ///
    /// Each figure is delivered once and again whenever it changes, per
    /// currency: a figure stated in two currencies is two figures.
    pub fn prepare_account_updates(&self, shared: &SharedState) -> Option<AccountUpdateBatch> {
        let portfolio = shared.portfolio_for(&self.account_routes.lock().unwrap().updates);
        if !self.account_updates_subscribed.load(Ordering::Acquire) {
            return None;
        }
        let stated = portfolio.stated_account_values();
        if stated.is_empty() {
            return None;
        }

        let mut already = self.last_stated_account.lock().unwrap();
        let mut fields = Vec::new();
        for (ledger, key, value, currency) in stated {
            let held = already.get(&(ledger, key.clone(), currency.clone()));
            if held.map(String::as_str) == Some(value.as_str()) {
                continue;
            }
            already.insert((ledger, key.clone(), currency.clone()), value.clone());
            fields.push(AccountFieldUpdate { key, value, currency });
        }

        // Said once, when the venue ends the batch it was sending. Timed out
        // of a quiet spell instead, an account still arriving was called fully
        // stated because it paused, and one that finished early waited on a
        // clock for permission to say so.
        // On the rising edge, not once for the life of the client. A rebuilt
        // connection states the account again and ends that batch too, and a
        // subscriber that had already been told once was never told again --
        // the second download completed in silence, so a caller waiting on the
        // end to know the book is whole waited for ever.
        // The swap runs whichever way the flag reads, so the falling edge is
        // observed: written with the test first, `&&` short-circuits while a
        // download is running, the latch is never cleared, and the end is
        // still said only once for the life of the client.
        let complete = portfolio.account_download_complete();
        let finished = !self.account_end_sent.swap(complete, Ordering::AcqRel) && complete;
        Some(AccountUpdateBatch { fields, finished })
    }

    /// What has changed of the account's figures since the multi-account
    /// subscription last heard, in the currency the venue states each in.
    ///
    /// The reference client keeps this request open: a figure that moves after
    /// the first batch is reported again under the same request, until the
    /// caller withdraws it. Answered with the first batch alone, a caller
    /// watching its balance sheet through this request watched a still
    /// picture — and every one of them was written against a client that keeps
    /// it moving.
    ///
    /// Answered against what this request has been told, which is what makes
    /// the first batch the account whole and every batch after it the moves.
    ///
    /// A request that asked for the ledger and net liquidation alone is given
    /// what the per-currency ledger states and nothing else, as a gateway
    /// gives it: the net liquidation it means is the ledger's own, per
    /// currency, and the account's other figures are not delivered.
    pub fn account_figures_that_moved(
        &self, shared: &SharedState, req_id: i64,
    ) -> Vec<AccountFieldUpdate> {
        let portfolio = shared.portfolio_for(&self.multi_account(shared, req_id));
        let ledger_only = self.ledger_only_multi.lock().unwrap().contains(&req_id);
        let mut held = self.last_stated_account_multi.lock().unwrap();
        let already = held.entry(req_id).or_default();
        let mut moved = Vec::new();
        for (ledger, key, value, currency) in portfolio.stated_account_values() {
            if ledger_only && !ledger {
                continue;
            }
            if already.get(&(ledger, key.clone(), currency.clone())).map(String::as_str)
                == Some(value.as_str())
            {
                continue;
            }
            already.insert((ledger, key.clone(), currency.clone()), value.clone());
            moved.push(AccountFieldUpdate { key, value, currency });
        }
        moved
    }

    /// Forget what a request has been told, so the next ask under it is
    /// answered with the account whole, and a withdrawn one keeps nothing.
    pub fn forget_account_figures_for(&self, req_id: i64) {
        self.last_stated_account_multi.lock().unwrap().remove(&req_id);
    }

    /// Whether a multi-account request asked for the ledger and net
    /// liquidation alone. Stated before the request is watched, so no figure
    /// outside the ledger reaches it in between.
    pub fn ledger_only_for(&self, req_id: i64, ledger_and_nlv: bool) {
        let mut asked = self.ledger_only_multi.lock().unwrap();
        if ledger_and_nlv { asked.insert(req_id); } else { asked.remove(&req_id); }
    }

    /// Prepare portfolio updates (position entries) for account streaming.
    /// Returns changed/new position infos when account updates are subscribed.
    pub fn prepare_portfolio_updates(&self, shared: &SharedState) -> Vec<PortfolioUpdateEntry> {
        let portfolio = shared.portfolio_for(&self.account_routes.lock().unwrap().updates);
        if !self.account_updates_subscribed.load(Ordering::Acquire) {
            return Vec::new();
        }
        // On the download being finished, not on anything having been heard.
        // A drop zeroes every row's marks deliberately, and the first figure
        // of the rebuilt connection raised the older flag -- so every holding
        // went out at a price of nothing, a value of nothing and no profit,
        // including any the account had closed while the connection was down.
        // A caller summing what it was worth read zero exposure until the
        // venue got round to pricing them again.
        if !portfolio.account_download_complete() {
            return Vec::new();
        }

        let current = portfolio.position_infos();
        let mut prev_guard = self.last_portfolio.lock().unwrap();
        let is_first = prev_guard.is_none();

        let to_entry = |pi: &PositionInfo| PortfolioUpdateEntry {
            con_id: pi.con_id,
            position: pi.position,
            avg_cost: pi.avg_cost as f64 / PRICE_SCALE_F,
            market_price: pi.market_price as f64 / PRICE_SCALE_F,
            market_value: pi.market_value as f64 / PRICE_SCALE_F,
            unrealized_pnl: pi.unrealized_pnl as f64 / PRICE_SCALE_F,
            realized_pnl: pi.realized_pnl as f64 / PRICE_SCALE_F,
        };

        let changed = if is_first {
            current.iter().map(&to_entry).collect()
        } else {
            let prev = prev_guard.as_ref().unwrap();
            // Marks are part of the row: a mark move (each account-updates
            // snapshot) is a genuine update, so compare them too.
            current.iter().filter(|pi| {
                !prev.iter().any(|pp| pp.con_id == pi.con_id
                    && pp.position == pi.position
                    && pp.avg_cost == pi.avg_cost
                    && pp.market_price == pi.market_price
                    && pp.market_value == pi.market_value
                    && pp.unrealized_pnl == pi.unrealized_pnl
                    && pp.realized_pnl == pi.realized_pnl)
            }).map(&to_entry).collect()
        };

        *prev_guard = Some(current);
        changed
    }

    /// Whether a figure the venue stated answers a tag the caller named.
    ///
    /// `$LEDGER` is not the name of a figure: it is the venue's word for the
    /// per-currency cash rows, bare for the base-currency bucket, `$LEDGER:EUR`
    /// for that currency's, `$LEDGER:ALL` for every one. Matched as a literal
    /// name it matched nothing, and the standard way to read per-currency cash,
    /// exchange rate and per-currency profit came back as an end with no rows —
    /// which a caller cannot tell from an account holding no cash at all.
    fn answers_tag(tag: &str, key: &str, currency: &str) -> bool {
        let Some(after) = tag.strip_prefix("$LEDGER") else { return tag == key };
        // The figures the venue keeps per currency, which are the rows a ledger
        // request asks for. Every other stated figure is about the account as a
        // whole and is not part of a currency bucket.
        //
        // The names are the venue's own and the set is closed, so it is written
        // down here as the venue writes it down. An insured-deposit balance is
        // among them: the venue publishes it as a figure of its own where the
        // session splits it from the cash balance, and folds it into that
        // balance where it does not.
        const LEDGER: [&str; 26] = [
            "Currency", "CashBalance", "TotalCashBalance", "AccruedCash", "StockMarketValue",
            "OptionMarketValue", "FutureOptionValue", "FuturesPNL", "NetLiquidationByCurrency",
            "UnrealizedPnL", "RealizedPnL", "ExchangeRate", "FundValue", "NetDividend",
            "MutualFundValue", "MoneyMarketFundValue", "CorporateBondValue", "TBondValue",
            "TBillValue", "WarrantValue", "FxCashBalance", "AccountOrGroup", "RealCurrency",
            "IssuerOptionValue", "Cryptocurrency", "InsuredDeposit",
        ];
        // Whatever follows the colon is the currency asked for, and no colon at
        // all is the base bucket — the venue's own reading of the text.
        let wanted = after.strip_prefix(':').unwrap_or("BASE");
        LEDGER.contains(&key) && (wanted == "ALL" || wanted == currency)
    }

    /// Prepare the initial summary or the values changed since its last interval.
    pub fn prepare_account_summary(&self, shared: &SharedState, _account_id: &str) -> Option<AccountSummaryBatch> {
        self.prepare_account_summary_where(shared, None)
    }

    /// The same for one request alone, for a call that answers and reads only
    /// its own.
    pub fn prepare_account_summary_for(&self, shared: &SharedState, req_id: i64) -> Option<AccountSummaryBatch> {
        self.prepare_account_summary_where(shared, Some(req_id))
    }

    fn prepare_account_summary_where(
        &self, shared: &SharedState, only: Option<i64>,
    ) -> Option<AccountSummaryBatch> {
        // Wait for gateway account data before delivering summary.
        // As above: on the download being finished. Answered on the first
        // figure, a summary asked for right after connecting -- which is the
        // ordinary idiom -- was handed the few tags parsed so far and its end,
        // before the tags it actually asked for arrived.
        // A session that has ended lets it through: no download is
        // coming, and parked behind the gate the caller could neither receive
        // its end nor withdraw it on the ended session.
        let mut req = self.account_summary_req.lock().unwrap();
        let mut other = self.account_summary_other_req.lock().unwrap();
        let mut last = self.last_account_summary.lock().unwrap();
        // A subscription outlives its own first answer, but not its session.
        // Held past the end, the caller can neither be told it finished nor
        // withdraw it on a session that is over.
        let session_over = shared.reference.session_over().is_some();

        // What the venue said, in the currency it said it in. Built from this
        // client's typed copy instead, an account held in a currency the venue
        // states its figures in came back as zero: the copy is filled from the
        // rows the venue sends, and a summary asked for before they arrive
        // reported an empty account rather than nothing.
        // "All" is the venue's word for every figure it holds. Matched against
        // a local list of names instead, "All" matches none of them and
        // returns empty, and any figure absent from that list is dropped with
        // it: accrued cash, SMA, look-ahead margin, per-currency ledger rows.
        // Empty tags are refused before this, as a gateway refuses them; a
        // list of nothing but separators is answered as "All" here, and what
        // the venue answers one with has not been seen.
        let asked: Vec<(i64, Vec<String>)> = req.iter().chain(other.iter())
            .filter(|(id, _)| only.is_none_or(|only| only == *id))
            .cloned().collect();
        for (req_id, tags) in &asked {
            let accounts = self.account_routes.lock().unwrap().summaries.get(req_id).cloned()
                .unwrap_or_else(|| vec![shared.account_name("")]);
            let portfolios: Vec<_> = accounts.iter().map(|account| (account, shared.portfolio_for(account))).collect();
            if !session_over && portfolios.iter().any(|(_, p)| !p.account_download_complete()) { continue; }
            let stated: Vec<_> = portfolios.iter().flat_map(|(account, p)| p.stated_account_values().into_iter()
                .map(|(ledger, key, value, currency)| ((*account).clone(), ledger, key, value, currency))).collect();
            let initial = !last.contains_key(req_id);
            let (when, already) = last.entry(*req_id)
                .or_insert_with(|| (std::time::Instant::now(), HashMap::new()));
            if !initial && when.elapsed() < std::time::Duration::from_secs(180) {
                continue;
            }
            *when = std::time::Instant::now();
            let wants_all = tags.is_empty() || tags.iter().any(|t| t == "All");
            let entries: Vec<_> = stated
                .iter()
                .filter(|(_, _, key, _, currency)| {
                    wants_all || tags.iter().any(|t| Self::answers_tag(t, key, currency))
                })
                .filter_map(|(account, ledger, key, value, currency)| {
                    let previous = already.insert((account.clone(), *ledger, key.clone(), currency.clone()), value.clone());
                    if previous.as_ref() == Some(value) {
                        return None;
                    }
                    Some(AccountSummaryEntry {
                        account: account.clone(),
                        tag: key.clone(), value: value.clone(), currency: currency.clone(),
                    })
                })
                .collect();
            if initial || !entries.is_empty() {
                if session_over {
                    if req.as_ref().is_some_and(|(id, _)| id == req_id) {
                        *req = None;
                    } else if other.as_ref().is_some_and(|(id, _)| id == req_id) {
                        *other = None;
                    }
                }
                return Some(AccountSummaryBatch { req_id: *req_id, entries });
            }
        }
        None
    }

    // ── Order routing ──

    /// Whether a gateway can read the manual time a withdrawal states, as it
    /// reads it before it withdraws anything: refused under 10301 in its
    /// words where it cannot, and the withdrawal with it.
    ///
    /// A gateway takes the UTC form, `yyyymmdd-hh:mm:ss`, and otherwise the
    /// local one — `yyyymmdd hh:mm:ss`, the date optional, with any words after
    /// a space read as a zone and set aside. What fails both is refused.
    ///
    /// Only the reading is made here. A gateway then turns what it read into
    /// a time and refuses one it cannot place, and one in a zone that is
    /// neither UTC nor its own machine's, which depends on where the gateway
    /// runs; neither is refused here.
    // ponytail: ASCII only. A gateway reads other scripts' digits too, so a
    // time holding anything else is let through rather than refused here.
    pub fn check_cancel_time(time: &str) -> Result<(), Refusal> {
        if time.is_empty() || !time.is_ascii() || utc_time_reads(time) || local_time_reads(time) {
            return Ok(());
        }
        Err(Refusal::stated(
            MANUAL_CANCEL_TIME_INVALID,
            "Manual Order Cancel Time: The date, time, or time-zone entered is invalid.\n\
             The correct format is yyyymmdd hh:mm:ss xx/xxxx\n\
             where yyyymmdd and xx/xxxx are optional.\n\
             E.g.: 20031126 15:59:00 US/Eastern\n\n\
             Note that there is a space between the date and time,\n\
             and between the time and time-zone.\n\n\
             If no date is specified, current date is assumed.\n\
             If no time-zone is specified, local time-zone is assumed(deprecated).\n\n\
             You can also provide yyyymmddd-hh:mm:ss time is in UTC.\n\
             Note that there is a dash between the date and time in UTC notation.",
        ))
    }

    /// A request's free-form option list, as the reference clients write it:
    /// one field, each entry `tag=value;`.
    pub fn written_options(options: &[crate::types::model::TagValue]) -> String {
        options.iter().map(|o| format!("{}={};", o.tag, o.value)).collect()
    }

    /// A request's free-form option list, as written, checked as a gateway
    /// reads and checks it.
    ///
    /// A gateway reads the list back from the one field it is written in: split on `;`
    /// and then on `=`, empty pieces dropped and nothing trimmed. An entry
    /// with no key or no value is a request it cannot read, refused under 320
    /// whatever else the venue has allowed. A key named twice keeps the last
    /// value. Unless the venue has lifted the checks at logon, each entry is
    /// then checked in the order a gateway holds them — its table's, not the
    /// caller's — a key the request does not take refused under 10337 and a
    /// value other than `0` or `1` under 10338. On a request that takes
    /// `manual`, the later check of it refuses a value that is not the number
    /// nought or one under 321, which is reached only where the checks were
    /// lifted.
    ///
    /// `manual` is the only key any request takes, the two option calculations
    /// take none, and an accepted one changes nothing a gateway sends or does.
    pub fn check_option_list(list: &OptionList, text: &str, features: &[String]) -> Result<(), Refusal> {
        let read = Self::read_option_list(list, text, features)?;
        Self::check_manual_option(list, &read)
    }

    /// Read the option field and check its keys and values. An order checks
    /// the manual value later, after its preview and trailing values.
    pub(crate) fn read_option_list<'a>(
        list: &OptionList, text: &'a str, features: &[String],
    ) -> Result<Vec<(&'a str, &'a str)>, Refusal> {
        let mut read: Vec<(&str, &str)> = Vec::new();
        for entry in text.split(';').filter(|e| !e.is_empty()) {
            let mut part = entry.split('=').filter(|p| !p.is_empty());
            let (Some(key), Some(value)) = (part.next(), part.next()) else {
                return Err(Refusal::stated(
                    REQUEST_NOT_READ,
                    "Error reading request:Please use 'Key=Value' format for Misc Options",
                ));
            };
            match read.iter_mut().find(|(k, _)| *k == key) {
                Some(held) => held.1 = value,
                None => read.push((key, value)),
            }
        }
        if !features.iter().any(|f| f == "NOAPIMISCVLD") {
            // The order a gateway's table walks its entries in: by bucket of
            // the key's hash, and in the order the keys were first put
            // within one. The table starts at sixteen buckets and doubles
            // past three quarters full.
            // ponytail: exact while no bucket holds nine keys, where the
            // table would reshape itself; no list here comes near that.
            let mut buckets = 16;
            while read.len() > buckets * 3 / 4 {
                buckets *= 2;
            }
            let bucket = |key: &str| {
                let h = key.encode_utf16().fold(0u32, |h, u| h.wrapping_mul(31).wrapping_add(u32::from(u)));
                (h ^ (h >> 16)) as usize & (buckets - 1)
            };
            let mut walked = read.clone();
            walked.sort_by_key(|(key, _)| bucket(key));
            let valid_keys = if list.checked_as.is_some() { "manual" } else { "" };
            for (key, value) in walked {
                if key != valid_keys {
                    return Err(Refusal::stated(MISC_OPTION_KEY_INVALID, format!(
                        "Misc options key={key} is invalid in {} request. Valid keys are: {valid_keys}",
                        list.request,
                    )));
                }
                if !matches!(value, "0" | "1") {
                    return Err(Refusal::stated(MISC_OPTION_VALUE_INVALID, format!(
                        "Misc options value={value} is invalid for key={key} in {} request. \
                         Valid values are: 0, 1",
                        list.request,
                    )));
                }
            }
        }
        Ok(read)
    }

    fn check_manual_option(list: &OptionList, read: &[(&str, &str)]) -> Result<(), Refusal> {
        // The later check, made once the request is read, on a value trimmed
        // of whatever sorts at or below a space and read as a number.
        // ponytail: ASCII digits only. A gateway reads any script's digits, so
        // a value holding another script's is let through rather than refused
        // on a reading a gateway may not share; an accepted `manual` changes
        // nothing sent. Read them when a caller writes them.
        if let (Some(checked_as), Some((_, value))) =
            (list.checked_as, read.iter().find(|(key, _)| *key == "manual"))
            && !value.chars().any(|c| !c.is_ascii() && c.is_numeric())
            && !matches!(value.trim_matches(|c: char| c <= ' ').parse::<i32>(), Ok(0 | 1))
        {
            return Err(Refusal::validation(format!(
                "{checked_as}: 'manual' has wrong value={value}, expected [1 or 0]",
            )));
        }
        Ok(())
    }

    /// Pre-validate order fields that don't depend on instrument ID.
    /// Call this before the order is handed to the engine, to fail fast.
    ///
    /// Where a gateway refuses an order while it validates it, it answers under
    /// 321 and puts the name of the request it was validating in front of the
    /// reason. This client answers under the same number with the same reason,
    /// without the name.
    pub fn validate_order(order: &ApiOrder, session: &OrderSession) -> Result<(), Refusal> {
        // An option list that cannot be read, or names an unknown key or
        // value, is refused before anything else about the order.
        let written_options = Self::written_options(&order.order_misc_options);
        let options = Self::read_option_list(&ORDER_OPTIONS, &written_options, &session.features)?;
        order.side()?;

        // An execution condition names a symbol, an exchange and a security
        // type, and the venue wants all three. Left short, it accepts the
        // order and holds it Inactive with "Invalid value in field # 6246",
        // which names a tag no caller of this client has heard of.
        for condition in &order.conditions {
            if let crate::types::OrderCondition::Execution { symbol, exchange, sec_type, .. } = condition {
                for (what, value) in
                    [("symbol", symbol), ("exchange", exchange), ("security type", sec_type)]
                {
                    if value.trim().is_empty() {
                        return Err(Refusal::stated(CONDITION_CONTRACT_INCOMPLETE, format!(
                            "an execution condition needs a {what}; the venue refuses one \
                             that leaves any of symbol, exchange or security type out",
                        )));
                    }
                }
            }
            // A price condition's trigger method, and a margin condition's
            // percent, are `int`s as the TWS API carries them, and go to the
            // venue as stated: no gateway refusal of either has been read, so
            // none is made here.
        }

        // Reject non-finite and out-of-range numerics up front, before any
        // caller-visible order gets built from a NaN, an Infinity, or a
        // magnitude the wire's fixed-point i64 can't hold.
        require_finite_price("lmt_price", order.lmt_price)?;
        require_finite_price("aux_price", order.aux_price)?;
        require_finite_price("discretionary_amt", order.discretionary_amt)?;
        // Nought is no discretion; below it is no amount a gateway takes.
        if order.discretionary_amt < 0.0 {
            return Err(Refusal::stated(
                DISCRETIONARY_AMOUNT_INVALID,
                "Discretionary amount does not conform to the minimum price variation for \
                 this contract",
            ));
        }
        require_finite_price("cash_qty", order.cash_qty)?;
        require_finite_price("trigger_price", order.trigger_price)?;
        require_finite_price("adjusted_stop_price", order.adjusted_stop_price)?;
        require_finite_price("adjusted_stop_limit_price", order.adjusted_stop_limit_price)?;
        // f64::MAX is the sentinel for "not set" on these three; any other
        // value must be finite and representable.
        if order.trail_stop_price != f64::MAX {
            require_finite_price("trail_stop_price", order.trail_stop_price)?;
        }
        if order.lmt_price_offset != f64::MAX {
            require_finite_price("lmt_price_offset", order.lmt_price_offset)?;
        }
        if order.adjusted_trailing_amount != f64::MAX {
            require_finite_price("adjusted_trailing_amount", order.adjusted_trailing_amount)?;
        }
        // Every other field a saturating cast turns into a different,
        // valid-looking number on its way to the wire. Guarding only the
        // handful above lets a NaN ladder step reach the venue as an increment
        // of zero, and a benchmark reference or hedging leg stated as an
        // infinity reach it as the largest price there is.
        for (field, value) in [
            ("scale_price_increment", order.scale_price_increment),
            ("scale_profit_offset", order.scale_profit_offset),
            ("scale_price_adjust_value", order.scale_price_adjust_value),
            ("delta_neutral_aux_price", order.delta_neutral_aux_price),
            ("pegged_change_amount", order.pegged_change_amount),
            ("reference_change_amount", order.reference_change_amount),
            ("starting_price", order.starting_price),
            ("stock_ref_price", order.stock_ref_price),
            ("stock_range_lower", order.stock_range_lower),
            ("stock_range_upper", order.stock_range_upper),
            ("percent_offset", order.percent_offset),
            ("volatility", order.volatility),
        ] {
            // f64::MAX is this API's "not set" and states nothing.
            if value != f64::MAX {
                require_finite_price(field, value)?;
            }
        }
        for (at, leg) in order.order_combo_legs.iter().enumerate() {
            if *leg != f64::MAX {
                require_finite_price(&format!("order_combo_legs[{at}]"), *leg)?;
            }
        }
        // A trail is an amount or a percentage, and a trailing order naming
        // both is refused as a gateway refuses it, while it reads the order.
        // Nought is no trail, and so is the reference client's value for one
        // left unset.
        let named = |v: f64| v != 0.0 && v != f64::MAX;
        let trailing = matches!(order.order_type_named(), Some("TRAIL" | "TRAIL LIMIT"));
        if trailing && named(order.aux_price) && named(order.trailing_percent) {
            return Err(Refusal::stated(
                REQUEST_NOT_READ,
                "Error reading request: Cannot specify Trailing Amount and Trailing Percent \
                 at the same time",
            ));
        }
        // A trail by percentage, outside what a percentage can be.
        if trailing
            && named(order.trailing_percent)
            && (order.trailing_percent < 0.0 || order.trailing_percent > 100.0)
        {
            return Err(Refusal::validation(
                "Invalid Trailing Percent value. Valid values are greater than 0 and less than 100.",
            ));
        }
        // A trailing stop limit by percentage, which a gateway takes. What it
        // states for one on the trigger is not established here, and a guess
        // would put a price the caller did not ask for on the order.
        if order.order_type_named() == Some("TRAIL LIMIT") && named(order.trailing_percent) {
            return Err(Refusal::validation(
                "trailing_percent on a TRAIL LIMIT order is not carried by this client: what \
                 the order states as its trigger then is not established here. State the trail \
                 as an amount on aux_price to place the order.",
            ));
        }
        // f64::MAX is this API's "not set" here too, and a caller who states
        // it is stating nothing: the models a caller builds from carry it as
        // the default for this field, so refusing it refused every order that
        // never mentioned a trailing percentage at all.
        if order.trailing_percent != f64::MAX
            && (!order.trailing_percent.is_finite()
                || order.trailing_percent < 0.0
                || order.trailing_percent * 100.0 > u32::MAX as f64)
        {
            return Err(format!(
                "trailing_percent must be a finite, non-negative number, got {}",
                order.trailing_percent
            ).into());
        }
        // The quantity reaches the wire as the decimal it was given, so a
        // fraction of a share is carried rather than refused. The bound is
        // where the fixed-point conversion stops being exact: past it the
        // low digits are lost, and the order goes out for a size nobody
        // asked for rather than being refused.
        if !order.total_quantity.is_finite() {
            return Err("total_quantity must be a finite number".to_string().into());
        }
        if order.total_quantity.abs() > crate::types::MAX_QTY_SHARES {
            return Err(format!("total_quantity {} is too large", order.total_quantity).into());
        }
        // Zero and negative go out as they were given. Both encode exactly —
        // tag 38 carries the sign and carries a zero — so the venue is asked
        // and the venue answers, under its own code. Refused here instead, a
        // caller reading that code was handed one this client made up for a
        // request the venue was never asked about.
        if order.parent_id < 0 {
            return Err(format!("parent_id must not be negative, got {}", order.parent_id).into());
        }

        // A time in force this client does not know becomes DAY, and a DAY
        // order dies at the close. A caller who wrote "gtc" and meant GTC gets
        // an order that quietly stops existing, so the spelling is checked
        // rather than fallen back on.
        const TIME_IN_FORCE: [&str; 10] =
            ["DAY", "GTC", "IOC", "FOK", "OPG", "GTD", "GTX", "DTC", "AUC", "NMIN"];
        if !order.tif.is_empty() && !TIME_IN_FORCE.contains(&order.tif.as_str()) {
            return Err(format!(
                "tif '{}' is not one this venue carries. It is one of {}, \
                 spelled exactly — an unrecognised value would otherwise be \
                 sent as DAY and expire at the close.",
                order.tif,
                TIME_IN_FORCE.join(", "),
            ).into());
        }

        // An expiry this client cannot read used to be logged and dropped, and
        // the order then went out with no expiry at all — which for a GTC-until
        // order is a different order from the one asked for.
        if !order.good_till_date.is_empty()
            && let Err(e) = crate::protocol::datetime::parse_ib_expiry(&order.good_till_date)
        {
            return Err(Refusal::stated(GOOD_TILL_DATE_INVALID, format!(
                "good_till_date '{}' cannot be read: {e}. State it as \
                 `yyyyMMdd HH:mm:ss` with an optional zone, or `yyyyMMdd` for \
                 a date — sent unread, the order would carry no expiry.",
                order.good_till_date,
            )));
        }

        // Everything this client does not carry. Each is documented on the
        // field with what is known about it; each is refused here, because a
        // caller that set one and was answered anyway would have an order the
        // venue never saw the instruction on, and nothing to say so.
        //
        // Compared against the default rather than against emptiness: a field
        // left alone is not a field asked for, and only what a caller stated
        // is refused. The list is checked against the documented one by
        // `scripts/gen_order_field_reach.py`, so a field that gains or loses
        // its note here cannot drift from the note itself.
        static UNCARRIED: LazyLock<ApiOrder> = LazyLock::new(ApiOrder::default);
        macro_rules! refuse_if_stated {
            // A field with something of its own to say says it. The rest share
            // the sentence below, and both are one list, so the registry the
            // reach check reads stays whole.
            ($($field:ident $(: $why:expr)?),+ $(,)?) => {
                $(if order.$field != UNCARRIED.$field {
                    let stated: Option<&str> = None $(.or(Some($why)))?;
                    return Err(match stated {
                        Some(why) => format!("{} {why}", stringify!($field)),
                        None => format!(
                            "{} is not carried by this client. It is documented \
                             on the field with what is known about it. Leave it \
                             at its default to place the order without it.",
                            stringify!($field),
                        ),
                    }.into());
                })+
            };
        }
        refuse_if_stated!(
            smart_combo_routing_params: "is not carried by this client: a gateway \
                sends each after checking its name and value against the \
                combination, and those checks are not all established here. Sent \
                unchecked, a parameter a gateway would refuse could reach the venue. \
                Leave it empty to place the combination without them.",
        );
        // Taken, and not sent, as a gateway sends nothing for it on the orders
        // this client places — except on an order that is itself a short sale
        // and names a hedging order type, where a gateway applies it over the
        // order's own short-sale handling and what that makes of the order is
        // not established here.
        if order.delta_neutral_short_sale
            && !order.delta_neutral_order_type.is_empty()
            && matches!(order.side(), Ok(Side::ShortSell))
        {
            return Err(Refusal::validation(
                "delta_neutral_short_sale on an order that is itself a short sale and \
                 names a hedging order type is not carried by this client: what a gateway \
                 makes of the order's own short sale then is not established here. Leave \
                 it unset to place the order.",
            ));
        }
        // What kind of preview is asked for. A released gateway previews the
        // ordinary kind alone; an unset kind is nought.
        let what_if_type = if order.what_if_type == i32::MAX { 0 } else { order.what_if_type };
        if order.what_if && !order.transmit {
            return Err(Refusal::validation("What-If order should have transmit flag set to TRUE "));
        }
        if !order.what_if && what_if_type > 0 {
            return Err(Refusal::validation(
                "What-If type specified but What-If flag is not set. Orders with \
                 whatIfType must have whatIf=true.",
            ));
        }
        if !(0..=6).contains(&what_if_type) || (order.what_if && what_if_type > 1) {
            return Err(Refusal::validation(format!(
                "What-If type {what_if_type} is not supported for your account configuration.",
            )));
        }
        // A login holding several accounts states which one an order is for,
        // and one that names none is refused. A login holding one puts its
        // own account on the order whatever the order names. An advisor's
        // login is not asked: its order is allocated across accounts.
        if !session.advisor && session.holds_several_accounts() && order.account.is_empty() {
            return Err(Refusal::validation("You must specify an account."));
        }
        // NOAPIMISCVLD lifts the key and value checks; a manual value other
        // than 0 or 1 is still refused, after the preview and trailing-percent
        // checks.
        Self::check_manual_option(&ORDER_OPTIONS, &options)?;

        // Held until a stated moment. Unreadable, the delay used to be dropped
        // and the order filled at once, which is the opposite of what was asked.
        if !order.good_after_time.is_empty()
            && let Err(e) = crate::protocol::datetime::parse_ib_expiry(&order.good_after_time)
        {
            return Err(format!(
                "good_after_time '{}' cannot be read: {e}. State it as \
                 `yyyyMMdd HH:mm:ss` with an optional zone, or `yyyyMMdd` for \
                 the start of a day — sent unread, the order would go live at \
                 once instead of waiting.",
                order.good_after_time,
            ).into());
        }

        // A quantity or a slot stated as a negative number is not a smaller
        // one, it is a mistake. The conversion clamps it, so the order goes out
        // asking for none of whatever was asked for — an iceberg with no
        // display size, a minimum of nothing, a leg that borrows from nowhere.
        for (what, stated) in [
            ("display_size", i64::from(order.display_size)),
            ("min_qty", i64::from(order.min_qty)),
            ("scale_init_level_size", i64::from(order.scale_init_level_size)),
            ("scale_subs_level_size", i64::from(order.scale_subs_level_size)),
            ("scale_price_adjust_interval", i64::from(order.scale_price_adjust_interval)),
        ] {
            if stated < 0 {
                return Err(format!(
                    "{what} is {stated}, which is not a quantity. Sent, it goes \
                     out as none at all and the order does something other than \
                     what was asked.",
                ).into());
            }
        }
        // These two go out in a single byte, so a value above it is not a
        // larger one — it arrives as whatever fits.
        for (what, stated) in [
            ("volatility_type", order.volatility_type),
            ("short_sale_slot", order.short_sale_slot),
        ] {
            if !(0..=255).contains(&stated) {
                return Err(format!(
                    "{what} is {stated}, which does not fit the field it goes \
                     out in. Sent, it would arrive as a different value.",
                ).into());
            }
        }
        // A cash quantity is what to spend, so a negative one buys nothing: the
        // encoder omits it and the order goes out sized by its quantity alone.
        if order.cash_qty < 0.0 {
            return Err(format!(
                "cash_qty is {}, which is not an amount to spend. Sent, it is \
                 omitted and the order goes out sized by its quantity instead.",
                order.cash_qty,
            ).into());
        }
        // Both halves or neither: a tier named with nothing against it is not
        // an arrangement, and the encoder writes neither part rather than half.
        if order.soft_dollar_tier_name.is_empty() != order.soft_dollar_tier_val.is_empty() {
            return Err(
                "a soft-dollar arrangement is a tier and what it is worth. \
                 Stated with one of the two, neither goes out and the \
                 commission goes wherever the account's default sends it."
                    .to_string()
                    .into(),
            );
        }
        // These carry a sentinel for "not stated", so only a value that is
        // neither the sentinel nor a quantity is a mistake.
        for (what, stated) in [
            ("min_trade_qty", order.min_trade_qty),
            ("post_to_ats", order.post_to_ats),
            ("min_compete_size", order.min_compete_size),
        ] {
            if stated != i32::MAX && stated < 0 {
                return Err(format!(
                    "{what} is {stated}, which is not a quantity. Sent, it goes \
                     out as none at all. Leave it at its default to state none.",
                ).into());
            }
        }

        // Two fields the conversion narrows to a set, turning anything else
        // into the default. An unknown value reaching the venue is narrowed
        // there the same way, but a value outside the set is a caller's
        // mistake and is reported rather than silently changed.
        if !matches!(order.trigger_method, 0..=4 | 7 | 8) {
            return Err(Refusal::stated(TRIGGER_METHOD_INVALID, format!(
                "trigger_method {} is not one this venue carries. It is 0 to 4, \
                 7 or 8 — anything else becomes 0, which is the default trigger \
                 and not the one asked for.",
                order.trigger_method,
            )));
        }
        if order.oca_type != 0 && !matches!(order.oca_type, 1..=4) {
            return Err(format!(
                "oca_type {} is not one this venue carries. It is 1 to 4, or 0 \
                 to leave it unset — anything else is sent as unset and the \
                 group cancels under the venue's default rather than the rule \
                 asked for.",
                order.oca_type,
            ).into());
        }

        // A hedge is stated as a kind and a parameter that goes with it. A
        // kind this client does not know reads as no hedge at all, and the
        // order goes out unhedged; a beta or a ratio it cannot read becomes
        // zero, which is omitted, and the order goes out hedged against
        // nothing. Both are a different order from the one asked for.
        const HEDGE: [&str; 5] = ["D", "B", "F", "P", "S"];
        if !order.hedge_type.is_empty() {
            let kind = order.hedge_type.to_ascii_uppercase();
            if !HEDGE.contains(&kind.as_str()) {
                return Err(format!(
                    "hedge_type '{}' is not one this venue carries. It is one of \
                     {} — delta, beta, FX, pair, or the venue's pair. An \
                     unrecognised kind would otherwise be dropped and the order \
                     sent unhedged.",
                    order.hedge_type,
                    HEDGE.join(", "),
                ).into());
            }
            // Only these two kinds are struck at a number. Delta and FX take
            // no parameter, so one stated with them is not read.
            if matches!(kind.as_str(), "B" | "P")
                && order.hedge_param.parse::<f64>().is_err()
            {
                return Err(format!(
                    "hedge_type '{}' is struck at a number and hedge_param \
                     '{}' is not one. Stated unreadable, the hedge would be \
                     sent as zero and dropped, leaving the order hedged \
                     against nothing.",
                    order.hedge_type, order.hedge_param,
                ).into());
            }
        }

        let order_type = order.order_type_named().unwrap_or("");

        // An order carrying an algorithm is encoded as a limit and nothing
        // else: the strategy rides on an order whose type byte is written
        // once, as `2`. The caller's own type must therefore be a limit.
        // Accepting another type and encoding a limit anyway sends an order
        // the caller did not describe — `MKT` with an algorithm becomes a
        // limit at whatever `lmt_price` holds.
        if !order.algo_strategy.is_empty() {
            if order.algo_strategy.eq_ignore_ascii_case("Adaptive") {
                adaptive_priority(&order.algo_params)?;
            } else {
                crate::client_core::parse_algo_params(&order.algo_strategy, &order.algo_params)?;
            }
            if !order.order_type.is_empty() && order_type != "LMT" {
                return Err(format!(
                    "algo_strategy '{}' is carried on a limit order, and this one \
                     states order_type '{}'. Sent as it stands the venue would \
                     receive a limit at {}, which is not the order described.",
                    order.algo_strategy, order.order_type, order.lmt_price,
                ).into());
            }
            if let (_, Some(why)) = Self::retired_instructions(order, session) {
                return Err(why);
            }
            return Ok(());
        }
        // A preview asks about an order this client could send, so it answers
        // for the same set of types. Returning before this sends an unknown
        // type to the wire as a limit, and the venue answers about an order
        // the caller did not ask about.
        if order.order_type_named().is_none() {
            return Err(Self::not_an_order_type(order));
        }
        if order.what_if {
            if let (_, Some(why)) = Self::retired_instructions(order, session) {
                return Err(why);
            }
            return Ok(());
        }

        // Reject orders that require aux_price when it is zero — prevents silent no-
        // trigger bugs.
        match order_type {
            "STP" | "STP PRT" | "MIT" if order.aux_price == 0.0 => {
                return Err(Refusal::stated(TRIGGER_PRICE_MISSING, format!(
                    "{} order requires aux_price (stop/trigger price) but got 0.0 — \
                     set aux_price to the desired trigger price, not lmt_price",
                    order.order_type
                )));
            }
            "STP LMT" | "LIT" if order.aux_price == 0.0 => {
                return Err(Refusal::stated(TRIGGER_PRICE_MISSING, format!(
                    "{} order requires aux_price (stop/trigger price) but got 0.0",
                    order.order_type
                )));
            }
            "TRAIL" if !named(order.trailing_percent) && !named(order.aux_price) => {
                return Err(Refusal::stated(
                    TRIGGER_PRICE_MISSING,
                    "TRAIL order requires either trailing_percent or aux_price (trail amount) \
                     but both are 0.0",
                ));
            }
            "TRAIL LIMIT" if order.aux_price == 0.0 => {
                return Err(Refusal::stated(
                    TRIGGER_PRICE_MISSING,
                    "TRAIL LIMIT order requires aux_price (trail amount) but got 0.0",
                ));
            }
            // This type's wire shape has one price and no second tag for it
            // to move to, so an order sent without one is malformed rather
            // than refused by the venue under a code a caller could expect.
            "PEG BEST" if order.lmt_price == 0.0 => {
                return Err(Refusal::validation(
                    "PEG BEST order requires lmt_price (the order's price) but got 0.0",
                ));
            }
            _ => {}
        }

        // Retired instructions are processed after all the order fields.
        if let (_, Some(why)) = Self::retired_instructions(order, session) {
            return Err(why);
        }
        Ok(())
    }

    /// The refusal of an order-type name this client does not place: one a
    /// gateway places and this client does not, by name, or a name that is no
    /// order type, as a gateway refuses it.
    fn not_an_order_type(order: &ApiOrder) -> Refusal {
        if crate::types::model::placed_only_by_a_gateway(&order.order_type) {
            Refusal::stated(
                ORDER_TYPE_UNSUPPORTED,
                format!("Unsupported order type: '{}' is not an order type this client places", order.order_type),
            )
        } else {
            Refusal::stated(INVALID_ORDER_TYPE, "Invalid order type")
        }
    }

    /// The retired instructions an order states, walked in the order a
    /// gateway walks them: `e_trade_only`, `firm_quote_only`,
    /// `nbbo_price_cap`, then `opt_out_smart_routing`. Where the session has
    /// withdrawn one, the walk stops there with its refusal; otherwise the
    /// order goes without it and the caller is warned. Returns the warnings
    /// given before the walk ended, and the refusal that ended it.
    pub fn retired_instructions(order: &ApiOrder, session: &OrderSession) -> (Vec<Refusal>, Option<Refusal>) {
        let cap = order.nbbo_price_cap != f64::MAX && order.nbbo_price_cap.is_finite();
        let mut warned = Vec::new();
        for (stated, feature, refused, warning, name) in [
            (order.e_trade_only, "DEPRETFQNC", E_TRADE_ONLY_WITHDRAWN, E_TRADE_ONLY_DROPPED, "EtradeOnly"),
            (order.firm_quote_only, "DEPRETFQNC", FIRM_QUOTE_ONLY_WITHDRAWN, FIRM_QUOTE_ONLY_DROPPED, "FirmQuoteOnly"),
            (cap, "DEPRETFQNC", NBBO_PRICE_CAP_WITHDRAWN, NBBO_PRICE_CAP_DROPPED, "NbboPriceCap"),
            (
                order.opt_out_smart_routing, "DEPRPREFBEST",
                OPT_OUT_SMART_ROUTING_WITHDRAWN, OPT_OUT_SMART_ROUTING_DROPPED, "OptOutFromSmartRouting",
            ),
        ] {
            if !stated {
                continue;
            }
            let why = format!("The '{name}' order attribute is not supported.");
            if session.enables(feature) {
                return (warned, Some(Refusal::stated(refused, why)));
            }
            warned.push(Refusal::stated(warning, why));
        }
        (warned, None)
    }

    /// The order as it goes out on this session.
    ///
    /// A login holding one account states that account on every order it
    /// sends, whatever the order names, so the name goes no further than the
    /// record of the order. A login holding several states the one the order
    /// names.
    pub fn as_sent<'a>(order: &'a ApiOrder, session: &OrderSession) -> std::borrow::Cow<'a, ApiOrder> {
        let clear_account = !order.account.is_empty() && !session.holds_several_accounts();
        if !clear_account && !order.e_trade_only && !order.firm_quote_only
            && order.nbbo_price_cap == f64::MAX
        {
            return std::borrow::Cow::Borrowed(order);
        }
        let mut sent = order.clone();
        if clear_account {
            sent.account.clear();
        }
        sent.e_trade_only = false;
        sent.firm_quote_only = false;
        sent.nbbo_price_cap = f64::MAX;
        std::borrow::Cow::Owned(sent)
    }

    /// Remember how a request asked for its bar times to be written.
    ///
    /// The reference client numbers the two forms: 1 for the venue's
    /// spelling, 2 for seconds since the epoch. Anything else is 1, which is
    /// what that client does with a number it does not know.
    pub fn note_date_format(&self, req_id: i64, format_date: i32) {
        self.historical_asks.lock().unwrap().entry(req_id).or_default().format_date = format_date;
    }

    /// The end and the duration a bar request named, which its range is
    /// counted from once its bars have all arrived, and the size of its bars,
    /// which says how the bar still forming is dated.
    pub fn note_historical_span(
        &self, req_id: i64, end_date_time: &str, duration: &str, bar_size: &str,
    ) {
        use crate::control::historical::BarSize;
        let mut asks = self.historical_asks.lock().unwrap();
        let ask = asks.entry(req_id).or_default();
        ask.end_date_time = end_date_time.to_string();
        ask.duration = duration.to_string();
        ask.daily_session = None;
        ask.by_day = BarSize::from_api_str(bar_size)
            .is_ok_and(|size| size.seconds() >= BarSize::Day1.seconds());
    }

    /// The range a finished request covered, as stated beside the last bar.
    /// Empty where nothing was asked under the id.
    pub fn historical_range_for(&self, req_id: i64, zone: &str) -> (String, String) {
        let ask = self.historical_asks.lock().unwrap().get(&req_id).cloned();
        ask.filter(|a| !a.duration.is_empty())
            .and_then(|a| {
                crate::protocol::datetime::historical_range(&a.end_date_time, &a.duration, zone)
            })
            .unwrap_or_default()
    }

    /// The date form the caller asked for under `req_id`: 1 where none was
    /// asked, as the reference client takes it.
    pub fn asked_date_format(&self, req_id: i64) -> i32 {
        self.historical_asks.lock().unwrap().get(&req_id).map_or(1, |ask| ask.format_date)
    }

    /// A bar's time, written the way the request that asked for it wanted.
    ///
    /// Where the stamp cannot be read back to an instant it is handed over as
    /// it came: a time nobody can parse is still what the venue said, and
    /// replacing it with a zero would state an instant in 1970.
    pub fn bar_time_for(&self, req_id: i64, stated: &str, zone: &str) -> String {
        let format_date = self
            .historical_asks
            .lock()
            .unwrap()
            .get(&req_id)
            .map_or(1, |ask| ask.format_date);
        crate::protocol::datetime::bar_date_as_asked(stated, format_date, zone)
    }

    /// Date a daily bar by the supplied session end on the series clock.
    pub fn historical_bar_time_for(
        &self, req_id: i64, bar: &crate::control::historical::HistoricalBar, zone: &str,
    ) -> String {
        let mut asks = self.historical_asks.lock().unwrap();
        if let Some(ask) = asks.get_mut(&req_id)
            && ask.by_day
            && let (Some(start), Some(end)) = (
                crate::protocol::datetime::ib_datetime_to_unix(&bar.time),
                crate::protocol::datetime::ib_datetime_to_unix(&bar.end),
            )
        {
            if start < end && ask.daily_session.is_none_or(|(held, _)| start >= held) {
                ask.daily_session = Some((start, end));
            }
            let dated = crate::protocol::datetime::bar_date_as_asked(&bar.end, 1, zone);
            return dated[..8].to_string();
        }
        drop(asks);
        self.bar_time_for(req_id, &bar.time, zone)
    }

    /// Forget what each caller was last told of every quote.
    ///
    /// At a market-data drop the engine zeroes every quote, so nothing reads a
    /// pre-drop price as current. Compared against what the caller had been
    /// told, those noughts read as moves and went out as prices; compared
    /// against nothing, only what the venue restates goes out, and what the
    /// caller last heard stands until then.
    pub fn forget_last_quotes(&self) {
        self.last_quotes.lock().unwrap().clear();
    }

    /// The zone a series was stated on, kept for the bars that continue it.
    pub fn note_historical_zone(&self, req_id: i64, zone: &str) {
        if zone.is_empty() {
            return;
        }
        self.historical_asks.lock().unwrap().entry(req_id).or_default().zone = zone.to_string();
    }

    /// A continuing bar's time as the caller asked bars to be dated, on the
    /// zone its history was stated on — or by its day alone where its bars are
    /// a day long or longer, as the history's are: by where it ends, the
    /// history's last bar's end or, past it, the end of the bar it rolled over
    /// to.
    pub fn bar_time_for_epoch(&self, req_id: i64, secs: i64, placed: Option<(u32, u32)>) -> String {
        let asks = self.historical_asks.lock().unwrap();
        let (format_date, zone, by_day) = asks
            .get(&req_id)
            .map_or((1, "", false), |ask| (ask.format_date, ask.zone.as_str(), ask.by_day));
        let placed = placed.map(|(start, end)| (i64::from(start), i64::from(end)));
        // A bar that ends where it opens still ends there, and is dated by it.
        let end = asks.get(&req_id).and_then(|ask| ask.daily_session).into_iter().chain(placed)
            .find(|(start, end)| *start <= secs && (secs < *end || secs == *start)).map(|(_, end)| end);
        crate::protocol::datetime::bar_epoch_as_asked(secs, end, format_date, zone, by_day)
    }

    /// Validate historical-request arguments before anything reaches the
    /// engine: an unrecognized bar_size falls back to 5-minute bars
    /// silently through two divergent tables, and an unrecognized
    /// what_to_show falls back to TRADES. The caller is answered with a
    /// synchronous Err at the call instead of plausible, wrong candles.
    ///
    /// And what a gateway refuses before it asks the venue, in its words: the
    /// adjusted series with an end date or with bars longer than a day, and a
    /// request kept up to date with an end date, on a combination, or on a
    /// series a gateway keeps no bar current for.
    pub fn validate_historical_args(
        bar_size: &str,
        what_to_show: &str,
        keep_up_to_date: bool,
        end_date_time: &str,
        sec_type: &str,
    ) -> Result<(), String> {
        let bs = crate::control::historical::BarSize::from_api_str(bar_size)?;
        // The adjusted series is folded here from the raw trades and the
        // contract's actions. A gateway refuses it with an end date, and with
        // a bar longer than a day.
        let adjusted = crate::control::historical::what_to_show_is_adjusted(what_to_show);
        if adjusted && !end_date_time.trim().is_empty() {
            return Err("End date not supported with adjusted last".to_string());
        }
        if adjusted && bs.seconds() > crate::control::historical::BarSize::Day1.seconds() {
            return Err("Multi day bar size not supported with adjusted last".to_string());
        }
        // Its name is not in the table `from_api_str` checks: it is not a
        // name the venue answers to.
        if !adjusted {
            crate::control::historical::BarDataType::from_api_str(what_to_show)?;
        }
        if !keep_up_to_date {
            return Ok(());
        }
        if !end_date_time.trim().is_empty() {
            return Err("End date not supported with live updates".to_string());
        }
        if sec_type.trim().eq_ignore_ascii_case("BAG") {
            return Err("Live updates for combos are not supported".to_string());
        }
        // The series a gateway keeps a bar current for, by the name the caller
        // gave, compared as given. The adjusted series is not one of them: it
        // is folded from a whole series, and a request that never completes
        // has none. Nor is a name left empty, which this client reads as
        // trades elsewhere and a gateway compares as it stands.
        const KEPT_CURRENT: [&str; 6] = [
            "TRADES", "MIDPOINT", "BID", "ASK", "CALL_OPTION_OPEN_INTEREST",
            "PUT_OPTION_OPEN_INTEREST",
        ];
        if !KEPT_CURRENT.iter().any(|kept| what_to_show.eq_ignore_ascii_case(kept)) {
            return Err("Source price not supported with live updates".to_string());
        }
        if !bs.supports_keep_up_to_date() {
            return Err(format!(
                "bar_size '{bar_size}' cannot be kept up to date: what the venue \
                 keeps sending is five-second bars and a bar still forming is folded \
                 from those, so a size shorter than five seconds cannot be formed",
            ));
        }
        Ok(())
    }

    /// What an order states that this client cannot carry out as stated.
    ///
    /// Two cases, and both would otherwise go out meaning something the caller
    /// did not ask for: a delta-neutral order naming no order type for its
    /// hedging leg, which describes nothing to place, and a hedge parameter
    /// given to a hedge type that takes none.
    ///
    /// Everything else an order can state is encoded. This list was once much
    /// longer — volatility, scale, short-sale slots and the rest were refused
    /// here because no encoder carried them. They are carried now.
    pub fn validate_supported_instructions(o: &ApiOrder) -> Result<(), String> {
        let mut unsent: Vec<&str> = Vec::new();
        // A hedging leg with no order type describes nothing to place.
        if o.delta_neutral_order_type.is_empty()
            && (o.delta_neutral_aux_price != f64::MAX || o.delta_neutral_con_id != 0)
        {
            unsent.push("deltaNeutral without deltaNeutralOrderType");
        }
        // A hedge parameter only means something for the kinds that take one.
        if !o.hedge_param.is_empty()
            && !matches!(o.hedge_type.to_ascii_uppercase().as_str(), "B" | "P")
        {
            unsent.push("hedgeParam for a hedge type that takes none");
        }
        if unsent.is_empty() {
            return Ok(());
        }
        Err(format!(
            "this order sets {}, which is not sent — the order placed would be a \
             different one from the order asked for. Remove it, or place the trade \
             it describes directly.",
            unsent.join(", "),
        ))
    }

    /// A combination states its legs on the order, so an order for one is
    /// placeable. What is refused is a combination that names none: the venue
    /// would be given a security type with nothing to build from.
    pub fn validate_combo_legs(sec_type: &str, leg_count: usize) -> Result<(), Refusal> {
        let names_a_combination = sec_type.eq_ignore_ascii_case("BAG")
            || sec_type.eq_ignore_ascii_case("COMBO");
        if leg_count > 0 || !names_a_combination {
            return Ok(());
        }
        Err(Refusal::stated(
            COMBINATION_NEEDS_LEGS,
            "a combination order has no legs: state them on the contract, \
             or use the security type of the thing you mean to trade",
        ))
    }

    /// What each leg of a combination states, before any of it is converted.
    ///
    /// The conversion takes each leg as it finds it: a side it does not
    /// recognise becomes a buy, a negative ratio becomes none, and a slot
    /// outside a byte is clamped to the nearest one that fits. Each of those is
    /// a leg trading the other way, in no size, or borrowing from somewhere
    /// nobody named —
    /// against the rest of a combination that is priced as one thing.
    pub fn validate_leg(at: usize, leg: &crate::types::model::ComboLeg) -> Result<(), Refusal> {
        if !leg.action.eq_ignore_ascii_case("BUY") && !leg.action.eq_ignore_ascii_case("SELL") {
            return Err(Refusal::stated(COMBINATION_LEG_INVALID, format!(
                "leg {at} states side {:?}, which is BUY or SELL. Anything else \
                 is sent as a buy, and the combination trades the wrong way \
                 round on that leg.",
                leg.action,
            )));
        }
        if leg.ratio <= 0 {
            return Err(Refusal::stated(COMBINATION_LEG_INVALID, format!(
                "leg {at} states a ratio of {}, which is not a quantity. Sent, \
                 the leg goes out in no size at all.",
                leg.ratio,
            )));
        }
        for (what, stated) in [("openClose", leg.open_close), ("shortSaleSlot", leg.shorting_policy)] {
            if !(0..=255).contains(&stated) {
                return Err(Refusal::stated(COMBINATION_LEG_INVALID, format!(
                    "leg {at} states {what} as {stated}, which does not fit the \
                     field it goes out in — sent, it would arrive as a \
                     different value.",
                )));
            }
        }
        Ok(())
    }

    /// `con_id` names one contract on its own. Where the caller gave one,
    /// nothing else has to be stated: the venue accepts an order carrying only
    /// the id and the security type, and answers it with a margin preview.
    /// The checks below exist to catch a contract that names a whole chain or
    /// series, which a contract id never does.
    /// Refuse a security type the venue does not permit this account to trade.
    ///
    /// The venue states its permissions at logon and refuses an order on an
    /// unpermitted type by returning it Inactive with no text at all, so
    /// without this the caller is told nothing. Silence here is not
    /// permission: a session that stated none has nothing to enforce.
    ///
    /// Shared, because a guard on one surface and not the other means the
    /// caller it protects depends on which language they wrote in.
    pub fn refuse_unpermitted_sec_type(
        permitted: &std::collections::HashMap<String, Vec<String>>,
        sec_type: &str,
    ) -> Result<(), Refusal> {
        if sec_type.is_empty() || permitted.is_empty() {
            return Ok(());
        }
        let ty = sec_type.to_ascii_uppercase();
        let key = if matches!(ty.as_str(), "BAG" | "COMBO") { "COMB" } else { ty.as_str() };
        if permitted.contains_key(key) {
            return Ok(());
        }
        let mut named: Vec<&str> = permitted.keys().map(String::as_str).collect();
        named.sort_unstable();
        Err(Refusal::stated(SECURITY_NOT_PERMITTED, format!(
            "the account is not permitted to trade {ty}. It is permitted: {}",
            named.join(", "),
        )))
    }

    /// An order states where it is to be filled.
    ///
    /// The venue does not choose a destination, and neither does this client:
    /// looking a contract up without one answers with whichever listing came
    /// first, which is how an order reaches a venue the caller never named.
    /// The reference client is refused by the server here, by name.
    pub fn validate_order_destination(exchange: &str) -> Result<(), String> {
        if exchange.trim().is_empty() {
            return Err(
                "an order states the exchange it is to be filled on, and this one \
                 names none".to_string(),
            );
        }
        Ok(())
    }

    /// Refuse an order whose contract does not name one contract:
    /// a symbol alone names a whole option chain.
    pub fn validate_order_contract(con_id: i64, sec_type: &str, identity: &str) -> Result<(), String> {
        if con_id != 0 {
            return Ok(());
        }
        // A currency pair is fully identified by what an order already carries:
        // symbol, currency, security type and destination. There is no expiry,
        // strike, right or multiplier to omit, so the silent mistrade this
        // guard exists to prevent cannot happen for CASH — unlike OPT and FUT,
        // whose orders would go out saying nothing about which strike or which
        // contract month. Verified against a live IDEALPRO book: limit, stop
        // limit, market-if-touched, limit-if-touched, trailing stop limit,
        // relative and hidden all acknowledge and cancel cleanly.
        // A symbol names a stock or a currency pair completely. Anything else
        // needs its expiry, strike, right or multiplier, which an order now
        // restates — so the question is not which type this is but whether the
        // caller said enough to identify one contract.
        let ty = sec_type.to_ascii_uppercase();
        // What identifies one contract differs by kind, and only two kinds need
        // more than the caller has already given.
        //
        // An option and a warrant are one of a chain, so they need the expiry,
        // strike or right that says which one. A future is one of a series and
        // needs its maturity. Everything else is named completely by its symbol
        // and the contract id and local symbol that travel with it, which is
        // how the venue itself names them on an order.
        if matches!(ty.as_str(), "OPT" | "FOP" | "WAR" | "IOPT") {
            if identity.is_empty() {
                return Err(format!(
                    "a {ty} contract needs its expiry, strike or right: the symbol alone \
                     names a whole chain, and an order stating only the symbol would be \
                     filled on whichever contract the gateway picked"
                ));
            }
            return Ok(());
        }
        if matches!(ty.as_str(), "FUT" | "FWD") {
            if identity.is_empty() {
                return Err(format!(
                    "a {ty} contract needs its maturity: the symbol alone names a series, \
                     and an order stating only the symbol would be filled on whichever \
                     contract the gateway picked"
                ));
            }
            return Ok(());
        }
        // A combination states its legs on the order itself, so it needs no
        // identity of its own here. An order that names one and states no legs
        // is refused by `validate_combo_legs` before this.
        Ok(())
    }

    /// What an exercise states, checked before the contract is registered so a
    /// refused one reaches nothing. Returns the action, the quantity and the
    /// account the order carries — empty for the session's own.
    ///
    /// The documented API names a third action, a hold, which the venue does
    /// not take from a client of this kind. It is refused here rather than sent
    /// and rejected, because the caller who asked for it wants to know that the
    /// position was left alone.
    ///
    /// The account is checked as a gateway checks it. A login holding several
    /// accounts has to name one it holds. A login holding one takes the
    /// exercise on that account, and a gateway looks the position up in the
    /// account named: named another, it finds nothing there and says so.
    pub fn validate_exercise(
        exercise_action: i32, exercise_quantity: i32,
        account: &str, session: &OrderSession,
    ) -> Result<(u8, u32, String), Refusal> {
        let action = match exercise_action {
            1 | 2 => exercise_action as u8,
            other => {
                return Err(format!(
                    "exercise_action {other} is not served: 1 exercises, 2 lapses"
                ).into());
            }
        };
        if exercise_quantity <= 0 {
            return Err(format!(
                "exercise_quantity {exercise_quantity} is not a number of contracts"
            ).into());
        }
        if session.holds_several_accounts() {
            if account.is_empty() {
                return Err(Refusal::validation("The account code is required for this operation."));
            }
            if !session.holds(account) {
                return Err(Refusal::validation(format!("Invalid account code '{account}'.")));
            }
            return Ok((action, exercise_quantity as u32, account.to_string()));
        }
        if !account.is_empty() && account != session.account {
            return Err(Refusal::stated(REQUEST_NOT_PROCESSED, format!(
                "Error processing request:No unlapsed position exists in this option in \
                 account {account}.",
            )));
        }
        Ok((action, exercise_quantity as u32, String::new()))
    }

    /// An exercise or a lapse, as the order the venue takes it for: the buy
    /// side, no price, and the action on the attributes so the encoder every
    /// other order goes through emits it.
    ///
    /// The override the documented signature takes is not here. It waives the
    /// check the engine makes against the option's standing in the money
    /// before the order is built, so no tag carries it and there is nothing to
    /// send.
    pub fn build_exercise_request(
        order_id: OrderId, instrument: InstrumentId, action: u8, qty: Qty,
        account: String, stated: ExerciseStates,
    ) -> OrderRequest {
        OrderRequest::SubmitEx {
            order_id,
            instrument,
            // An exercise is instructed on the slot the option holds, and the
            // caller states no contract id beside it.
            con_id: 0,
            side: Side::Buy,
            qty,
            kind: OrderKind::Limit { price: 0 },
            tif: b'0',
            attrs: OrderAttrs {
                exercise_action: action,
                account,
                manual_order_time: stated.manual_order_time,
                customer_account: stated.customer_account,
                professional_customer: stated.professional_customer,
                ..Default::default()
            },
        }
    }

    /// Build an `OrderRequest` from an API `Order`, handling all order types.
    /// This is the shared order-type match block used by both Rust and Python.
    /// A price the caller left alone is `f64::MAX`, which is not a price and
    /// does not survive being scaled into one.
    fn price_or_unset(v: f64) -> i64 {
        if v == f64::MAX { 0 } else { crate::types::price_from_f64(v) }
    }

    /// Turn what a caller set into the request the engine sends.
    pub fn build_order_request(
        order: &ApiOrder,
        order_id: u64,
        instrument: InstrumentId,
        contract: Option<&crate::types::model::Contract>,
    ) -> Result<ControlCommand, Refusal> {
        Self::build_order_request_with_kind(order, order_id, instrument, contract, None)
    }

    pub(crate) fn build_order_request_with_kind(
        order: &ApiOrder,
        order_id: u64,
        instrument: InstrumentId,
        contract: Option<&crate::types::model::Contract>,
        resolved_kind: Option<OrderKind>,
    ) -> Result<ControlCommand, Refusal> {
        let side = order.side()?;
        let qty = crate::types::qty_from_f64(order.total_quantity);
        let order_type = order.order_type_named();

        // Every order type carries its extended attributes and its time-in-force
        // through one encoder. Choosing per type between an attribute-carrying
        // request and a plain one is how an order type ends up shipping
        // without something the caller set: a bracket child that arrives
        // unlinked and immediate, an adjustable stop that never adjusts, an
        // algo that runs without its parameters.

        // The legs live on the contract, not the order, so they are attached
        // here rather than in `attrs()`.
        // The caller may price the legs separately rather than pricing the
        // combination, as a list of its own in leg order. What a gateway
        // makes of it is checked once the legs are read, below.
        let leg_prices = order.order_combo_legs.as_slice();
        // A price stated for a leg the combination does not have has nowhere
        // to go. Dropped where the lists ran past each other, the order went
        // out priced on the legs that happened to line up and the caller was
        // told it had been placed as written — which for two lists paired the
        // wrong way round is every leg priced as its neighbour. Only where the
        // contract is a combination at all: a price list on a contract with no
        // legs states nothing, and the reference client sends none either.
        let legs = contract.map_or(0, |c| c.combo_legs.len());
        if legs > 0 && leg_prices.len() > legs {
            return Err(Refusal::validation(format!(
                "the order prices {} legs and the combination has {legs}: a price stated \
                 for a leg that is not there cannot be sent",
                leg_prices.len(),
            )));
        }
        let leg_specs: Vec<crate::types::ComboLegSpec> =
            contract.map(|c| c.combo_legs.as_slice()).unwrap_or(&[]).iter().map(leg_spec).collect();
        // Prices stated for the legs, as a gateway reads them, leg by leg: a
        // priced leg is refused on any order but a limit, and an unpriced one
        // after a priced first leg. A priced first leg then refuses a limit
        // of the combination's own beside it. A combination priced on every
        // leg is one a gateway prices by its legs only where it is a
        // non-guaranteed combination of two legs routed by SMART, and refuses
        // otherwise; this client carries no routing parameter that makes one
        // non-guaranteed, so it refuses them all. Priced on later legs alone,
        // the prices are read and not sent, as a gateway sends none.
        if let Some(order_type) = order_type {
            let priced: Vec<bool> = (0..legs)
                .map(|at| leg_prices.get(at).is_some_and(|p| *p != f64::MAX))
                .collect();
            for (at, &leg_priced) in priced.iter().enumerate() {
                let (code, text) = if leg_priced && order_type != "LMT" {
                    (10055, "Only LMT or REL+LMT order allows using per-leg prices.")
                } else if !leg_priced && priced[0] {
                    (10056, "All leg prices are needed when specifying per-leg prices.")
                } else {
                    continue;
                };
                // A gateway puts the leg in front of the reason and writes
                // the reason out whole, number and both texts, and answers
                // it as a request it could not validate.
                return Err(Refusal::validation(format!(
                    "The combo details for leg '{at}' are invalid. - \
                     CodeMsgPair::[m_code={code}m_msg={text}]m_sysMsg={text}]",
                )));
            }
            if priced.first() == Some(&true) {
                return Err(if order.lmt_price != 0.0 {
                    Refusal::stated(COMBO_AND_LEG_PRICES, "Can't specify combo price when using per-leg prices.")
                } else {
                    Refusal::stated(
                        PER_LEG_PRICES_UNSUPPORTED,
                        "Combo per-leg prices are only supported for non-guaranteed smart \
                         combo with two legs and feature \"IECOMBOPERLEGPRICE\" enabled.",
                    )
                });
            }
        }
        // The contract the caller named, so the engine can see that the slot
        // beside it is no longer the one they meant.
        let con_id = contract.map_or(0, |c| c.con_id);
        let ex = |kind: OrderKind| OrderRequest::SubmitEx {
            order_id, instrument, con_id, side, qty,
            kind,
            tif: order.tif_byte(),
            attrs: crate::types::OrderAttrs {
                combo_legs: leg_specs.clone(),
                // The listing exchange and the hedging contract are stated on
                // the contract, not the order, so they are picked up here.
                primary_exchange: contract
                    .map(|c| c.primary_exchange.clone()).unwrap_or_default(),
                delta_neutral_contract: contract
                    .and_then(|c| c.delta_neutral_contract.as_ref())
                    .map(|d| Box::new(crate::types::DeltaNeutralContractSpec {
                        con_id: d.con_id,
                        delta: d.delta,
                        price: d.price,
                    })),
                ..order.attrs()
            },
        };

        if let Some(kind) = resolved_kind {
            return Ok(ControlCommand::Order(ex(kind)));
        }

        // Adaptive orders (special-cased before generic algo)
        if order.algo_strategy.eq_ignore_ascii_case("Adaptive") {
            let price = crate::types::price_from_f64(order.lmt_price);
            let priority = adaptive_priority(&order.algo_params)?;
            return Ok(ControlCommand::Order(ex(OrderKind::Adaptive { price, priority })));
        }

        // Algo orders
        if !order.algo_strategy.is_empty() {
            let algo = crate::client_core::parse_algo_params(&order.algo_strategy, &order.algo_params)?;
            let price = crate::types::price_from_f64(order.lmt_price);
            return Ok(ControlCommand::Order(ex(OrderKind::Algo { price, algo })));
        }

        // Adjustable stop: a base STP that converts to another order type when
        // its trigger is reached. Signalled by a non-empty adjustedOrderType,
        // which is empty on every ordinary order, so this affects nothing else.
        // A Trail/TrailLimit conversion carries the trailing amount + unit
        // (tags 6260/6269).
        if !order.adjusted_order_type.is_empty() {
            let adjusted = match order.adjusted_order_type.to_uppercase().as_str() {
                "STP" => AdjustedOrderType::Stop,
                "STP LMT" => AdjustedOrderType::StopLimit,
                "TRAIL" => AdjustedOrderType::Trail,
                "TRAIL LIMIT" => AdjustedOrderType::TrailLimit,
                other => return Err(Refusal::validation(
                    format!("unknown adjustedOrderType '{other}'"),
                )),
            };
            let scale = |v: f64| crate::types::price_from_f64(v);
            // adjusted_trailing_amount defaults to f64::MAX when unset.
            let adj_trail = if order.adjusted_trailing_amount == f64::MAX {
                0.0
            } else {
                order.adjusted_trailing_amount
            };
            // Through the same construction every other order type uses, so a
            // bracket child keeps its parent link, its OCA group and its tif —
            // and so does everything the contract states rather than the order:
            // its legs, its listing exchange and the contract it hedges
            // against. Built from `order.attrs()` alone, an adjustable stop on
            // a combination reached the encoder with no legs at all.
            return Ok(ControlCommand::Order(ex(OrderKind::AdjustableStop {
                stop_price: scale(order.aux_price),
                trigger_price: scale(order.trigger_price),
                adjusted_order_type: adjusted,
                adjusted_stop_price: scale(order.adjusted_stop_price),
                adjusted_stop_limit_price: scale(order.adjusted_stop_limit_price),
                adjusted_trailing_amount: scale(adj_trail),
                adjustable_trailing_unit: order.adjustable_trailing_unit,
            })));
        }

        let Some(order_type) = order_type else {
            return Err(Self::not_an_order_type(order));
        };
        let req = match order_type {
            "MKT" => {
                ex(OrderKind::Market)
            }
            "LMT" => {
                let price = crate::types::price_from_f64(order.lmt_price);
                ex(OrderKind::Limit { price })
            }
            "STP" => {
                let stop = crate::types::price_from_f64(order.aux_price);
                ex(OrderKind::Stop { stop_price: stop })
            }
            "STP LMT" => {
                let price = crate::types::price_from_f64(order.lmt_price);
                let stop = crate::types::price_from_f64(order.aux_price);
                ex(OrderKind::StopLimit { price, stop_price: stop })
            }
            "TRAIL" => {
                // Optional initial stop trigger (tag 6117); default f64::MAX = unset.
                let trail_stop = (order.trail_stop_price != f64::MAX).then(|| crate::types::price_from_f64(order.trail_stop_price));
                // The reference client's unset value is not a percentage.
                if order.trailing_percent > 0.0 && order.trailing_percent != f64::MAX {
                    // Wire granularity is basis points, so a percentage
                    // stated finer than that is put on the nearest one.
                    // Rounded rather than cut: a hundredth of a per cent is
                    // not exactly a double, and 0.29 times a hundred is
                    // 28.999999999999996, so cutting sends 0.28 for a figure
                    // the wire can carry exactly. Five hundred and
                    // seventy-three of the ten thousand basis points went out
                    // a point low. This is the hazard `price_from_f64` names
                    // and rounds for on the price path. validate_order has
                    // already confirmed the value is finite, non-negative and
                    // fits u32 once scaled.
                    let pct = (order.trailing_percent * 100.0).round() as u32;
                    ex(OrderKind::TrailPct { trail_pct: pct, trail_stop_price: trail_stop })
                } else {
                    let trail = crate::types::price_from_f64(order.aux_price);
                    ex(OrderKind::TrailingStop { trail_amt: trail, trail_stop_price: trail_stop })
                }
            }
            "TRAIL LIMIT" => {
                // Wire-side semantic is `LimitPriceOffset` (tag 6370), not an
                // absolute limit price. Prefer `lmt_price_offset`; fall back
                // to `lmt_price` for callers that haven't migrated.
                let offset_f = if order.lmt_price_offset != f64::MAX {
                    order.lmt_price_offset
                } else {
                    order.lmt_price
                };
                let lmt_offset = crate::types::price_from_f64(offset_f);
                let trail = crate::types::price_from_f64(order.aux_price);
                let trail_stop = (order.trail_stop_price != f64::MAX).then(|| crate::types::price_from_f64(order.trail_stop_price));
                ex(OrderKind::TrailingStopLimit { lmt_offset, trail_amt: trail, trail_stop_price: trail_stop })
            }
            "MOC" => {
                ex(OrderKind::Moc)
            }
            "LOC" => {
                let price = crate::types::price_from_f64(order.lmt_price);
                ex(OrderKind::Loc { price })
            }
            "MIT" => {
                let stop = crate::types::price_from_f64(order.aux_price);
                ex(OrderKind::Mit { stop_price: stop })
            }
            "LIT" => {
                let price = crate::types::price_from_f64(order.lmt_price);
                let stop = crate::types::price_from_f64(order.aux_price);
                ex(OrderKind::Lit { price, stop_price: stop })
            }
            "MTL" | "BOX TOP" => {
                ex(OrderKind::Mtl)
            }
            "MKT PRT" => {
                ex(OrderKind::MktPrt)
            }
            "STP PRT" => {
                let stop = crate::types::price_from_f64(order.aux_price);
                ex(OrderKind::StpPrt { stop_price: stop })
            }
            // The offset is the aux price and the cap the limit price, as they
            // are for the pegged types.
            "REL" => {
                ex(OrderKind::Rel {
                    offset: crate::types::price_from_f64(order.aux_price),
                    price_cap: Self::price_or_unset(order.lmt_price),
                })
            }
            // Sits away from the best price and follows it, no further than
            // the cap the caller states. The offset is the aux price, as it is
            // for every relative type here.
            "PASSV REL" => {
                ex(OrderKind::PassiveRel {
                    offset: crate::types::price_from_f64(order.aux_price),
                    price_cap: Self::price_or_unset(order.lmt_price),
                })
            }
            // Sits at the best bid or offer, filling no worse than the price
            // the caller states. What it competes with, and how far it will
            // go towards the midpoint, are stated on the order's attributes.
            "PEG BEST" => {
                ex(OrderKind::PegBest {
                    price: crate::types::price_from_f64(order.lmt_price),
                })
            }
            // Every reference field was already carried here and then read by
            // nobody: a caller setting all six got an order that mentioned none
            // of them.
            "PEG BENCH" => {
                ex(OrderKind::PegBench {
                    price: crate::types::price_from_f64(order.lmt_price),
                    ref_con_id: order.reference_contract_id.max(0) as u32,
                    is_peg_decrease: order.is_pegged_change_amount_decrease,
                    pegged_change_amount: crate::types::price_from_f64(order.pegged_change_amount),
                    ref_change_amount: crate::types::price_from_f64(order.reference_change_amount),
                    starting_price: Self::price_or_unset(order.starting_price),
                    stock_ref_price: Self::price_or_unset(order.stock_ref_price),
                    ref_exchange: order.reference_exchange_id.clone(),
                })
            }
            "PEG MKT" => {
                let offset = crate::types::price_from_f64(order.aux_price);
                let price_cap = crate::types::price_from_f64(order.lmt_price);
                ex(OrderKind::PegMkt { offset, price_cap })
            }
            "PEG MID" => {
                let offset = crate::types::price_from_f64(order.aux_price);
                let price_cap = crate::types::price_from_f64(order.lmt_price);
                ex(OrderKind::PegMid { offset, price_cap })
            }
            "MIDPRICE" => {
                let cap = crate::types::price_from_f64(order.lmt_price);
                ex(OrderKind::MidPrice { price_cap: cap })
            }
            "SNAP MKT" => {
                let offset = crate::types::price_from_f64(order.aux_price);
                ex(OrderKind::SnapMkt { offset })
            }
            "SNAP MID" => {
                let offset = crate::types::price_from_f64(order.aux_price);
                ex(OrderKind::SnapMid { offset })
            }
            "SNAP PRIM" => {
                let offset = crate::types::price_from_f64(order.aux_price);
                ex(OrderKind::SnapPri { offset })
            }
            // Every name `order_type_named` answers is placed above.
            _ => return Err(Self::not_an_order_type(order)),
        };

        Ok(ControlCommand::Order(req))
    }
    /// The contract's terms and the venue's model for it, or why neither
    /// question can be answered.
    pub(crate) fn solve_option(
        &self,
        shared: &SharedState,
        contract: &crate::types::model::Contract,
        // The request the model is being watched for, where one is. A contract
        // stated by description carries no id, so there is nothing to look its
        // model up by — and nothing is ever kept under nought, because one
        // entry there would point every conId-less contract at the first one's
        // slot. The watch opened to obtain the model resolves the contract and
        // records the slot under the request that opened it, so that is where
        // it is found.
        //
        // Without it the lookup could not succeed however long it waited, and
        // the refusal it gave is the one refusal here that means "not yet":
        // the question was kept, re-solved on every pass, and its caller told
        // neither an answer nor a reason for the life of the session.
        //
        // `None` on a first ask, which has opened no watch yet. The number is
        // the caller's own and may already be watching something else — and
        // for a contract with no id of its own, that other contract's slot is
        // what the fallback would find. A model belonging to a different
        // contract answers as readily as the right one and is not the right
        // one.
        watched_under: Option<i64>,
        solve: impl Fn(
            crate::control::option_model::OptionTerms,
            crate::control::option_model::VenueModel,
            &[(f64, f64)],
        ) -> Option<f64>,
    ) -> Result<f64, crate::error_codes::Refusal> {
        // Forget released slots before reading the model: another contract
        // can now hold the slot this contract used to name.
        let by_its_own_id = self.cached_instrument(shared, contract.con_id);
        let instrument = by_its_own_id
            .or_else(|| watched_under.and_then(|req_id| self.watching(req_id)));
        solve_option_on(shared, instrument, contract, solve)
    }
}

/// Solve a question about an option against the venue's model for the
/// contract in `instrument`, or say why it cannot be answered.
///
/// Of this side's records only the slot is read, and it is handed in: the
/// model and the dividend schedule are the session's own. So the engine can
/// answer a kept question where it writes the model, on the slot the question's
/// watch took.
pub(crate) fn solve_option_on(
    shared: &SharedState,
    instrument: Option<InstrumentId>,
    contract: &crate::types::model::Contract,
    solve: impl Fn(
        crate::control::option_model::OptionTerms,
        crate::control::option_model::VenueModel,
        &[(f64, f64)],
    ) -> Option<f64>,
) -> Result<f64, crate::error_codes::Refusal> {
    {
        let instrument = instrument.ok_or_else(|| OPTION_MODEL_UNSTATED.to_string())?;
        let stated = shared
            .market
            .option_model(instrument)
            .ok_or_else(|| OPTION_MODEL_UNSTATED.to_string())?;
        // What the venue did not state is not a number. It writes the largest
        // double where it has nothing to say, which this client passes on
        // as-is because the reference client does — so it has to be read back
        // as silence here rather than taken for a value. Taken for one, a
        // contract with no dividend had the largest double in the world
        // subtracted from its underlying.
        let stated_or_none = |v: f64| (v.is_finite() && v != f64::MAX).then_some(v);
        // How long the contract has left, as the venue says it on the same
        // statement as the rest of its model — carried to the fraction, the
        // hours of the last day included. Counted here instead, from whole
        // days off this machine's clock, a contract expiring today has none
        // left and is refused, and so is every contract from the evening
        // before its expiry, because the clock has already turned over in
        // UTC. The venue's own count says 0.73 of a day where this said none.
        //
        // Its basis is 365: the daily rate it states beside this divides the
        // annual one by 365 exactly.
        let years = match stated_or_none(stated.cal_days) {
            Some(days) if days > 0.0 => days / 365.0,
            // Where it stated none, back to counting — a contract with hours
            // left still reads as expired, which is the old behaviour and not
            // worse than refusing outright.
            _ => years_to_expiry(&contract.last_trade_date_or_contract_month)
                .ok_or_else(|| "the contract states no expiry to measure from".to_string())?,
        };
        // The venue states which of two models it priced this contract on, and
        // this library has one of them. Answering a question about the other
        // with this one gives a number, and a number worked out under the
        // wrong distribution is worse than no answer — so it is refused by
        // name rather than quietly produced.
        if stated.price_based_vol {
            return Err(crate::error_codes::Refusal::validation(
                "the venue priced this contract on a volatility stated in its own price \
                 units, which is a different model from the one this client solves with. \
                 Its own figures for the contract are on the model tick and stand as \
                 stated; what cannot be answered is a question about a price or a \
                 volatility other than the ones it published",
            ));
        }
        // The strike the same way every other figure here is read: the largest
        // double is this venue's word for "not stated", and a contract with no
        // strike is not a contract struck at the largest number a double
        // carries. Taken for one, a call there is worth nothing and was
        // answered as exactly that.
        let strike = stated_or_none(contract.strike)
            .filter(|k| *k > 0.0)
            .ok_or_else(|| "the contract states no strike to solve at".to_string())?;
        // A call or a put, and nothing else. Read as "a call if it says call",
        // an option stating a right this client does not know — or stating
        // none at all — was priced as a put, and a put's price for a contract
        // whose pay-off nobody here can name is a number made up about it.
        enum Right { Call, Put }
        let right = match contract.right.to_ascii_uppercase().as_str() {
            "C" | "CALL" => Right::Call,
            "P" | "PUT" => Right::Put,
            other => {
                return Err(crate::error_codes::Refusal::validation(format!(
                    "this contract states its right as {other:?}, which is neither a call \
                     nor a put, so there is no pay-off to price"
                )));
            }
        };
        let terms = crate::control::option_model::OptionTerms {
            strike,
            years_to_expiry: years,
            is_call: match right {
                Right::Call => true,
                Right::Put => false,
            },
            // The venue's own calculator tells these apart, and so must this:
            // an option on a future is priced on one that drifts nowhere and
            // settles at expiry.
            on_a_future: contract.sec_type.eq_ignore_ascii_case("FOP"),
        };
        let model = crate::control::option_model::VenueModel {
            volatility: stated_or_none(stated.implied_vol)
                .ok_or_else(|| OPTION_MODEL_UNSTATED.to_string())?,
            option_price: stated_or_none(stated.opt_price)
                .ok_or_else(|| OPTION_MODEL_UNSTATED.to_string())?,
            underlying_price: stated_or_none(stated.und_price)
                .ok_or_else(|| OPTION_MODEL_UNSTATED.to_string())?,
            // No dividend stated is no dividend, which is what it means.
            present_value_of_dividends: stated_or_none(stated.pv_dividend).unwrap_or(0.0),
            // No rate stated is no discount, which over the days one of these
            // has left moves the price by less than it is quoted in.
            rate: stated_or_none(stated.rate).unwrap_or(0.0),
            // The venue states no yield, and on an underlying whose dividends
            // it carries as one rather than as a present value, leaving it at
            // nothing prices every call dear and every put cheap by the whole
            // of it. Recovered from the price the venue itself published.
            yield_rate: 0.0,
        };
        // What the underlying pays out over the option's life, where the venue
        // has stated its schedule. It states one per contract, and the one
        // that matters is the underlying's rather than the option's — so it is
        // looked up by what the definition said this option is written on.
        let schedule = shared
            .reference
            .under_con_id(contract.con_id as u32)
            .and_then(|under| shared.reference.dividend_schedule(under))
            .map(|schedule| {
                let today = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|since| (since.as_secs() / 86_400) as i64)
                    .unwrap_or(0);
                // The far end of the window is the contract's own expiry date,
                // which the caller named. Taken off the count of days the
                // venue states the contract has left — a fraction, rounded up
                // — it was the day after expiry for every hour of a session on
                // a contract expiring at the close, and a payment going ex
                // that day was priced into a contract that never sees it.
                let expires_on = crate::protocol::datetime::day_number(
                    &contract.last_trade_date_or_contract_month,
                );
                expires_on
                    .map(|expires_on| {
                        crate::control::dividends::over_the_life(
                            &schedule, today, expires_on, &contract.currency,
                        )
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        // The carry is solved for on top of whatever the schedule says, not
        // instead of it: the schedule states when the underlying drops and by
        // how much, and the carry puts the price back on the figure the venue
        // published. A recovery that fails is not nought — it means no carry in
        // a plausible range reproduces that figure, so the model is not the
        // venue's and a number worked out on it would be this client's own with
        // the venue's name on it. Taken as nought, that is exactly what was
        // published.
        //
        // A future is the exception: the tree prices one on a price that
        // drifts nowhere, so what the underlying yields never enters the
        // arithmetic and nought is what it uses either way.
        let solved = if terms.on_a_future {
            solve(terms, model, &schedule)
        } else {
            crate::control::option_model::recover_yield(terms, model, &schedule)
                .and_then(|yield_rate| {
                    solve(
                        terms,
                        crate::control::option_model::VenueModel { yield_rate, ..model },
                        &schedule,
                    )
                })
        };
        solved.ok_or_else(|| {
            crate::error_codes::Refusal::validation(
            "this contract cannot be solved under the venue's model for it. The model is \
             anchored to the price the venue published, so a figure no rate reproduces leaves \
             nothing to solve against — and an option far enough into the money is worth its \
             intrinsic value and little else, its price hardly moving with volatility at all, \
             so no one volatility is implied either. Naming a number for either would be \
             picking one rather than solving for it")
        })
    }
}

/// Answer the calculations kept for a model, now that one may be stated.
///
/// Called by the engine where it writes a contract's model — `slot` — so each
/// answer is pushed right behind the model it was solved against and before
/// anything the engine pushes later, the session's last record included; and
/// by a call that has just kept one — `only` — in case the model arrived while
/// it was keeping it. The answer goes on 53 under the question's own number; a
/// question the model cannot answer is refused under it; one still waiting on
/// a model stays kept.
pub fn answer_kept_calculations(
    shared: &SharedState, slot: Option<InstrumentId>, only: Option<i64>,
) {
    shared.market.answer_kept_calculations(
        |req_id, kept| slot.is_none_or(|slot| kept.slot == slot) && only.is_none_or(|id| id == req_id),
        |req_id, kept| {
            let (asked, und) = (kept.option_price, kept.under_price);
            let solved = solve_option_on(shared, Some(kept.slot), &kept.contract, |terms, model, schedule| {
                if kept.wants_volatility {
                    crate::control::option_model::implied_volatility(terms, model, schedule, asked, und)
                } else {
                    crate::control::option_model::option_price(terms, model, schedule, asked, und)
                }
            });
            match solved {
                Ok(figure) => {
                    let (implied_vol, opt_price) = if kept.wants_volatility {
                        (figure, asked)
                    } else {
                        (asked, figure)
                    };
                    shared.market.push_option_computation(crate::types::OptionComputation {
                        implied_vol,
                        opt_price,
                        und_price: und,
                        ..crate::types::OptionComputation::solved(req_id)
                    });
                    true
                }
                // Only one of the refusals resolves by waiting: the one saying
                // the venue has not stated its model yet. The rest are
                // permanent, and read as "not yet" they leave the question
                // kept for the life of the session with its caller told
                // neither an answer nor a reason.
                Err(why) if why.message == OPTION_MODEL_UNSTATED => false,
                Err(why) => {
                    shared.push_refused(
                        crate::types::model::ErrorOrigin::Request { id: req_id, ends: true },
                        i64::from(why.code), why.message,
                    );
                    true
                }
            }
        },
    );
}


/// A whole number as a gateway reads one: an optional sign, then digits,
/// within thirty-two bits.
fn gateway_int(text: &str) -> Option<i32> {
    text.parse().ok()
}

/// `yyyymmdd-hh:mm:ss`, read strictly, as a gateway reads the UTC form.
fn utc_time_reads(time: &str) -> bool {
    let b = time.as_bytes();
    let digits = |from: usize, to: usize| b[from..to].iter().all(u8::is_ascii_digit);
    let n = |from: usize, to: usize| time[from..to].parse::<u32>().unwrap_or(u32::MAX);
    b.len() == 17 && b[8] == b'-' && b[11] == b':' && b[14] == b':'
        && digits(0, 8) && digits(9, 11) && digits(12, 14) && digits(15, 17)
        && n(0, 4) >= 1
        && (1..=12).contains(&n(4, 6))
        // A day past the month's end is moved back to it, and midnight may be
        // written as the end of the day before, when the parse is read.
        && (1..=31).contains(&n(6, 8))
        && (n(9, 11) <= 23 || (n(9, 11) == 24 && n(12, 14) == 0 && n(15, 17) == 0))
        && n(12, 14) <= 59 && n(15, 17) <= 59
}

/// `[yyyymmdd ]hh:mm:ss[ zone]`, read as a gateway reads the local form.
///
/// The string is trimmed, and every word after a space that does not start
/// with a digit is taken as the zone. That many characters, and one more,
/// are cut from the end; what is left is a date and a time where it holds a
/// space, and a time alone where it does not.
fn local_time_reads(time: &str) -> bool {
    let time = time.trim_matches(|c: char| c <= ' ');
    let mut zone = String::new();
    let mut rest = time;
    while let Some(at) = rest.rfind(' ') {
        let word = &rest[at + 1..];
        rest = rest[..at].trim_matches(|c: char| c <= ' ');
        if !word.starts_with(|c: char| c.is_ascii_digit()) {
            zone = if zone.is_empty() { word.to_string() } else { format!("{word} {zone}") };
        }
    }
    let time = if zone.is_empty() { time } else { &time[..time.len() - zone.len() - 1] };
    match time.split_once(' ') {
        Some((date, clock)) => clock_reads(clock) && date_reads(date),
        None => clock_reads(time),
    }
}

/// `hh:mm:ss`, each a number in range, the pieces between colons read with
/// empty ones skipped.
fn clock_reads(clock: &str) -> bool {
    let mut pieces = clock.split(':').filter(|p| !p.is_empty());
    let mut next = || pieces.next().and_then(gateway_int);
    let (Some(h), Some(m), Some(s)) = (next(), next(), next()) else { return false };
    (0..=23).contains(&h) && (0..=59).contains(&m) && (0..=59).contains(&s) && pieces.next().is_none()
}

/// `yyyymmdd`: eight characters, the year 1978 to 3000, the month 1 to 12 and
/// the day 0 to 31, each read as a number.
fn date_reads(date: &str) -> bool {
    let n = |from: usize, to: usize| date.get(from..to).and_then(gateway_int);
    date.len() == 8
        && n(0, 4).is_some_and(|y| (1978..=3000).contains(&y))
        && n(4, 6).is_some_and(|m| (1..=12).contains(&m))
        && n(6, 8).is_some_and(|d| (0..=31).contains(&d))
}

#[cfg(test)]
mod tests;

// ── Opening a session ──
//
// Both surfaces open a session the same way and then hold what comes back
// differently. What they do identically is written once here.

/// Remember this session, so the next start does not need a second factor.
///
/// Best effort: a session that cannot be written is a slower start next time,
/// never a failed connect now. Called with the file the caller named, and does
/// nothing when they named none.
pub fn remember_session(
    file: Option<&std::path::Path>,
    password: &str,
    gateway: &crate::gateway::Gateway,
    username: &str,
    paper: bool,
) -> crate::auth::resume::ResumableSession {
    let session = crate::auth::resume::ResumableSession {
        token: crate::auth::crypto::strip_leading_zeros(
            &gateway.session_token.to_bytes_be(),
        ).to_vec(),
        server_session_id: gateway.server_session_id.clone(),
        hw_info: gateway.hw_info.clone(),
        encoded: gateway.encoded.clone(),
        username: username.to_string(),
        paper,
    };
    if let Some(path) = file
        && let Err(e) = crate::auth::resume::save(path, password, &session)
    {
        log::warn!("session not saved to {}: {e}", path.display());
    }
    session
}

fn leg_spec(leg: &crate::types::model::ComboLeg) -> crate::types::ComboLegSpec {
    crate::types::ComboLegSpec {
        con_id: leg.con_id,
        ratio: leg.ratio.max(0) as u32,
        is_sell: leg.action.eq_ignore_ascii_case("SELL"),
        exchange: if leg.exchange.eq_ignore_ascii_case("SMART") {
            String::new()
        } else {
            leg.exchange.clone()
        },
        open_close: leg.open_close.clamp(0, 255) as u8,
        short_sale_slot: leg.shorting_policy.clamp(0, 255) as u8,
        designated_location: leg.designated_location.clone(),
        exempt_code: leg.exempt_code,
        price: None,
    }
}
