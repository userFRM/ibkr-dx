//! ibapi-compatible EClient — Rust equivalent of C++ `EClientSocket`.
//!
//! Connects to IB, provides ibapi-matching method signatures, and dispatches
//! events to a [`Wrapper`] via `process_msgs()`.
//!
//! ```no_run
//! use ibkr_dx::api::{EClient, EClientConfig, Wrapper, Contract, Order};
//! use ibkr_dx::api::types::TickAttrib;
//!
//! struct MyWrapper;
//! impl Wrapper for MyWrapper {
//!     fn tick_price(&mut self, req_id: i64, tick_type: i32, price: f64, attrib: &TickAttrib) {
//!         println!("tick_price: req_id={req_id} type={tick_type} price={price}");
//!     }
//! }
//!
//! let mut client = EClient::connect(&EClientConfig {
//!     username: "user".into(),
//!     password: "pass".into(),
//!     host: "your_ib_host".into(),
//!     paper: true,
//!     core_id: None,
//!     ..Default::default()
//! }).unwrap();
//!
//! // Returns once the request is taken. What the engine answers — the ticks,
//! // or a refusal on `error` — arrives through `process_msgs`, in order.
//! client.req_mkt_data(1, &Contract { con_id: 756733, symbol: "SPY".into(), ..Default::default() },
//!     "", false, false);
//!
//! let mut wrapper = MyWrapper;
//! loop {
//!     client.process_msgs(&mut wrapper);
//! }
//! ```

pub(crate) mod ask;
mod simple;
pub use ask::{AccountValue, OptionChain, OrderReport, PositionRow, ScanRow, Schedule};
mod market_data;
mod orders;
mod account;
pub(crate) mod reference;
pub(crate) mod dispatch;
mod stubs;

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod origin_tests;
#[cfg(test)]
mod lifecycle_tests;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::api::wrapper::Wrapper;
use crate::control::adjustments::{AdjustedContract, Adjustment};
use crate::error_codes::Refusal;
use std::sync::{Arc, Mutex};
use std::thread;

use std::sync::mpsc::{Receiver, Sender};

use crate::types::model::{
    Contract as ApiContract, Order as ApiOrder, TagValue as ApiTagValue,
};
use crate::bridge::{Event, SharedState};
use crate::engine::hot_loop::EventSink;
use crate::client_core::ClientCore;
use crate::gateway::{Gateway, GatewayConfig, Session};
use crate::types::*;

// Re-export as public type names for the API surface
/// The contract type both surfaces share.
pub type Contract = ApiContract;
/// The order type both surfaces share.
pub type Order = ApiOrder;
/// One named option carried by a request.
pub type TagValue = ApiTagValue;

// Re-export public items from submodules
// Reads an order the caller composed, so it lives with the order model;
// reachable here because that is the path callers know it by.
pub use crate::client_core::parse_algo_params;

/// Configuration for connecting to IB via EClient.
///
/// # Live logins block on second-factor approval
///
/// With `paper: false`, [`connect()`](EClient::connect) enters a second-factor
/// approval window and **blocks** until the factor is approved or the attempt
/// runs out of time. This is expected — it is a human approval gate, not a
/// hang. How long the venue allows has not been timed here. Bound or avoid it by using `paper: true`, lowering
/// the timeout (via [`GatewayConfig::ib_key_timeout_secs`] when building through
/// the lower-level API), or supplying a `code_provider`. Paper logins skip the
/// gate entirely. An `info`-level log line is emitted when the wait begins
/// (`RUST_LOG=info`).
///
/// # Multiple engines per process
///
/// Multiple `EClient` instances can run concurrently in one process. Each owns
/// its own state, sockets, and `ib-engine-hotloop` thread; nothing is shared
/// between them, and `connect()` does not serialize across instances. If you
/// pin engines with `core_id`, give each a **distinct** value — pinning two hot
/// loops to the same core makes them busy-poll the same CPU and starve each
/// other (degraded throughput, not a hang). With `core_id: None` (the default)
/// no pinning happens and there is no conflict.
#[derive(Default)]
pub struct EClientConfig {
    /// The login.
    pub username: String,
    /// Its password. Held only for the length of the logon.
    pub password: String,
    /// Where to start the session.
    ///
    /// Leave it empty. A login is enough: the venue answers the first message
    /// by naming which server this account belongs on, and the session moves
    /// there — so what is named here is only where to knock. Name one for a
    /// test, or to knock at a particular region.
    pub host: String,
    /// The API client id this session connects as, as a gateway is given one
    /// at connect.
    ///
    /// Every order the session places goes out under it and is reported under
    /// it, on `open_order` and on `order_status`; the next order id kept across
    /// sessions is kept for it; and `req_auto_open_orders` is refused to any
    /// client but 0, as a gateway refuses it. Zero unless set.
    pub client_id: i32,
    /// Refuse to send anything that places, changes or withdraws an order.
    ///
    /// The gateway had this as a setting of its own, and the other client here
    /// takes it on connect. A Rust caller could not state it at all, so a
    /// session meant to only look could still trade.
    pub readonly: bool,
    /// What a gateway holds in its own configuration file.
    ///
    /// A gateway is a process configured by a file beside it; this client is a
    /// library, so those settings are stated here instead of in a file nobody
    /// writes. Applied as the session opens, and for the whole process
    /// [`GatewaySettings`](crate::settings::GatewaySettings).
    pub gateway: crate::settings::GatewaySettings,
    /// `false` enters the live second-factor approval gate on connect (blocking).
    /// `true` skips it. See the type-level docs.
    pub paper: bool,
    /// CPU core to pin this engine's hot loop to. `None` = no pinning. When
    /// running multiple engines, use a **distinct** core per engine.
    pub core_id: Option<usize>,
    /// Supplies the second-factor code. Required for accounts whose factor is
    /// an authenticator code — those have no push to fall back to, and connect
    /// fails without it. For IBKey accounts it selects Challenge/Response over
    /// waiting for a mobile push, so `None` is fine there.
    pub code_provider: Option<crate::auth::session::CodeProvider>,
    /// Offer a session captured earlier, instead of logging in again.
    ///
    /// Take it from [`session()`](EClient::session). The connect names that
    /// session and the server chooses: a challenge it can answer from the
    /// session alone, or the ordinary login. Whatever it chooses, this connects
    /// — a session it will not take costs a login, never an error.
    ///
    /// **The servers reached from here have not yet chosen the challenge.**
    /// Every observed login, including one offering a session left by a process
    /// that was killed rather than closed, has been answered with the ordinary
    /// one. The client asks and handles both answers, because the protocol
    /// carries both and this library already answers the challenge when a
    /// dropped connection is rebuilt. Set this and lose nothing; do not plan
    /// around it skipping a second factor until you have seen it do so.
    pub resume: Option<crate::auth::resume::ResumableSession>,
    /// What to do about a dropped connection.
    ///
    /// The default recovers on its own and keeps trying, which is what a
    /// process that must stay up wants and what having no gateway makes this
    /// library's job. Set it to bound the effort, or to be told about a loss
    /// and decide yourself.
    /// [`ReconnectConfig`](crate::reliability::ReconnectConfig).
    pub reconnect: crate::reliability::ReconnectConfig,
    /// Keep the session in this file, so a restart can offer it without a
    /// person present.
    ///
    /// Off unless set, and worth leaving off for now. Nothing about the session
    /// touches disk otherwise: it is held in memory for the life of the
    /// process, which is all a reconnect needs. Setting this writes a
    /// credential to disk, sealed under the account password and readable only
    /// by its owner, and buys whatever [`resume`](EClientConfig::resume) buys —
    /// which today, on the servers reached from here, is nothing. That is a
    /// cost with no measured return, so it is a decision rather than a default.
    pub session_file: Option<std::path::PathBuf>,
    /// Set to take the connect back.
    ///
    /// Read through the whole of [`connect`](EClient::connect): inside each
    /// dial, handshake, read and write, which wake at most a second apart to
    /// read it; between the second factor's polls; and before and after each
    /// name lookup and each `code_provider` call, neither of which can be cut
    /// short — a lookup is bounded by the system's resolver and a provider by
    /// nothing this client sets, so one that blocks holds the take-back until
    /// it returns. Seen once the session is open, the session is logged out and
    /// the engine stopped before `connect` returns. A connect taken back
    /// returns an error saying so.
    ///
    /// Also read by [`next_shared_id_within`](EClient::next_shared_id_within).
    pub cancel: Option<Arc<AtomicBool>>,
}

/// How the engine's thread ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineEnd {
    /// The thread returned normally.
    Ended,
    /// The thread, or its engine loop, panicked.
    Panicked,
}

/// The session's state once its engine thread has ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shutdown {
    /// Whether the session's logout was written successfully.
    pub logout_sent: bool,
    /// How the engine ended. Either value confirms the thread has ended.
    pub engine: EngineEnd,
}

/// ibapi-compatible EClient. Matches C++ `EClientSocket` method signatures.
///
/// # Thread lifecycle
///
/// `connect()` spawns a single `ib-engine-hotloop` background thread.
/// The thread is **joined** on [`disconnect()`](EClient::disconnect) and on [`Drop`].
/// Dropping an `EClient` without calling `disconnect()` first is safe:
/// the `Drop` impl sends `Shutdown` and joins the thread.
///
/// # Losing the connection
///
/// A loss the engine is working to recover is said under 1100, and its
/// recovery under 1102. When the session ends — [`disconnect()`](EClient::disconnect),
/// a recovery given up on, or the hot loop panicking — the engine's last
/// record is delivered as
/// [`connection_closed`](crate::api::wrapper::Wrapper::connection_closed), once,
/// after everything the session queued before it and after the last quotes
/// and figures, and nothing is delivered after it.
///
/// # One order
///
/// Every request and cancel takes its command and returns. What the engine
/// answers — a refusal, an answer given without the venue, the venue's own
/// reply — is delivered by [`process_msgs()`](EClient::process_msgs), in the
/// order the engine took it in, after everything pushed before the call.
pub struct EClient {
    pub(crate) shared: Arc<SharedState>,
    pub(crate) control_tx: Sender<ControlCommand>,
    pub(crate) thread: Joiner,
    /// The account this session acts for.
    pub account_id: String,
    /// Every account this login holds, the first being [`EClient::account_id`].
    pub accounts: Vec<String>,
    pub(crate) connected: AtomicBool,
    /// Whether the session's last record has been delivered. Nothing is
    /// delivered for the session after it.
    pub(crate) ended: AtomicBool,
    /// Whether this client pushes the session's last record itself, after its
    /// join: a client assembled from parts runs no engine of its own to push
    /// it.
    pub(crate) closes_itself: bool,
    /// Whether the caller asked for positions and has not withdrawn the ask.
    ///
    /// `req_positions` subscribes to a real-time feed, so a holding that
    /// moves afterwards is reported as it moves rather than only in the set
    /// held when the call was made.
    pub(crate) positions_requested: AtomicBool,
    /// Order records kept back because a fill for them was still queued.
    ///
    /// A fill is read against the record, so the record cannot be freed while
    /// one is waiting. Freed on the next read of the completed orders, which
    /// is when the fill has been delivered — so the deferral costs a pass and
    /// not the rest of the session.
    pub(crate) deferred_evictions: Mutex<std::collections::HashSet<u64>>,
    /// The requests watching holdings per account or model.
    ///
    /// `positionMulti` is the same live feed as `position`, asked for under a
    /// request id and withdrawn under it. Held apart from that flag because
    /// both may be watching at once and each is answered on its own callback.
    /// The requests watching the account's figures per account or model.
    ///
    /// `accountUpdateMulti` is a subscription, not a question: a figure that
    /// moves after the first batch is reported again under the same request
    /// until the caller withdraws it. Held apart from the plain account
    /// subscription beside it, because both may be open at once and each is
    /// answered on its own callback.
    /// Zero until the working orders the venue names at connect have been
    /// read and an id above them settled on.
    pub(crate) next_order_id: Arc<AtomicU64>,
    /// One question at a time.
    ///
    /// A question drives the message pump itself, and the pump hands every
    /// message it drains to the collector that is running. A collector keeps
    /// what carries its own request id and discards the rest, so a second
    /// question asked while the first is pumping has its answer read and thrown
    /// away by the first, and waits out its timeout for a reply that already
    /// came. Held across the sending too, so the second question is not on the
    /// wire while the first is listening.
    pub(crate) asking: Mutex<()>,
    /// Where the callbacks a question does not care about are also delivered.
    ///
    /// A question holds the read turn and pumps into its own collector, and
    /// the queues empty as they are read — so a fill or a trade arriving while
    /// one runs reached a collector that ignores it and was gone. A session
    /// keeping a running record installs it here, and both are fed.
    ///
    /// Empty on a bare client, which has no record of its own to keep.
    pub(crate) kept: Mutex<Option<std::sync::Arc<Mutex<dyn crate::api::wrapper::Wrapper + Send>>>>,
    /// How many events the channel above discarded, if one is attached.
    pub(crate) discarded: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The orders the venue has finished with, as they were reported.
    ///
    /// The queue they arrive on empties as it is read and the venue does not
    /// send them again, so what has been read once is kept here: asked a
    /// second time, this client answered with none of them, which reads as an
    /// account that completed nothing today. Each is kept with the venue's
    /// own name for it, which tells it from another order finished under the
    /// same number.
    pub(crate) completed: Mutex<Vec<(ApiContract, ApiOrder, crate::types::model::OrderState, String)>>,
    pub(crate) core: ClientCore,
    pub(crate) session_token_bytes: Vec<u8>,
    pub(crate) session: crate::auth::resume::ResumableSession,
    /// When the venue stamped this session's logon, in its own spelling:
    /// `yyyyMMdd-HH:mm:ss`, GMT. Empty where nothing stamped one.
    pub(crate) logged_in_at: String,
    /// What takes a logon back: the config's `cancel`, which the wait for the
    /// replay also reads.
    pub(crate) cancel: Option<Arc<AtomicBool>>,
}

impl Drop for EClient {
    fn drop(&mut self) {
        if self.thread.id == thread::current().id() {
            self.stop();
        } else {
            self.disconnect();
        }
    }
}

/// A failed thread start must still log the opened session out.
struct PreparedEngine(Option<crate::engine::hot_loop::HotLoop>);

impl Drop for PreparedEngine {
    fn drop(&mut self) {
        if let Some(engine) = self.0.as_mut() { engine.close_before_start(); }
    }
}

/// Where the engine's thread stands, as the callers that stop it see it.
pub(crate) enum Join {
    /// Running, and nobody has taken its handle to join it.
    Running(thread::JoinHandle<()>),
    /// A caller holds the handle and is joining it.
    Joining,
    /// The thread has ended.
    Ended,
}

/// The engine's thread, joined once by whichever caller gets there first, and
/// waited out by every other.
///
/// A caller that finds it running takes the handle and joins it outside the
/// lock; one that finds it being joined waits until it has ended. So every
/// caller returns only once the thread has ended, and a second caller no
/// longer returns early because the first took the handle. Every lock here is
/// taken through a poisoned one, so a stop never panics.
pub(crate) struct Joiner {
    id: thread::ThreadId,
    state: Mutex<Join>,
    ended: std::sync::Condvar,
    panicked: AtomicBool,
}

impl Joiner {
    pub(crate) fn new(handle: thread::JoinHandle<()>) -> Self {
        Self { id: handle.thread().id(), state: Mutex::new(Join::Running(handle)), ended: std::sync::Condvar::new(), panicked: AtomicBool::new(false) }
    }

    /// Return once the engine's thread has ended: joined here, or by the
    /// caller already joining it.
    pub(crate) fn join(&self) -> EngineEnd {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match std::mem::replace(&mut *state, Join::Joining) {
                Join::Running(handle) => {
                    drop(state);
                    JoinGuard { joiner: self, handle: Some(handle) }.join();
                    return self.outcome();
                }
                Join::Joining => {
                    state = self.ended.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                Join::Ended => {
                    *state = Join::Ended;
                    return self.outcome();
                }
            }
        }
    }

    fn outcome(&self) -> EngineEnd {
        if self.panicked.load(Ordering::Acquire) { EngineEnd::Panicked } else { EngineEnd::Ended }
    }

}

/// The handle, held by the caller joining it.
///
/// Once the join has returned the thread has ended, and every waiter is told.
/// Unwound before that, the handle is put back and the waiters told, so one of
/// them takes the join over rather than waiting for a join nobody is doing.
struct JoinGuard<'a> {
    joiner: &'a Joiner,
    handle: Option<thread::JoinHandle<()>>,
}

impl JoinGuard<'_> {
    fn join(mut self) {
        #[cfg(test)]
        crate::bridge::hooks::run(&crate::bridge::hooks::BEFORE_THE_JOIN);
        if let Some(handle) = self.handle.take() {
            // A thread that panicked has ended all the same.
            if let Err(payload) = handle.join() {
                self.joiner.panicked.store(true, Ordering::Release);
                // A panic payload can itself panic when dropped.
                std::mem::forget(payload);
            }
        }
    }
}

impl Drop for JoinGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.joiner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = match self.handle.take() {
            Some(handle) => Join::Running(handle),
            None => Join::Ended,
        };
        self.joiner.ended.notify_all();
    }
}

thread_local! {
    /// Whether this thread is inside one of the calls that answer.
    ///
    /// Those number themselves in a band of their own and reach the wire
    /// through the same requests a caller does, so the number alone cannot say
    /// which of the two is asking. Set for the length of the call and read
    /// where a caller's number is narrowed.
    static ANSWERING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread is inside a call that answers.
pub(crate) fn answering_now() -> bool {
    ANSWERING.with(std::cell::Cell::get)
}

/// Whether an order number is one a call that answers took for its own
/// question — a preview's — which spends no number a caller could be handed.
///
/// Spent like a caller's, it moved the allocator into the band those calls
/// number themselves in, and every number handed out after a preview was one
/// no request can carry.
pub(crate) fn a_question_of_ours(order_id: u64) -> bool {
    answering_now()
        && u32::try_from(order_id).is_ok_and(crate::bridge::ReferenceState::is_ask_id)
}

/// Mark this thread as inside a call that answers, until dropped.
pub(crate) struct Answering(bool);

impl Answering {
    pub(crate) fn begin() -> Self {
        Self(ANSWERING.replace(true))
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        ANSWERING.set(self.0);
    }
}

thread_local! {
    /// Which session this thread is inside a read of, if any.
    ///
    /// A read hands every message it drains to a wrapper, and a wrapper is a
    /// caller's own code. A question asked from inside one waits for the turn
    /// that same thread is already holding, on a lock that is not re-entrant,
    /// and the read that would have answered it is the one it is inside — so
    /// neither ever runs again, and the deadline that would have said so is
    /// set inside a wait that never starts.
    ///
    /// Held as the session being read rather than as a flag, so a callback on
    /// one session may still ask a question of another.
    static READING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Which session this thread is inside a read of. Zero: none.
pub(crate) fn reading_now() -> usize {
    READING.with(std::cell::Cell::get)
}

/// Mark this thread as inside a read of one session, until dropped.
pub(crate) struct Reading(usize);

impl Reading {
    pub(crate) fn begin(session: usize) -> Self {
        Self(READING.replace(session))
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        READING.set(self.0);
    }
}

/// Narrow a caller's req_id to the width the request carries on the wire.
///
/// `EClient` takes req_id as `i64` for ibapi parity, but these requests encode
/// it as a `u32`, and the callbacks report back whatever was encoded. A cast
/// would answer under an id the caller never used — and `next_order_id()`
/// hands out ids well past `u32::MAX`, so the ibapi idiom of one counter for
/// orders and requests hit it on the first call. Refuse instead.
pub(crate) fn wire_req_id(req_id: i64) -> Result<u32, Refusal> {
    let id = u32::try_from(req_id).map_err(|_| {
        Refusal::validation(format!(
            "req_id {req_id} is outside the range this request can carry (0..={})", u32::MAX,
        ))
    })?;
    // The band the answering calls number themselves in is not a caller's to
    // use. An answer is handed to whoever is waiting under its number, so a
    // request numbered inside it has its answer taken by one of those calls,
    // about something it did not ask for — and that call loses its own.
    //
    // Told apart by who is asking rather than by the number, because the number
    // is held precisely while the collision is possible: an answering call
    // marks itself for the length of its own call, and nothing else can.
    if crate::bridge::ReferenceState::is_ask_id(id) && !answering_now() {
        return Err(Refusal::validation(format!(
            "req_id {req_id} is inside the range this client numbers its own answering \
             calls in, and an answer under it would be taken for one of theirs: number \
             the request below {}",
            crate::bridge::ReferenceState::ASK_ID_BASE,
        )));
    }
    // The top of the range already means something: it is what this client
    // reports when a message names no request at all, and it reaches a caller
    // as minus one. A request numbered with it is answered under a number that
    // is not the one it asked under.
    if id == crate::bridge::ReferenceState::NO_REQUEST {
        return Err(Refusal::validation(format!(
            "req_id {req_id} is the number this client uses for a message that names no \
             request, and an answer under it reaches a caller as -1: number the request \
             below it",
        )));
    }
    // Above this the engine numbers the lookups it takes for itself, and the
    // answers to those are kept rather than handed on. A caller numbering a
    // request here is answered by nobody: the reply is read as internal and
    // its callbacks are suppressed, which reads as a request that vanished.
    if id >= crate::bridge::ENGINE_ID_BASE {
        return Err(Refusal::validation(format!(
            "req_id {req_id} is inside the range this client numbers the lookups it takes \
             for itself in, and an answer under it is kept rather than handed on: number \
             the request below {}",
            crate::bridge::ENGINE_ID_BASE,
        )));
    }
    Ok(id)
}

/// What [`EClient::next_shared_id`] answers, read off a session's state: both
/// surfaces answer it from here.
pub(crate) fn next_shared_id_of(shared: &SharedState) -> Result<i64, Refusal> {
    // Read after the venue has named what the account is working, as the
    // order ids are: the mark is raised by that naming, which lands after the
    // connect returns.
    shared.orders.wait_for_replay();
    shared_id_past(shared)
}

/// One past the widest id the venue has named that a request can carry, as
/// it stands now.
pub(crate) fn shared_id_past(shared: &SharedState) -> Result<i64, Refusal> {
    let next = shared.orders.narrow_id_watermark() + 1;
    // One past the widest carryable id is not itself carryable, and nor is
    // anything this client has reserved. Handing one back would number a
    // request that is answered to nobody, so the caller is told instead.
    wire_req_id(next as i64).map(|_| next as i64).map_err(|refusal| {
        Refusal::validation(format!(
            "this account has no order id left that a request can also carry: {}",
            refusal.message,
        ))
    })
}

/// Check a value a caller stated before it rides the wire as one field.
///
/// The byte that separates fields cannot sit inside one: carried anyway, the
/// value stops where the byte sits and everything after it arrives as fields
/// the caller never wrote. Nothing this protocol sends can carry the byte in
/// a value, so the request is refused rather than sent stating something
/// else.
pub(crate) fn wire_text(what: &str, value: &str) -> Result<(), Refusal> {
    if value.contains(crate::protocol::fix::SOH as char) {
        return Err(Refusal::validation(format!(
            "{what} carries the byte that separates fields on the wire, and a field \
             cannot hold it: what follows the byte would go out as fields this request \
             never stated",
        )));
    }
    Ok(())
}

/// The request a refusal is reported against, or the mark for none.
///
/// A number too wide to carry is reported against no request rather than
/// against its own low half. Narrowed, a refusal for one request is delivered
/// under another that a caller may well be waiting on — and this account hands
/// out order ids far wider than a request number, so a caller numbering both
/// from one counter reaches this with every refusal it gets.
#[cfg_attr(not(any(test, feature = "python")), allow(dead_code))]
pub(crate) fn carried_under(req_id: i64) -> u32 {
    match u32::try_from(req_id) {
        Ok(id) if id != crate::bridge::ReferenceState::NO_REQUEST => id,
        _ => crate::bridge::ReferenceState::NO_REQUEST,
    }
}

/// Whether a connect was taken back once its session opened, and if so the
/// session logged out, as the stop's goodbye logs one out: the session is
/// open at the venue from here, and a closed socket alone leaves it there.
fn logged_out_if_taken_back(cancel: Option<&AtomicBool>, trading: &mut crate::protocol::connection::Connection) -> bool {
    let taken = cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    if taken {
        let _ = trading.logout_cancelled_logon();
    }
    taken
}

/// What a connect taken back by its `cancel` ends with.
fn logon_taken_back() -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "logon cancelled by the client",
    ))
}

/// The gateway's view of an [`EClientConfig`].
///
/// Extracted so the forwarding is checkable without opening a socket: the
/// second-factor provider reaching the gateway is the whole of what makes the
/// feature usable from this client, and it is one line that a refactor can
/// drop silently.
fn gateway_config(config: &EClientConfig) -> GatewayConfig {
    GatewayConfig {
        // Settled here, once, on the caller's thread: everything downstream
        // reads a value rather than the process it happens to run in.
        settings: std::sync::Arc::new(config.gateway.resolve()),
        username: config.username.clone(),
        password: zeroize::Zeroizing::new(config.password.clone()),
        // A caller with a login should not have to know a hostname. The one
        // it would name is where every session starts anyway, and the venue
        // answers the first message by naming which server to go to — every
        // session in this codebase's logs is redirected within a second of
        // connecting. Naming one stays possible, for a test or a region.
        host: if config.host.trim().is_empty() {
            crate::config::CCP_HOSTS[0].to_string()
        } else {
            config.host.clone()
        },
        paper: config.paper,
        accept_invalid_certs: false,
        ib_key_timeout_secs: crate::auth::session::IB_KEY_DEFAULT_TIMEOUT_SECS,
        ib_key_token_sub_type: crate::auth::session::IB_KEY_DEFAULT_TOKEN_SUB_TYPE.into(),
        code_provider: config.code_provider.clone(),
        cancel: config.cancel.clone(),
        // What the caller handed back, or what was left in the file they named.
        // A file that cannot be read is a slower start, not a failed one: the
        // password is still here, and the whole point of the file is to avoid
        // needing a person, which an error thrown at one defeats.
        resume: config.resume.clone().or_else(|| {
            config.session_file.as_ref().and_then(|path| {
                crate::auth::resume::load(path, &config.username, &config.password, config.paper)
            })
        }),
    }
}

/// What a reconnect logs in with.
///
/// Extracted for the reason [`gateway_config`] is: the settings a session
/// opened under have to reach the reconnect, and building this inline left
/// them as `Default::default()`. Nothing failed until a connection went away,
/// and then the session came back announcing a different build, locale and
/// timezone, and asked the venue for every execution it holds where the caller
/// had asked for today's.
fn caller_auth(config: &EClientConfig, gateway: &GatewayConfig) -> crate::gateway::CallerAuth {
    crate::gateway::CallerAuth {
        settings: gateway.settings.clone(),
        host: config.host.clone(),
        username: config.username.clone(),
        password: zeroize::Zeroizing::new(config.password.clone()),
        paper: config.paper,
        code_provider: gateway.code_provider.clone(),
        ib_key_timeout_secs: gateway.ib_key_timeout_secs,
        ib_key_token_sub_type: gateway.ib_key_token_sub_type.clone(),
    }
}

/// What a session's two sides start from: the client it places and is reported
/// under, on both, and on the caller's side whether it may trade at all.
///
/// Extracted for the reason [`gateway_config`] is. Stated inline, the client id
/// was a zero written in two places, and every order a session placed went out
/// and came back as client 0's whatever the caller connected as.
fn session_state(config: &EClientConfig) -> (Arc<SharedState>, ClientCore) {
    let shared = Arc::new(SharedState::new());
    shared.orders.set_api_client_id(config.client_id);
    let core = ClientCore::new();
    core.set_api_client_id(config.client_id);
    // Stated before the client is handed back, so a caller cannot place
    // anything between the session opening and the setting taking hold.
    core.set_readonly(config.readonly);
    (shared, core)
}

impl EClient {
    /// Connect to IB and start the engine.
    ///
    /// The session states `next_valid_id` once, as a gateway states it to a
    /// client that has just connected: the [`process_msgs`](EClient::process_msgs)
    /// read after the venue has named what the account is working delivers it.
    pub fn connect(config: &EClientConfig) -> Result<Self, Box<dyn std::error::Error>> {
        Self::connect_inner(config, None)
    }

    /// Connect to IB and start the engine with an event channel attached.
    ///
    /// A second, optional delivery path for a program that would rather own a
    /// queue than be called back. It is bounded, and an event arriving at a
    /// full one is discarded rather than made to wait — a session that stalled
    /// on a slow reader would stop carrying market data. Read
    /// [`events_lost`](EClient::events_lost) to learn whether that happened.
    ///
    /// One reader, and it is told what it drains and nothing else. For a
    /// program that wants none of it dropped, drive
    /// [`process_msgs`](EClient::process_msgs) with a
    /// [`Wrapper`] instead: its callbacks are
    /// called with the message rather than sent a copy of it, so there is no
    /// queue to fill and nothing to fall out of one.
    ///
    /// This is a second, optional delivery path that runs alongside
    /// `process_msgs` — it does not replace it, and nothing is removed from the
    /// wrapper callbacks when it is in use.
    ///
    /// The channel is bounded by `capacity`; the engine never blocks on it, so
    /// a consumer that falls behind loses events rather than slowing the hot
    /// loop. Drain it from a thread that is not the one calling
    /// `process_msgs()`, or keep `capacity` generous.
    ///
    /// Attaching a channel makes the engine build events it would otherwise
    /// skip, which for bar batches and contract definitions means one deep copy
    /// each. Use [`connect()`](EClient::connect) when you only need the wrapper
    /// callbacks.
    pub fn connect_with_events(
        config: &EClientConfig,
        capacity: usize,
    ) -> Result<(Self, Receiver<Event>), Box<dyn std::error::Error>> {
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel(capacity.max(1));
        let lost = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut client =
            Self::connect_inner(config, Some(EventSink::new(event_tx, std::sync::Arc::clone(&lost))))?;
        client.discarded = lost;
        Ok((client, event_rx))
    }

    fn connect_inner(
        config: &EClientConfig,
        event_tx: Option<EventSink>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Before anything is logged, because the settings that say where it
        // goes are among them.
        crate::logging::apply(&config.gateway);
        let gw_config = gateway_config(config);
        let taken_back = || {
            config.cancel.as_deref().is_some_and(|c| c.load(Ordering::Relaxed))
        };

        let Session { gateway: gw, market_data: farm_conn, trading: mut ccp_conn, historical: hmds_conn, security_definition: secdef_conn } = Gateway::connect(&gw_config)?;
        if logged_out_if_taken_back(config.cancel.as_deref(), &mut ccp_conn) {
            return Err(logon_taken_back());
        }
        let account_id = gw.account_id.clone();
        let accounts = gw.accounts.clone();
        let logged_in_at = gw.logged_in_at.clone();
        let session = crate::client_core::remember_session(
            config.session_file.as_deref(),
            &config.password,
            &gw,
            &config.username,
            config.paper,
        );
        if logged_out_if_taken_back(config.cancel.as_deref(), &mut ccp_conn) {
            return Err(logon_taken_back());
        }
        let session_token_bytes = session.token.clone();

        let (shared, core) = session_state(config);
        // Before the engine's threads exist, so nothing reads a setting that
        // can still change.
        shared.set_settings(gw_config.settings.clone());
        gw.populate_init_data(&shared);
        if logged_out_if_taken_back(config.cancel.as_deref(), &mut ccp_conn) {
            return Err(logon_taken_back());
        }

        let (mut hot_loop, control_tx) = crate::engine::hot_loop::HotLoop::for_session(
            gw,
            shared.clone(), event_tx, farm_conn, ccp_conn, hmds_conn, secdef_conn, config.core_id,
            caller_auth(config, &gw_config),
        );
        // The switch a gateway carries turns recovery off wherever it is
        // stated — in code, or in the environment a program migrating from a
        // gateway already sets. What is left of `reconnect` still stands: it
        // says how hard to try, and this says whether to.
        let mut recovery = config.reconnect.clone();
        if !gw_config.settings.reconnect_on_socket_err {
            recovery.policy = crate::reliability::ReconnectPolicy::Manual;
        }
        hot_loop.set_reconnect_config(recovery);

        if taken_back() {
            hot_loop.close_before_start();
            return Err(logon_taken_back());
        }
        let mut prepared = PreparedEngine(Some(hot_loop));
        let (start, starting) = std::sync::mpsc::channel();
        let handle = thread::Builder::new()
            .name("ib-engine-hotloop".into())
            .spawn(move || {
                if starting.recv().is_ok() {
                    prepared.0.take().unwrap().run_with_panic_recovery();
                }
            })?;

        let client = Self {
            shared,
            control_tx,
            thread: Joiner::new(handle),
            account_id,
            accounts,
            connected: AtomicBool::new(true),
            ended: AtomicBool::new(false),
            closes_itself: false,
            positions_requested: AtomicBool::new(false),
            deferred_evictions: Mutex::new(std::collections::HashSet::new()),
            next_order_id: Arc::new(AtomicU64::new(0)),
            asking: Mutex::new(()),
            kept: Mutex::new(None),
            discarded: Default::default(),
            completed: Mutex::new(Vec::new()),
            core,
            session_token_bytes,
            session,
            logged_in_at,
            cancel: config.cancel.clone(),
        };
        // Taken back while the engine was being built: the engine is stopped
        // as `disconnect` stops it, logging the session out, and has ended
        // before this returns.
        client.finish_connect(start)
    }

    fn finish_connect(self, start: Sender<()>) -> Result<Self, Box<dyn std::error::Error>> {
        if self.cancel.as_deref().is_some_and(|flag| flag.load(Ordering::Acquire)) {
            drop(start);
            self.disconnect();
            return Err(logon_taken_back());
        }
        let _ = start.send(());
        // As a gateway states it to a client that has just connected: once,
        // in its place in the session's order, after the venue has named what
        // the account is working.
        self.req_ids();
        Ok(self)
    }

    /// Construct from pre-built components (for testing or custom setups).
    #[doc(hidden)]
    pub fn from_parts(
        shared: Arc<SharedState>,
        control_tx: Sender<ControlCommand>,
        handle: thread::JoinHandle<()>,
        account_id: String,
    ) -> Self {
        shared.set_session_account(&account_id);
        Self {
            shared,
            control_tx,
            thread: Joiner::new(handle),
            accounts: vec![account_id.clone()],
            account_id,
            connected: AtomicBool::new(true),
            ended: AtomicBool::new(false),
            closes_itself: true,
            positions_requested: AtomicBool::new(false),
            deferred_evictions: Mutex::new(std::collections::HashSet::new()),
            next_order_id: Arc::new(AtomicU64::new(0)),
            asking: Mutex::new(()),
            kept: Mutex::new(None),
            discarded: Default::default(),
            completed: Mutex::new(Vec::new()),
            core: ClientCore::new(),
            session_token_bytes: Vec::new(),
            session: Default::default(),
            logged_in_at: String::new(),
            cancel: None,
        }
    }

    /// Map a reqId to an InstrumentId (for testing without a live engine).
    #[doc(hidden)]
    pub fn map_req_instrument(&self, req_id: i64, instrument: InstrumentId) {
        self.core.req_to_instrument.lock().unwrap().insert(req_id, instrument);
        self.core.instrument_to_req.lock().unwrap().insert(instrument, req_id);
    }

    /// Pre-populate the order tracker (for testing the dispatcher path
    /// without going through the engine's place-order flow).
    #[doc(hidden)]
    pub fn track_order_for_test(
        &self,
        order_id: u64,
        contract: ApiContract,
        order: ApiOrder,
        instrument: InstrumentId,
    ) {
        self.core.track_order(order_id, contract, order, instrument);
    }

    /// Pre-seed a con_id → InstrumentId mapping (for testing without a live engine).
    #[doc(hidden)]
    pub fn seed_instrument(&self, con_id: i64, instrument: InstrumentId) {
        self.core.con_id_to_instrument.lock().unwrap().insert(con_id, instrument);
    }

    /// Admit a command to the engine, without waiting for it. Returns `Err`
    /// if the engine has shut down.
    pub(crate) fn send(&self, cmd: ControlCommand) -> Result<(), Refusal> {
        self.shared.admit(&self.control_tx, cmd)
    }

    /// How many commands this client has handed the engine that the engine
    /// has not finished with: still waiting to be taken, or taken and held —
    /// for the contract to be named, in the order buffer, or behind the
    /// session's own replay.
    ///
    /// No call waits for the engine to take what it is handed, so this is what
    /// bounds what a caller has handed over. A command the engine has sent,
    /// refused or withdrawn is no longer counted. Read once per lap of the
    /// engine's loop, which takes at most 64 commands a lap.
    pub fn backlog(&self) -> usize {
        self.shared.backlog()
    }

    // ── Connection ──

    /// What this session has sent and received on the venue's connections
    /// since it opened: bytes and messages, each way, across every connection
    /// it has held, those a reconnect opened included.
    ///
    /// Bytes are the protocol bytes read or written on the established
    /// connections, before TLS encryption and after decryption; messages are
    /// whole frames, including heartbeats. Authentication before a connection
    /// is established is outside these counts. What
    /// a TWS client reads as its connection's statistics.
    pub fn traffic(&self) -> Traffic {
        self.shared.traffic()
    }

    /// How many events the channel from
    /// [`connect_with_events`](EClient::connect_with_events) discarded.
    ///
    /// The engine never waits on a reader — a session that stalled on one
    /// would stop carrying market data — so an event arriving at a full
    /// channel is dropped. A program that acted on every fill it saw needs to
    /// know the difference between that and every fill there was. Zero for a
    /// session with no channel attached, and for one whose reader kept up.
    pub fn events_lost(&self) -> u64 {
        self.discarded.load(Ordering::Relaxed)
    }

    /// False after [`disconnect()`](EClient::disconnect), after the engine
    /// has ended the session, and after a `process_msgs()` call that
    /// observed the engine stopping.
    ///
    /// The engine records an ended session itself. A shape that never pumps
    /// `process_msgs` hears of it nowhere else, and kept saying connected
    /// after the session underneath it was over.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
            && self.shared.reference.session_over().is_none()
    }

    /// Whether this session is finished rather than merely disconnected:
    /// closed by [`disconnect()`](EClient::disconnect), or given up on by the
    /// engine, which records why. A loss the engine is still working on is
    /// neither — `is_connected()` reads false between the 1100 and the 1102,
    /// and a request made then is carried when the transports come back.
    /// Refused under 504 there, a withdrawal was left unapplied and the feed
    /// came back with the session; the reference client serves that window.
    pub fn session_over(&self) -> bool {
        self.shared.admission_closed() || self.shared.reference.session_over().is_some()
    }

    /// The protocol level this client implements: 217, the reference client's
    /// `MIN_SERVER_VER_ADDITIONAL_ORDER_PARAMS_2`. `None` once the session is
    /// over, as the reference client answers before its greeting — and not
    /// while a lost connection is being recovered, which the reference client
    /// rides out holding the number.
    ///
    /// In the reference architecture this number is the API level of the
    /// process a program is talking to. That process was a gateway, which
    /// announced it and had every request gated on it; here it is this
    /// client, so the number is a statement about this client and not a
    /// reading off the venue, whose logon names no such level.
    ///
    /// Attached orders (218) load the selected account preset and construct
    /// the parent and children. Percentage allocations still need positions
    /// by account and model and the applicable allocation-group state, so
    /// the level remains 217. A percentage allocation cannot yet determine
    /// the quantities of its attached children here; supply explicitly sized
    /// parent and child orders when those quantities depend on group or model
    /// holdings. Configuration requests (219, 221) and the last
    /// price and size stated to their precision (222, 224) are absent.
    /// `hedgeMaxSize` (223) is sent on a beta hedge, and odd-lot
    /// quotes (225) are served: generic tick 787 is asked for and its prices,
    /// sizes and venues delivered. `conditionsIncludeOvernight` (226) is
    /// sent with an order's conditions, and refused under 10371 when the order
    /// is placed where the logon does not enable it or the contract trades on
    /// no overnight venue.
    /// 226 is the highest level a gateway announces.
    ///
    /// Below it, a program that believes the number is wrong about the
    /// following, and each is said on use rather than passed over:
    ///
    /// * An order field this client does not carry, refused by name on
    ///   `error` under 321 when the order is placed: `smartComboRoutingParams`
    ///   (57).
    /// * A withdrawal stating a manual time (169): the withdrawal goes, with
    ///   its operator and who entered it, and the caller is told on `error`
    ///   that the time did not travel.
    ///
    /// Every other gate at or below 217 names a request, field or callback
    /// that is here and does what it does through a gateway.
    pub fn server_version(&self) -> Option<i32> {
        (!self.session_over()).then_some(crate::client_core::PROTOCOL_LEVEL)
    }

    /// When the venue says this session logged in, by its own clock and in its
    /// own spelling; `None` once the session is over, and held while a lost
    /// connection is recovered, as the level is.
    ///
    /// The reference client answers the time its gateway stamped on its
    /// greeting. The venue stamps every message it sends with the time it sent
    /// it, the answer to the logon included, and this is that stamp — the
    /// clock [`competing_session`](EClient::competing_session) reads the other
    /// session's logon off. Where the venue stamped none, the connect holds
    /// this machine's clock instead and says so in the log.
    pub fn tws_connection_time(&self) -> Option<String> {
        (!self.session_over())
            .then(|| self.logged_in_at.clone())
            .filter(|stamp| !stamp.is_empty())
    }

    /// Nothing to start. The reference client sends its client id here and its
    /// gateway begins the exchange on receiving it; here the session is up
    /// and its engine running by the time [`connect`](EClient::connect)
    /// returns, so there is nothing left to begin. Once the session is over
    /// this is reported the way the reference client reports a call with no
    /// session: on `error`, under 504 and no request.
    pub fn start_api(&self) {
        if self.session_over() {
            self.refuse_session(&Refusal::not_connected("Not connected"));
        }
    }

    /// Push a refusal into the session's order, at the call.
    ///
    /// For a value the caller cannot hand the engine because the engine's
    /// types cannot carry it: the refusal then takes its place in the session's
    /// one order, after everything pushed before this call, as a gateway's
    /// rejection arrives after everything the gateway wrote before it.
    /// Delivered by [`process_msgs`](EClient::process_msgs) on `error`, under
    /// the number `origin` names.
    pub fn refuse(&self, origin: crate::types::model::ErrorOrigin, code: i64, msg: &str) {
        if !self.shared.admission_closed() { self.shared.push_refused(origin, code, msg); }
    }

    /// Wait for the engine to signal, for at most `timeout`: true when it
    /// signalled, false when the wait ran out.
    ///
    /// The engine signals at the end of each pass of its loop, and when a
    /// connection goes or comes back. One waiter takes each signal. A thread
    /// that reads the session only when there may be something to read waits
    /// here and then calls [`process_msgs`](EClient::process_msgs): true is a
    /// reason to read, not a promise that the read delivers anything.
    pub fn wait_for_data(&self, timeout: std::time::Duration) -> bool {
        self.shared.wait_for_data(timeout)
    }

    /// Be called when this session has something to read.
    ///
    /// The hook runs on the engine's own thread, after a pass of its loop that
    /// pushed a record or wrote a quote, a holding or an account figure, and at
    /// most once until the next [`process_msgs`](EClient::process_msgs)
    /// begins: an idle session never calls it, and a busy one calls it once
    /// per read however much arrives. It is called holding none of the
    /// engine's locks, so it may take a lock the engine takes, but it must
    /// return at once: the loop that reads the venue's sockets waits for it.
    ///
    /// A hook replaces the one before it, and `None` removes it. A hook that
    /// panics is caught, logged and removed, and the engine goes on.
    pub fn on_data(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.shared.set_wake_hook(hook);
    }

    /// Deliver to `record` everything a call that answers reads, its own
    /// answer under its own number included.
    ///
    /// A call that answers rather than delivers —
    /// [`contract_details`](EClient::contract_details),
    /// [`historical_data`](EClient::historical_data) and the others that hand
    /// back what they asked for — holds the session's turn while it waits.
    /// With a record kept here, each of its pumps is a whole read of the
    /// session, delivered to the record and to the call's collector together:
    /// the record receives everything in the session's order, the call's own
    /// answer in its place under a number from the range those calls take,
    /// which no request of the caller's carries. With none, the call takes
    /// only the records under the numbers it holds and leaves everything else
    /// where it is for [`process_msgs`](EClient::process_msgs).
    ///
    /// Keep a record that writes into the state the program's own
    /// [`process_msgs`](EClient::process_msgs) loop writes into: what reaches
    /// it here is not delivered to that loop again, the notice that a
    /// connection went or came back included. Never hold this record's lock
    /// across `process_msgs`. A call locks the record inside the turn it
    /// already holds, so a loop that locks the record and then waits for the
    /// turn waits on a call that is waiting on it, and neither ever returns.
    /// Hand `process_msgs` a wrapper of its own that locks the shared state on
    /// each callback, as the record does: the turn first, then the state, on
    /// both sides. Replaces any record kept before.
    pub fn keep_record(&self, record: Arc<Mutex<dyn Wrapper + Send>>) {
        *self.kept.lock().unwrap_or_else(|e| e.into_inner()) = Some(record);
    }

    /// Disconnect from IB.  Sends `Shutdown` to the hot loop, waits for the
    /// background thread to exit, and marks the client as disconnected.
    ///
    /// Returns only once the engine's thread has ended, however many callers
    /// stop it at once: a TWS client knows its
    /// socket is closed when `disconnect` returns, and a program that must not
    /// open a second session on the account needs the same of the first. It
    /// never panics. The engine's wake hook must return at once, so it cannot
    /// call this blocking method or drop the client's last owner.
    ///
    /// The engine's last record is delivered by the next
    /// [`process_msgs`](EClient::process_msgs), after everything the session
    /// queued before it, as `connection_closed`. What this side keeps about
    /// the session's requests is kept until then, so a final read still
    /// delivers each quote and callback under the request it belongs to.
    pub fn disconnect(&self) -> Shutdown {
        self.stop();
        let joined = self.thread.join();
        // A client assembled from parts runs no engine that would push it.
        if self.closes_itself {
            self.shared.push_closed();
        }
        Shutdown {
            logout_sent: self.shared.logout_sent.load(Ordering::Acquire),
            engine: if self.shared.engine_panicked.load(Ordering::Acquire) { EngineEnd::Panicked } else { joined },
        }
    }

    fn stop(&self) {
        // The loop takes the stop once the logout's finishing phase is over.
        self.shared.close_admission();
        self.connected.store(false, Ordering::Release);
        let _ = self.shared.admit(&self.control_tx, ControlCommand::Logout);
        let _ = self.shared.admit(&self.control_tx, ControlCommand::Shutdown);
    }
}

impl EClient {
    /// Which slot a contract holds on this session, if it holds one.
    pub fn instrument_of(&self, con_id: i64) -> Option<crate::types::InstrumentId> {
        // Through the same lookup every other reader uses, which drops what the
        // engine has given back before it answers. Read from the map directly,
        // this named a slot that had already gone to the next contract.
        self.core.cached_instrument(&self.shared, con_id)
    }

    /// The session's own state, for reading what has arrived.
    pub fn shared_state(&self) -> &Arc<SharedState> {
        &self.shared
    }

    /// What tells this session apart from another in the same process.
    pub(crate) fn which_session(&self) -> usize {
        Arc::as_ptr(&self.shared) as usize
    }

    /// Take the session's turn to ask one question.
    ///
    /// One question at a time: see [`EClient::asking`]. Refused rather than
    /// waited on where this thread is the one reading the session — a question
    /// asked from inside a callback waits for a turn its own thread holds, and
    /// the read that would answer it is the read it is inside. Told on the
    /// spot, a caller can do something about it; left waiting, nothing about
    /// that program runs again.
    pub(crate) fn take_the_turn(&self) -> Result<std::sync::MutexGuard<'_, ()>, Refusal> {
        if reading_now() == self.which_session() {
            return Err(Refusal::validation(
                "a question cannot be asked from inside a callback: this thread is the \
                 one reading the session, and the answer would arrive on the read it is \
                 inside. Ask from another thread, or keep what you need and ask once the \
                 read has returned".to_string(),
            ));
        }
        Ok(self.asking.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// What the venue last stated for a contract, and the contract as it named
    /// it.
    ///
    /// Not every action of the contract's life: each question replaces what the
    /// one before it left, so this states the answer to the last range asked
    /// about.
    ///
    /// The venue serves no adjusted series of its own: asked for one by name it
    /// answers that it has no such data, and the trades it does serve are raw.
    /// A series that crosses a split steps by the split's ratio with nothing in
    /// it saying so, which is a wrong number rather than a missing one.
    ///
    /// This is what a caller adjusts with.
    /// [`scale_before`](crate::scale_before) turns a date
    /// and these actions into the factor a price from that date carries, so a
    /// caller can put a series on one scale. Three kinds move a price: a stock
    /// dividend, a split and a spin-off. The factor is the value the action
    /// states, and a spin-off states its reciprocal — established against a
    /// contract that split ten for one, where the closes either side were
    /// 1208.88 and 121.79, and every close before it folds to a tenth.
    ///
    /// A cash dividend is stated here and moves nothing, and neither does a
    /// rights offer; a future rollover carries no value to move anything by.
    /// That is the scale the adjusted series is stated in, not a gap in this
    /// client: a series that took dividends off as well would be on a second
    /// scale, and nothing the venue serves beside it would be on that one.
    ///
    /// Empty until the venue has stated them for the contract, which it does
    /// once per contract on a historical request.
    pub fn adjustments(&self, con_id: &str) -> Option<(AdjustedContract, Vec<Adjustment>)> {
        self.shared.reference.adjustments_for(con_id)
    }

    /// What the venue sent this session that nothing here reads, by
    /// connection: each kind of message named once, the first time it
    /// arrives. With `IBKR_DX_CAPTURE_WIRE` set, every frame is kept here as
    /// well, whole and as sent — a reading checked only against frames this
    /// client made up says nothing about the ones that arrive.
    pub fn unread_wire(&self) -> Vec<(&'static str, String)> {
        self.shared.market.unread_wire()
    }

    /// Another session that already held this account when this one connected.
    ///
    /// `None` when this session is alone. Otherwise where the other one
    /// connected from, when it logged in — GMT, as the venue writes it:
    /// `yyyyMMdd-HH:mm:ss` — and whether this session is held to reading only
    /// because the other has the account.
    ///
    /// Worth asking before starting work: the venue permits one logon at a time
    /// and takes the account from the older session without saying which it
    /// dropped, so a second client reads as data that stops arriving.
    pub fn competing_session(&self) -> Option<(String, String, bool)> {
        self.shared.reference.competing_session()
    }

    /// Session ID surfaced to webapp REST clients as `x-ccp-session-id`.
    pub fn ccp_session_id(&self) -> String {
        self.shared.reference.ccp_session_id()
    }

    /// Logical-name → host URL lookup from the gateway logon MiscUrls push
    /// (e.g. `region_dam`). Returns `None` when the gateway did not push this key.
    pub fn misc_url(&self, key: &str) -> Option<String> {
        self.shared.reference.misc_url(key)
    }

    /// Canonical big-endian session-token bytes (leading zeros stripped) captured
    /// at connect. Round-trips through `BigUint::from_bytes_be` to the SRP shared
    /// secret K and is the second SHA-1 input for SSO `Authenticate-TWS` bodies.
    pub fn session_token_bytes(&self) -> &[u8] {
        &self.session_token_bytes
    }

    /// The session this connection established, for a caller that wants to
    /// resume from it later.
    ///
    /// Hand it back through [`EClientConfig::resume`] on a subsequent connect.
    /// Keep it wherever the process keeps secrets — it is a credential, and
    /// where it lives is the caller's decision, which is why nothing here
    /// writes it anywhere by default.
    pub fn session(&self) -> &crate::auth::resume::ResumableSession {
        &self.session
    }

}

#[cfg(test)]
mod host_default_tests {
    use super::*;

    /// A caller with a login and nothing else gets a session. The hostname it
    /// would otherwise have to name is where every session starts anyway, and
    /// the venue redirects from there.
    #[test]
    fn a_config_without_a_host_still_knows_where_to_knock() {
        let config = EClientConfig {
            username: "someone".to_string(),
            password: "secret".to_string(),
            ..Default::default()
        };
        assert!(config.host.is_empty(), "a caller stated none");
        let resolved = gateway_config(&config);
        assert_eq!(resolved.host, crate::config::CCP_HOSTS[0]);
    }

    /// And a reconnect logs in under the settings the session opened under.
    ///
    /// Built inline, they were `Default::default()`: the first login announced
    /// what the caller stated and every login after a drop announced whatever
    /// the environment held, on the same session.
    #[test]
    fn a_reconnect_states_what_the_session_opened_under() {
        let config = EClientConfig {
            username: "someone".to_string(),
            password: "secret".to_string(),
            gateway: crate::settings::GatewaySettings {
                timezone: Some("America/New_York".to_string()),
                build: Some("10999".to_string()),
                execution_reports: Some(crate::settings::ExecutionReportScope::Today),
                ..Default::default()
            },
            ..Default::default()
        };
        let auth = caller_auth(&config, &gateway_config(&config));
        assert_eq!(auth.settings.timezone, "America/New_York");
        assert_eq!(auth.settings.build, "10999");
        assert_eq!(
            auth.settings.execution_reports,
            crate::settings::ExecutionReportScope::Today,
            "a reconnect asking for every execution the venue holds is a request \
             the caller did not make",
        );
    }

    /// One that is named is used as given.
    #[test]
    fn a_host_that_is_named_is_used() {
        let config = EClientConfig {
            username: "someone".to_string(),
            password: "secret".to_string(),
            host: "ndc1.ibllc.com".to_string(),
            ..Default::default()
        };
        assert_eq!(gateway_config(&config).host, "ndc1.ibllc.com");
    }
}

#[cfg(test)]
mod readonly_tests {
    use super::*;

    /// And the calls a caller actually makes are the ones that refuse.
    ///
    /// The guard existed and every trading call on this surface reached the
    /// venue without consulting it: the flag was stored, the helper was tested
    /// directly, and a session opened read-only placed orders.
    #[test]
    fn the_trading_calls_are_the_ones_that_refuse() {
        let (client, _rx, _shared) = crate::api::client::tests::test_client();
        client.core.set_readonly(true);
        let spy = Contract { con_id: 756733, symbol: "SPY".into(), ..Default::default() };
        let order = crate::types::model::Order::limit("BUY", 1.0, 1.00);

        assert!(client.try_place_order(1, &spy, &order).is_err(), "an order is refused");
        assert!(crate::api::client::tests::reported(&client, || client.cancel_order(1, "")).is_err(), "a cancel is refused");
        client.cancel_order_by_perm_id(1);
        assert_eq!(client.shared.drain_refused().len(), 1, "a cancel by permanent id is refused");
        assert!(crate::api::client::tests::reported(&client, || client.req_global_cancel("")).is_err(), "a global cancel is refused");
        assert!(
            crate::api::client::tests::reported(&client, || client.exercise_options(1, &spy, 1, 1, "", false, Default::default())).is_err(),
            "an exercise is refused",
        );
        // Three orders under one call, and the only trading call that reached
        // the engine without consulting the flag: it sends through a path of
        // its own, which the guard did not sit on.
        assert!(
            client.place_bracket(&spy, "BUY", 1.0, 100.0, 110.0, 90.0).is_err(),
            "a bracket is refused",
        );
    }

    /// A cancel addresses an API number through the engine's order identities.
    #[test]
    fn a_cancel_of_an_unknown_api_number_is_refused_by_the_engine() {
        let (client, rx, shared) = crate::api::client::tests::test_client();
        shared.orders.set_replay_done();
        for id in [-1_i64, 0, i64::MIN] {
            client.cancel_order(id, "");
            assert!(shared.drain_refused().is_empty());
            rx.pump();
            assert!(matches!(shared.drain_refused().as_slice(), [(asked, 135, _)] if *asked == id));
        }
        assert!(rx.try_recv().is_err());
    }

    /// A session meant only to look refuses to place, change or withdraw an
    /// order. The other client here has taken this on connect all along; a
    /// Rust caller could not state it, so a read-only session could still
    /// trade.
    #[test]
    fn a_read_only_session_refuses_to_trade() {
        let core = ClientCore::new();
        core.set_readonly(true);
        assert!(core.refuse_if_readonly("place an order").is_err());
        assert!(
            core.refuse_if_readonly("place an order")
                .unwrap_err()
                .to_lowercase()
                .contains("read"),
            "the refusal does not say why",
        );
    }

    /// And one that was not asked for does not.
    #[test]
    fn an_ordinary_session_is_not_refused() {
        let core = ClientCore::new();
        core.set_readonly(false);
        assert!(core.refuse_if_readonly("place an order").is_ok());
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use std::time::Duration;

    /// A client whose engine thread ends `after` from now, and says when it has.
    fn ending_after(after: Duration) -> (Arc<EClient>, Arc<AtomicBool>) {
        let ended = Arc::new(AtomicBool::new(false));
        let handle = {
            let ended = Arc::clone(&ended);
            thread::spawn(move || {
                thread::sleep(after);
                ended.store(true, Ordering::Release);
            })
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let client = EClient::from_parts(Arc::new(SharedState::new()), tx, handle, "DU1".into());
        (Arc::new(client), ended)
    }

    /// Two callers stopping one session at once both return after its thread
    /// has ended. The second used to find the handle taken and return at once,
    /// with the first still joining.
    #[test]
    fn two_callers_both_return_after_the_thread_has_ended() {
        let (client, ended) = ending_after(Duration::from_millis(300));
        let callers: Vec<_> = (0..2)
            .map(|_| {
                let client = Arc::clone(&client);
                let ended = Arc::clone(&ended);
                thread::spawn(move || {
                    client.disconnect();
                    ended.load(Ordering::Acquire)
                })
            })
            .collect();
        for caller in callers {
            assert!(caller.join().unwrap(), "a caller returned before the thread ended");
        }
    }

    /// A caller that unwinds while it holds the thread's handle hands the join
    /// to the caller waiting on it, which returns only after the thread has
    /// ended.
    #[test]
    fn a_joiner_that_unwinds_hands_the_join_to_the_other_caller() {
        let (client, ended) = ending_after(Duration::from_millis(400));
        let (holding_tx, holding) = std::sync::mpsc::channel();
        let first = {
            let client = Arc::clone(&client);
            thread::spawn(move || {
                crate::bridge::hooks::set(&crate::bridge::hooks::BEFORE_THE_JOIN, move || {
                    holding_tx.send(()).unwrap();
                    thread::sleep(Duration::from_millis(100));
                    panic!("the joiner goes before its join returns");
                });
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| client.disconnect())).is_err()
            })
        };
        holding.recv().unwrap();
        let second = {
            let client = Arc::clone(&client);
            let ended = Arc::clone(&ended);
            thread::spawn(move || {
                client.disconnect();
                ended.load(Ordering::Acquire)
            })
        };
        assert!(first.join().unwrap(), "the first caller unwound");
        assert!(second.join().unwrap(), "the second returned only once the thread had ended");
    }

    #[test]
    fn the_last_owner_on_the_engine_thread_requests_a_stop_without_joining_itself() {
        let (owner_tx, owner_rx) = std::sync::mpsc::channel::<EClient>();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            let client = owner_rx.recv().unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(client)));
            done_tx.send(result.is_ok()).unwrap();
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Arc::new(SharedState::new());
        let client = EClient::from_parts(Arc::clone(&shared), tx, handle, "DU1".into());
        owner_tx.send(client).unwrap();
        assert!(
            done_rx.recv_timeout(Duration::from_secs(30))
                .expect("dropping the last owner on the engine thread did not finish"),
            "dropping the last owner on the engine thread panicked"
        );
        assert!(shared.admission_closed());
        assert!(matches!(rx.recv().unwrap(), ControlCommand::Logout));
        assert!(matches!(rx.recv().unwrap(), ControlCommand::Shutdown));
    }

    /// A lock poisoned by a thread that panicked holding it does not make
    /// dropping the client panic, which during an unwind would abort.
    #[test]
    fn a_poisoned_lock_does_not_make_drop_panic() {
        let (client, ended) = ending_after(Duration::from_millis(50));
        let poisoner = {
            let client = Arc::clone(&client);
            thread::spawn(move || {
                let _held = client.thread.state.lock().unwrap();
                panic!("poison the lock");
            })
        };
        assert!(poisoner.join().is_err());
        assert!(client.thread.state.is_poisoned());
        let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            drop(Arc::into_inner(client).expect("the only handle left"));
        }));
        assert!(dropped.is_ok(), "dropping the client panicked");
        assert!(ended.load(Ordering::Acquire), "and it returned after the thread ended");
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    struct Quiet;
    impl Wrapper for Quiet {}

    /// A connected session whose loop runs on its own thread, with the far
    /// ends of its sockets held open so nothing reads as a loss.
    fn connected() -> (EClient, Vec<std::net::TcpStream>) {
        let pair = || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let near = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (far, _) = listener.accept().unwrap();
            (crate::protocol::connection::Connection::new_raw(near).unwrap(), far)
        };
        let shared = Arc::new(SharedState::new());
        let (farm, farm_peer) = pair();
        let (ccp, ccp_peer) = pair();
        let (hot_loop, tx) = crate::engine::hot_loop::HotLoop::with_connections(
            shared.clone(), None, "DU1".into(), farm, ccp, None, None,
        );
        let handle = thread::spawn(move || hot_loop.run_with_panic_recovery());
        let client = EClient::from_parts(shared, tx, handle, "DU1".into());
        // Whatever the loop's start changed is read, and every lap after it
        // has had its look.
        thread::sleep(Duration::from_millis(100));
        client.process_msgs(&mut Quiet);
        thread::sleep(Duration::from_millis(20));
        (client, vec![farm_peer, ccp_peer])
    }

    fn counting() -> (Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let hook: Arc<dyn Fn() + Send + Sync> = {
            let calls = Arc::clone(&calls);
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        };
        (calls, hook)
    }

    fn a_quote(client: &EClient, bid: i64) {
        client.shared.market.push_quote(0, &crate::types::Quote { bid, ..Default::default() });
    }

    fn within(bound: Duration, done: impl Fn() -> bool) -> bool {
        let began = Instant::now();
        while began.elapsed() < bound {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        done()
    }

    /// A session counts what its connections carry: what each read and wrote
    /// before the session took it, the logon's own, and everything after.
    #[test]
    fn a_session_counts_what_its_connections_carry() {
        let pair = || {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let near = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (far, _) = listener.accept().unwrap();
            (crate::protocol::connection::Connection::new_raw(near).unwrap(), far)
        };
        let shared = Arc::new(SharedState::new());
        let (farm, _farm_peer) = pair();
        let (mut ccp, mut ccp_peer) = pair();
        // What the logon read before the session took the connection.
        let heartbeat = crate::protocol::fix::fix_build(&[(35, "0")], 1);
        ccp.seed_buffer(&heartbeat);
        let (hot_loop, tx) = crate::engine::hot_loop::HotLoop::with_connections(
            shared.clone(), None, "DU1".into(), farm, ccp, None, None,
        );
        assert_eq!(shared.traffic().bytes_received, heartbeat.len() as u64,
            "counted when the session takes its connections, before any lap");
        let handle = thread::spawn(move || hot_loop.run_with_panic_recovery());
        let client = EClient::from_parts(shared, tx, handle, "DU1".into());
        assert_eq!(Traffic::default(), Traffic { bytes_sent: 0, bytes_received: 0, messages_sent: 0, messages_received: 0 });

        use std::io::Write as _;
        ccp_peer.write_all(&heartbeat).unwrap();
        let counted = |t: Traffic| {
            t.bytes_received >= 2 * heartbeat.len() as u64
                && t.messages_received >= 2
                && t.bytes_sent > 0
                && t.messages_sent > 0
        };
        assert!(
            within(Duration::from_secs(5), || counted(client.traffic())),
            "the seeded frame, the one read after it, and what the loop wrote: {:?}",
            client.traffic(),
        );
        client.disconnect();
    }

    /// A connected session with nothing arriving never calls its hook: the
    /// loop laps as fast as it can, and a hook called each lap would wake its
    /// owner at that rate.
    #[test]
    fn an_idle_session_never_calls_the_hook() {
        let (client, _peers) = connected();
        let (calls, hook) = counting();
        client.on_data(Some(hook));
        thread::sleep(Duration::from_secs(1));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "called with nothing to read");
        client.disconnect();
    }

    #[test]
    fn replacing_a_hook_drops_its_capture_outside_the_lock() {
        struct Captured(std::sync::Weak<SharedState>);
        impl Drop for Captured {
            fn drop(&mut self) {
                if let Some(shared) = self.0.upgrade() {
                    shared.set_wake_hook(None);
                }
            }
        }
        let shared = Arc::new(SharedState::new());
        let captured = Captured(Arc::downgrade(&shared));
        shared.set_wake_hook(Some(Arc::new(move || { let _ = &captured; })));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            shared.set_wake_hook(None);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(30))
            .expect("removing the hook did not finish dropping its captured value");
        worker.join().unwrap();
    }

    /// With a quote every millisecond, the hook is called at most once per
    /// read: once as it is set, and once more after each read begins.
    #[test]
    fn a_busy_session_calls_the_hook_at_most_once_per_read() {
        let (client, _peers) = connected();
        let client = Arc::new(client);
        let (calls, hook) = counting();
        client.on_data(Some(hook));
        let feeder = {
            let client = Arc::clone(&client);
            thread::spawn(move || {
                for bid in 1..=400 {
                    a_quote(&client, bid);
                    thread::sleep(Duration::from_millis(1));
                }
            })
        };
        let mut reads = 0;
        while !feeder.is_finished() {
            thread::sleep(Duration::from_millis(20));
            client.process_msgs(&mut Quiet);
            reads += 1;
        }
        feeder.join().unwrap();
        thread::sleep(Duration::from_millis(20));
        let called = calls.load(Ordering::SeqCst);
        assert!(called >= 1, "a session that changed never called its hook");
        assert!(called <= reads + 1, "called {called} times across {reads} reads");
        client.disconnect();
    }

    /// A hook replaces the one before it.
    #[test]
    fn a_second_hook_replaces_the_first() {
        let (client, _peers) = connected();
        let (first_calls, first) = counting();
        client.on_data(Some(first));
        a_quote(&client, 1);
        assert!(within(Duration::from_secs(5), || first_calls.load(Ordering::SeqCst) == 1));

        let (second_calls, second) = counting();
        client.on_data(Some(second));
        client.process_msgs(&mut Quiet);
        a_quote(&client, 2);
        assert!(within(Duration::from_secs(5), || second_calls.load(Ordering::SeqCst) == 1));
        assert_eq!(first_calls.load(Ordering::SeqCst), 1, "the first hook was called after it was replaced");
        client.disconnect();
    }

    #[test]
    fn replacing_a_hook_does_not_begin_another_read() {
        let shared = SharedState::new();
        let (first_calls, first) = counting();
        shared.set_wake_hook(Some(first));
        shared.market.push_quote(0, &crate::types::Quote { bid: 1, ..Default::default() });
        shared.notify();
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        let (second_calls, second) = counting();
        shared.set_wake_hook(Some(second));
        shared.market.push_quote(0, &crate::types::Quote { bid: 2, ..Default::default() });
        shared.notify();
        assert_eq!(second_calls.load(Ordering::SeqCst), 0, "no read began");
        shared.arm_wake();
        shared.market.push_quote(0, &crate::types::Quote { bid: 3, ..Default::default() });
        shared.notify();
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        shared.set_wake_hook(None);
        shared.arm_wake();
        shared.market.push_quote(0, &crate::types::Quote { bid: 4, ..Default::default() });
        shared.notify();
        assert_eq!(second_calls.load(Ordering::SeqCst), 1, "removed");
    }

    #[test]
    fn data_beside_the_quote_wakes_its_reader() {
        use crate::types::{SeriesTick, SeriesValue};
        let writes: &[fn(&SharedState)] = &[
            |s| s.push_refused(crate::types::model::ErrorOrigin::Session, 321, "an answer"),
            |s| s.market.push_series_tick(SeriesTick { instrument: 0, tick_type: 23, value: SeriesValue::Generic(0.2) }),
            |s| s.market.push_snapshot_answer(0, vec![SeriesTick { instrument: 0, tick_type: 1, value: SeriesValue::Price(100.0) }]),
            |s| s.market.note_quote_attributes(0, 3, 1),
            |s| s.market.note_scanned_strategies(0, Vec::new()),
            |s| s.market.note_stated_figures(0, 493, vec![1.0, 1.0, 0.2]),
            |s| s.market.note_stated_rows(0, 547, vec![(1.0, 2.0, 3.0)]),
            |s| s.market.note_paired_figures(0, 236, vec![(1.0, 2.0)]),
            |s| s.market.note_numbered_figures(0, 3, &[(1, 2.0)], &[]),
            |s| s.market.note_contract_figures(0, Some(1000.0), None),
            |s| s.market.note_short_sale_restriction(0, true),
            |s| s.portfolio.note_account_value("NetLiquidation", "1000", "USD"),
        ];
        for (at, write) in writes.iter().enumerate() {
            let shared = SharedState::new();
            let (calls, hook) = counting();
            shared.set_wake_hook(Some(hook));
            shared.notify();
            assert_eq!(calls.load(Ordering::SeqCst), 0, "idle writer {at}");
            write(&shared);
            shared.notify();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "writer {at}");
            shared.arm_wake();
            shared.notify();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "writer {at} did not change again");
        }
    }

    /// A hook may take the locks the engine takes as it notifies: the signal's
    /// own, and the hook's, by setting another in its place. Neither is held
    /// while it runs, so the loop goes on.
    #[test]
    fn a_hook_taking_the_engines_locks_does_not_deadlock() {
        let (client, _peers) = connected();
        let (after_calls, after) = counting();
        let shared = Arc::clone(&client.shared);
        let took = Arc::new(AtomicBool::new(false));
        let hook: Arc<dyn Fn() + Send + Sync> = {
            let took = Arc::clone(&took);
            Arc::new(move || {
                shared.wait_for_data(Duration::ZERO);
                shared.set_wake_hook(Some(Arc::clone(&after)));
                took.store(true, Ordering::SeqCst);
            })
        };
        client.on_data(Some(hook));
        a_quote(&client, 1);
        assert!(within(Duration::from_secs(5), || took.load(Ordering::SeqCst)), "the hook ran");
        client.process_msgs(&mut Quiet);
        a_quote(&client, 2);
        assert!(
            within(Duration::from_secs(5), || after_calls.load(Ordering::SeqCst) == 1),
            "the loop went on after a hook that took its locks",
        );
        client.disconnect();
    }

    #[test]
    fn a_panicking_hooks_payload_cannot_panic_when_removed() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("a panic payload that panics when dropped");
            }
        }
        let shared = SharedState::new();
        shared.set_wake_hook(Some(Arc::new(|| std::panic::panic_any(PanicOnDrop))));
        shared.market.push_quote(0, &crate::types::Quote { bid: 1, ..Default::default() });
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shared.notify())).is_ok());
        shared.arm_wake();
        shared.market.push_quote(0, &crate::types::Quote { bid: 2, ..Default::default() });
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shared.notify())).is_ok());
    }

    #[test]
    fn a_panicking_hooks_captured_value_cannot_panic_when_removed() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("a captured value that panics when dropped");
            }
        }
        let shared = SharedState::new();
        let captured = PanicOnDrop;
        shared.set_wake_hook(Some(Arc::new(move || {
            let _keep = &captured;
            panic!("a hook with a captured value");
        })));
        shared.market.push_quote(0, &crate::types::Quote { bid: 1, ..Default::default() });
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shared.notify())).is_ok());
        let (calls, hook) = counting();
        shared.set_wake_hook(Some(hook));
        shared.arm_wake();
        shared.market.push_quote(0, &crate::types::Quote { bid: 2, ..Default::default() });
        shared.notify();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A hook that panics is caught, logged and removed, and the loop goes on.
    #[test]
    fn a_panicking_hook_is_removed_and_the_loop_goes_on() {
        let (client, _peers) = connected();
        let panicked = Arc::new(AtomicUsize::new(0));
        let hook: Arc<dyn Fn() + Send + Sync> = {
            let panicked = Arc::clone(&panicked);
            Arc::new(move || {
                panicked.fetch_add(1, Ordering::SeqCst);
                panic!("a hook that panics");
            })
        };
        client.on_data(Some(hook));
        a_quote(&client, 1);
        assert!(within(Duration::from_secs(5), || panicked.load(Ordering::SeqCst) == 1));

        client.process_msgs(&mut Quiet);
        a_quote(&client, 2);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(panicked.load(Ordering::SeqCst), 1, "a hook that panicked was called again");

        let (calls, hook) = counting();
        client.on_data(Some(hook));
        a_quote(&client, 3);
        assert!(
            within(Duration::from_secs(5), || calls.load(Ordering::SeqCst) == 1),
            "the loop went on",
        );
        client.disconnect();
    }
}

#[cfg(test)]
mod take_back_tests {
    use super::*;
    use std::io::Read;

    /// A connect taken back once its session is open logs that session out,
    /// as the stop's goodbye does, and one not taken back says nothing.
    #[test]
    fn a_session_opened_for_a_connect_taken_back_is_logged_out() {
        let (mut trading, mut venue) = crate::protocol::connection::Connection::for_test();
        venue.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(!logged_out_if_taken_back(Some(&cancel), &mut trading), "not taken back");
        assert!(!logged_out_if_taken_back(None, &mut trading), "nothing to take it back");
        let mut heard = [0u8; 256];
        assert!(venue.read(&mut heard).is_err(), "a session not taken back is left open");

        cancel.store(true, Ordering::Relaxed);
        assert!(logged_out_if_taken_back(Some(&cancel), &mut trading), "taken back");
        let n = venue.read(&mut heard).expect("the logout");
        let said = String::from_utf8_lossy(&heard[..n]).replace('\u{1}', "|");
        assert!(said.contains("35=5|"), "the session is logged out: {said}");
    }
}
