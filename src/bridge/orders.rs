//! What has been submitted, filled, and refused.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use crate::types::*;
use crate::types::model as api;
use super::record::{FillRecord, Queue, Stamps, UpdateRecord};

mod ids;
#[cfg(feature = "python")]
pub(crate) use ids::reserve_order_ids;

/// How long a caller waits for the venue to finish naming the working orders.
///
/// Read by the engine as well, which holds the question of what the account
/// has finished for the same replay and must not invent a second answer to
/// "how long could that take".
pub(crate) const REPLAY_WAIT: Duration = Duration::from_secs(3);

/// What the venue has said about the numbers orders were placed under.
#[derive(Default)]
struct Numbers {
    /// Finished: filled, cancelled or refused.
    finished: std::collections::HashSet<u64>,
}

/// Attachment fields stated by execution reports, including explicit zeroes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AttachedOrderMetadata {
    pub family_key: Option<String>,
    pub parent: Option<String>,
    pub use_parent_price: Option<bool>,
    pub profit_offset: Option<f64>,
    pub api_order_id: Option<i64>,
    pub api_client_id: Option<i32>,
}

/// What a connection has said about the orders it already had working.
///
/// One record rather than three flags. A reconnect has to put all of it back
/// to "nothing said yet" in one move: written separately, a caller reading
/// between the writes saw the new connection's unfinished naming against the
/// previous connection's bound, which had already passed, and was answered at
/// once instead of waiting for the naming it was asking about.
#[derive(Default)]
struct Replay {
    /// Set when the server finishes naming the orders already working, which
    /// it does unprompted after a connect. Until then "none" and "not yet
    /// told" look the same to a caller.
    done: bool,
    /// Whether the venue has named anything on this connection.
    began: bool,
    /// When the wait for that naming gives up, shared by everyone waiting.
    ///
    /// Set when a connection comes up — the venue starts the naming then —
    /// and replaced with the next one on a reconnect, so each connection pays
    /// the bound once and every caller in that window waits for the same
    /// moment.
    deadline: Option<Instant>,
}

/// How a bounded wait for the naming of what the account is working ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayWait {
    /// The naming finished (`true`), or its own bound passed first (`false`).
    Settled(bool),
    /// The caller's own bound passed first.
    TimedOut,
    /// The caller took the wait back.
    TakenBack,
}

/// Fills, order status updates, cancel rejects, what-if responses, order
/// cache, and inactive-order reasons.
pub struct OrderState {
    saved_ids: Mutex<Option<ids::SavedIds>>,
    /// The highest id this client has used, as a new order's number is
    /// checked against it: saved before the session opened, and every id the
    /// venue states for a working order this client placed.
    used: AtomicU64,
    /// Each fill and the report it was booked off, where there is one.
    ///
    /// One pass can carry two prints of the same order. Looked up against the
    /// order afterwards, both read the record the later print left, so the
    /// earlier one was reported under the later one's execution id, time,
    /// running quantity and average — and the charge that named that id was
    /// then attached to both.
    ///
    /// Each carries the status the same report stated, so one execution
    /// report is one record and nothing about it is paired up again at the
    /// read.
    pub(super) fills: Queue<FillRecord>,
    /// Status changes with no fill on the same report, each with the order's
    /// state as that report stated it.
    pub(super) order_updates: Queue<UpdateRecord>,
    /// Every order whose message this client put on the wire.
    ///
    /// What the venue then said about it is a separate question, and the two
    /// are indistinguishable from outside without this. Only tests keep this
    /// record; ordinary sessions have no reader that needs it.
    orders_sent: Mutex<std::collections::HashSet<u64>>,
    reusable_order_ids: Mutex<std::collections::HashSet<u64>>,
    attached_metadata: Mutex<HashMap<u64, AttachedOrderMetadata>>,
    api_order_ids: Mutex<HashMap<(i32, i64), u64>>,
    api_client_id: std::sync::atomic::AtomicI32,
    waiting_attached_orders: Mutex<Vec<OrderRequest>>,
    /// Every finished order the venue stated an API order id for.
    ///
    /// The venue numbers an order placed through an API and does not number
    /// one typed in by hand, so this is what tells the two apart — and it is
    /// the only thing that does. A caller asking for the API orders alone is
    /// answered with these.
    api_numbered: Mutex<std::collections::HashSet<u64>>,
    /// Refused cancels and changes, each with when the venue sent the report
    /// it comes from, where it said.
    pub(super) cancel_rejects: Queue<(CancelReject, Option<i64>)>,
    /// What each fill cost, as the venue states it on a record of its own.
    pub(super) charges: Queue<crate::types::model::CommissionAndFeesReport>,
    /// Executions the venue restated rather than announced: replayed at logon
    /// for quantity the book already holds, or for an order this session never
    /// tracked. Nothing is booked from them, so none became a fill; they are
    /// kept so a caller asking for the day's executions is answered.
    pub(super) restated_executions: Queue<(api::Contract, api::Execution)>,
    pub(super) what_if_responses: Queue<WhatIfResponse>,
    completed_orders: Mutex<Vec<CompletedOrder>>,
    /// Whether the venue has said it has stated every finished order it holds.
    completed_orders_ended: std::sync::atomic::AtomicU64,
    completed_orders_asked: std::sync::atomic::AtomicU64,
    /// The latest turn the venue has finished answering.
    completed_orders_ended_on: std::sync::atomic::AtomicU64,
    /// Orders the venue has taken back after reporting them finished.
    ///
    /// The completion queue empties on read, and what is read out of it is
    /// kept by the caller's side for as long as the session lasts — so
    /// removing a queued completion cannot reach one already read. This is how
    /// the correction reaches it: the id and the venue's name for the order go
    /// out on the same path the completion did, and whoever holds the archive
    /// drops it.
    order_corrections: Mutex<Vec<(u64, String)>>,
    /// Enriched order info from CCP exec reports (order_id -> RichOrderInfo).
    order_cache: Mutex<HashMap<u64, Arc<RichOrderInfo>>>,
    /// Orders that reached a terminal state, and when, with the venue's own
    /// name for the order that finished under each number. The cache row is
    /// evicted when an order completes, so the cached status alone cannot say
    /// an order is done — a replayed frame would find nothing to refuse and
    /// insert it as open.
    pub(super) completed: Mutex<HashMap<u64, (Instant, String)>>,
    /// Every number the venue has finished an order under this session, and
    /// every number it has said names no order it holds.
    ///
    /// Read by the engine as it takes an order: a number the venue has
    /// finished an order under is spent — the venue refuses a repeated number
    /// only while it is still working one, so after a fill it would take a
    /// second order under it — and a number the venue says it does not know
    /// names nothing this session placed.
    numbers: Mutex<Numbers>,
    /// Whether an order left the book since the engine last removed stale terms.
    numbers_changed: AtomicBool,
    /// What this connection has said about the orders it already has working.
    replay: Mutex<Replay>,
    /// The highest id the venue has named an order working under, from any
    /// session. An id is spent only while its order is live, so this is the
    /// floor a new id has to clear.
    working_id_watermark: AtomicU64,
    /// The same mark, kept to what a request can carry.
    ///
    /// An order id goes as wide as the venue lets it and a request id is four
    /// billion wide. A caller that numbers both out of one counter — which is
    /// how the client this one stands in for is written — needs a number that
    /// clears every id an order has spent and still fits a request. This is
    /// that number: the highest the venue has named which a request could
    /// carry.
    narrow_id_watermark: AtomicU64,
    /// Reason for a genuinely-Inactive (39=I) transition: (order_id, ibapi
    /// error code, message, the operation it answers, when the venue sent the
    /// message it comes from where it said). ibapi has no callback dedicated
    /// to "order parked with reason", so this is drained into
    /// `Wrapper::error_from` the same way a cancel/modify reject is.
    pub(super) order_inactive: Queue<(u64, i32, String, api::OrderOp, Option<i64>)>,
    /// What the caller is told about an order that goes anyway, as
    /// (order_id, code, message, the operation it answers, when the venue sent
    /// the message it comes from where it said).
    pub(super) order_notices: Queue<(u64, i32, String, api::OrderOp, Option<i64>)>,
    /// Orders whose outstanding replacement the venue has taken.
    ///
    /// The surfaces hold the terms an order had before a replacement, to put
    /// back where the venue refuses it. What spends that copy is the venue
    /// taking the replacement, and nothing but the venue's own word says so:
    /// read off a status instead, a fill landing between the attempt and the
    /// answer hid it, and the copy outlived the replacement it belonged to.
    pub(super) replacements_taken: Queue<u64>,
}

/// The highest id an order can be given.
///
/// One below the widest signed value, because that is where the reader of the
/// venue's reports stops: an id past it does not parse, so an order placed
/// under one could never be matched to the report that acknowledges, fills or
/// withdraws it. Stated once, so the allocator and the reader cannot disagree
/// about where the range ends.
pub const MAX_ORDER_ID: u64 = i64::MAX as u64 - 1;

/// Say, once, that this account's order ids have outgrown a request id.
///
/// A program written against the reference client numbers its orders and its
/// requests out of one signed 32-bit counter, and this protocol carries an
/// order id far wider than it carries a request id. Nothing here can undo an
/// id the account has already used, so it is said rather than worked around —
/// silently, it reads as a client that cannot place its first order.
///
/// [`OrderState::narrow_id_watermark`] is what such a program should count
/// from instead.
pub fn say_if_past_a_request_id(order_id: u64) {
    if order_id > u32::MAX as u64 {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| log::warn!(
            "this account has used an order id above {}, so the next one is {order_id}; a \
             program that numbers its requests from the same counter cannot carry it",
            u32::MAX,
        ));
    }
}

fn waiting_family(orders: &[OrderRequest], held: &[OrderRequest]) -> Vec<OrderRequest> {
    let mut family: Vec<_> = orders.iter().filter_map(|order| held.iter()
        .find(|latest| latest.order_id() == order.order_id()).cloned()
        .or_else(|| (!matches!(order, OrderRequest::SubmitEx { .. })).then(|| order.clone()))).collect();
    for order in held {
        if let OrderRequest::SubmitEx { attrs, .. } = order
            && attrs.parent_id != 0
            && family.iter().any(|parent| parent.order_id() == attrs.parent_id)
            && !family.iter().any(|known| known.order_id() == order.order_id())
        { family.push(order.clone()); }
    }
    family
}

impl OrderState {
    /// An empty one, stamping from its own counter.
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::stamping(&Stamps::default())
    }

    /// An empty one, stamping from the session's counter.
    pub(super) fn stamping(stamps: &Stamps) -> Self {
        Self {
            saved_ids: Mutex::new(None),
            used: AtomicU64::new(0),
            fills: Queue::with_capacity(stamps, 64),
            orders_sent: Mutex::new(std::collections::HashSet::new()),
            reusable_order_ids: Mutex::new(std::collections::HashSet::new()),
            attached_metadata: Mutex::new(HashMap::new()),
            api_order_ids: Mutex::new(HashMap::new()),
            api_client_id: std::sync::atomic::AtomicI32::new(0),
            waiting_attached_orders: Mutex::new(Vec::new()),
            api_numbered: Mutex::new(std::collections::HashSet::new()),
            order_updates: Queue::with_capacity(stamps, 64),
            cancel_rejects: Queue::with_capacity(stamps, 16),
            charges: Queue::with_capacity(stamps, 16),
            restated_executions: Queue::new(stamps),
            what_if_responses: Queue::with_capacity(stamps, 8),
            completed_orders: Mutex::new(Vec::with_capacity(64)),
            completed_orders_ended: std::sync::atomic::AtomicU64::new(0),
            completed_orders_asked: std::sync::atomic::AtomicU64::new(0),
            completed_orders_ended_on: std::sync::atomic::AtomicU64::new(0),
            order_corrections: Mutex::new(Vec::new()),
            order_cache: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            numbers: Mutex::new(Numbers::default()),
            numbers_changed: AtomicBool::new(false),
            replay: Mutex::new(Replay::default()),
            working_id_watermark: AtomicU64::new(0),
            narrow_id_watermark: AtomicU64::new(0),
            order_inactive: Queue::with_capacity(stamps, 8),
            order_notices: Queue::new(stamps),
            replacements_taken: Queue::with_capacity(stamps, 4),
        }
    }

    pub(crate) fn stage_waiting_attached(&self, orders: &[OrderRequest]) {
        let mut held = self.waiting_attached_orders.lock().unwrap();
        for order in orders {
            if matches!(order, OrderRequest::SubmitEx { .. })
                && !held.iter().any(|known| known.order_id() == order.order_id())
            { held.push(order.clone()); }
        }
    }

    pub(crate) fn is_waiting_attached(&self, order_id: u64) -> bool {
        self.waiting_attached_orders.lock().unwrap().iter().any(|order| order.order_id() == order_id)
    }

    pub(crate) fn amend_waiting_attached(&self, change: &OrderRequest, children: &[OrderRequest]) -> bool {
        let OrderRequest::Modify { order_id, qty, tif, spec: Some(spec), .. } = change else { return false };
        let mut held = self.waiting_attached_orders.lock().unwrap();
        let Some(OrderRequest::SubmitEx { kind, attrs, qty: held_qty, tif: held_tif, .. }) =
            held.iter_mut().find(|order| order.order_id() == *order_id) else { return false };
        let mut replacement = spec.attrs.clone();
        replacement.parent_id = attrs.parent_id;
        if replacement.oca_group_str.is_empty() && replacement.oca_group == 0 {
            replacement.oca_group_str = attrs.oca_group_str.clone();
            replacement.oca_group = attrs.oca_group;
        }
        if replacement.oca_type == 0 { replacement.oca_type = attrs.oca_type; }
        replacement.attached.clone_from(&attrs.attached);
        if attrs.attached.as_ref().is_some_and(|held| held.contract_id.is_some()) {
            replacement.combo_legs = attrs.combo_legs.clone();
        }
        *kind = spec.kind.clone();
        *attrs = replacement;
        *held_qty = *qty;
        if *tif != 0 { *held_tif = *tif; }
        for child in children {
            if !held.iter().any(|known| known.order_id() == child.order_id()) { held.push(child.clone()); }
        }
        true
    }

    pub(crate) fn waiting_attached_family(&self, orders: &[OrderRequest]) -> Vec<OrderRequest> {
        waiting_family(orders, &self.waiting_attached_orders.lock().unwrap())
    }

    pub(crate) fn take_waiting_attached_family(&self, orders: &[OrderRequest]) -> Vec<OrderRequest> {
        let mut held = self.waiting_attached_orders.lock().unwrap();
        let family = waiting_family(orders, &held);
        held.retain(|order| !family.iter().any(|taken| taken.order_id() == order.order_id()));
        family
    }

    pub(crate) fn take_waiting_attached_order(&self, order_id: u64) -> Option<OrderRequest> {
        let mut held = self.waiting_attached_orders.lock().unwrap();
        let at = held.iter().position(|order| order.order_id() == order_id)?;
        Some(held.remove(at))
    }

    /// A waiting member discarded before it was sent: finished as cancelled,
    /// said with 202 and a status. Answers the status, for the event stream.
    pub(crate) fn discard_waiting(&self, request: &OrderRequest, timestamp_ns: u64) -> Option<crate::types::OrderUpdate> {
        let OrderRequest::SubmitEx { order_id, instrument, qty, attrs, .. } = request else { return None };
        let update = crate::types::OrderUpdate {
            order_id: *order_id,
            instrument: *instrument,
            status: crate::types::OrderStatus::Cancelled,
            filled_qty: 0.0,
            remaining_qty: crate::types::qty_to_f64(*qty),
            avg_price: 0,
            perm_id: 0,
            parent_id: attrs.parent_id as i64,
            timestamp_ns,
        };
        self.note_order_finished(*order_id, "Cancelled", "");
        self.push_order_notice(*order_id, api::OrderOp::Cancel, 202, "Order was discarded".into());
        self.push_order_update(update);
        Some(update)
    }

    pub(crate) fn note_attached_order_metadata(&self, wire: u64, update: AttachedOrderMetadata) {
        if update == AttachedOrderMetadata::default() { return; }
        let mut all = self.attached_metadata.lock().unwrap();
        let held = all.entry(wire).or_default();
        if update.family_key.is_some() { held.family_key = update.family_key; }
        if update.parent.is_some() { held.parent = update.parent; }
        if update.use_parent_price.is_some() { held.use_parent_price = update.use_parent_price; }
        if update.profit_offset.is_some() { held.profit_offset = update.profit_offset; }
        if update.api_order_id.is_some() { held.api_order_id = update.api_order_id; }
        if update.api_client_id.is_some() { held.api_client_id = update.api_client_id; }
        if let (Some(client), Some(api)) = (held.api_client_id, held.api_order_id) {
            self.api_order_ids.lock().unwrap().entry((client, api)).or_insert(wire);
        }
    }

    pub(crate) fn set_api_client_id(&self, client_id: i32) {
        self.api_client_id.store(client_id, Ordering::Release);
    }

    pub(crate) fn api_client_id(&self) -> i32 {
        self.api_client_id.load(Ordering::Acquire)
    }

    pub(crate) fn wire_order_id(&self, api: i64) -> Option<u64> {
        let client = self.api_client_id();
        let placed = self.api_order_ids.lock().unwrap().get(&(client, api)).copied();
        placed.or_else(|| {
            let wire = u64::try_from(api).ok().filter(|id| *id != 0)?;
            let all = self.attached_metadata.lock().unwrap();
            (!all.get(&wire).is_some_and(|held| held.api_order_id.is_some_and(|id| id != api)
                || held.api_client_id.is_some_and(|id| id != client))).then_some(wire)
        })
    }

    pub(crate) fn forget_local_api_order(&self, wire: u64) {
        self.api_order_ids.lock().unwrap().retain(|_, value| *value != wire);
    }

    pub(crate) fn attached_order_metadata(&self, wire: u64) -> Option<AttachedOrderMetadata> {
        self.attached_metadata.lock().unwrap().get(&wire).cloned()
    }

    pub(crate) fn allow_order_id_reuse(&self, order_id: u64) {
        self.reusable_order_ids.lock().unwrap().insert(order_id);
    }

    pub(crate) fn take_order_id_reuse(&self, order_id: u64) -> bool {
        self.reusable_order_ids.lock().unwrap().remove(&order_id)
    }

    /// Take every fills waiting, leaving none.
    pub fn drain_fills(&self) -> Vec<(Fill, Option<RichOrderInfo>)> {
        self.fills.drain().into_iter().map(|f| (f.fill, f.report.map(|r| (*r).clone()))).collect()
    }

    /// Take the queued statuses, leaving none.
    pub fn drain_order_updates(&self) -> Vec<OrderUpdate> {
        self.order_updates.drain().into_iter().map(|u| u.update).collect()
    }

    /// Take every cancel rejects waiting, leaving none.
    pub fn drain_cancel_rejects(&self) -> Vec<CancelReject> {
        self.cancel_rejects.drain().into_iter().map(|(reject, _)| reject).collect()
    }

    /// Take what the venue has said its fills cost, leaving none.
    ///
    /// The charge is not on the execution report — that report carries no
    /// commission tag at all — but on a record of its own that follows it,
    /// naming the execution it belongs to. A caller reads it the same way:
    /// the fill first, then what it cost.
    pub fn drain_charges(&self) -> Vec<crate::types::model::CommissionAndFeesReport> {
        self.charges.drain()
    }

    #[doc(hidden)] pub fn push_charge(&self, charge: crate::types::model::CommissionAndFeesReport) {
        self.charges.push(charge);
    }

    /// Take the executions the venue restated, leaving none.
    pub fn drain_restated_executions(&self) -> Vec<(api::Contract, api::Execution)> {
        self.restated_executions.drain()
    }

    #[doc(hidden)] pub fn push_restated_execution(&self, contract: api::Contract, execution: api::Execution) {
        self.restated_executions.push((contract, execution));
    }

    /// Drain reasons for genuinely-Inactive (39=I) transitions, each as
    /// (order_id, ibapi error code, message) — see `order_inactive`.
    pub fn drain_order_inactive(&self) -> Vec<(u64, i32, String)> {
        self.order_inactive.drain().into_iter().map(|(id, code, msg, ..)| (id, code, msg)).collect()
    }

    /// Take every what if responses waiting, leaving none.
    pub fn drain_what_if_responses(&self) -> Vec<WhatIfResponse> {
        self.what_if_responses.drain()
    }

    /// The refusals and previews a dispatch loop should deliver, leaving
    /// those a call that answers is waiting on under its own number.
    pub fn drain_order_inactive_for_dispatch(&self, mine: impl Fn(u64) -> bool) -> Vec<(u64, i32, String)> {
        self.order_inactive
            .take_if(|e| !mine(e.0))
            .into_iter()
            .map(|(id, code, msg, ..)| (id, code, msg))
            .collect()
    }



    /// The preview answering one order, if it has arrived, leaving the rest.
    pub fn take_what_if_for(&self, order_id: u64) -> Option<WhatIfResponse> {
        self.what_if_responses.take_first(|w| w.order_id == order_id)
    }

    /// The refusal of one order, if it has arrived, leaving the rest.
    pub fn take_order_inactive_for(&self, order_id: u64) -> Option<(i32, String)> {
        let (_, code, message, ..) = self.order_inactive.take_first(|(id, ..)| *id == order_id)?;
        Some((code, message))
    }

    /// Say that the venue has finished stating what it has finished.
    ///
    /// A caller asking for those waits on this rather than on a clock: the
    /// answer is a run of ordinary reports and its end is the only thing that
    /// says the run is over.
    #[doc(hidden)] pub fn note_completed_orders_end(&self) {
        self.completed_orders_ended.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// The same, for the turn the question was asked on.
    ///
    /// A caller waits for the end of its own question: the answer is a run of
    /// ordinary reports that says nothing about which question it answers, so
    /// the turn travels with the question and comes back here. Counted alone,
    /// a caller that gave up left its answer on its way and the next caller
    /// was released by it.
    #[doc(hidden)] pub fn note_completed_orders_end_on(&self, turn: u64) {
        self.completed_orders_ended_on.fetch_max(turn, std::sync::atomic::Ordering::AcqRel);
        self.note_completed_orders_end();
    }

    /// The latest turn the venue has finished answering.
    pub fn completed_orders_ended_on(&self) -> u64 {
        self.completed_orders_ended_on.load(std::sync::atomic::Ordering::Acquire)
    }

    /// How many times that end has been said.
    ///
    /// Counted rather than flagged, and each caller reads the count before it
    /// asks and waits for it to move. A flag is set by one answer and taken by
    /// whoever polls next: a caller that gave up a moment before the flag was
    /// set left it standing, and the next caller took it as the answer to a
    /// question that had not been asked yet, returning before its own request
    /// reached the venue.
    pub fn completed_orders_ended(&self) -> u64 {
        self.completed_orders_ended.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Note that a question about finished orders has been acted on.
    ///
    /// Said when the engine sends it, or holds it, or refuses it for want of a
    /// connection — the point is that the engine has seen it, not what it did.
    #[doc(hidden)] pub fn note_completed_orders_asked(&self) {
        self.completed_orders_asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// How many have been acted on.
    ///
    /// A caller waits for this to move before it waits for an answer. Without
    /// it, an answer to somebody else's question that completed between the
    /// caller reading the count and the engine taking its question off the
    /// queue was read as the answer to a question the engine had not yet seen.
    pub fn completed_orders_asked(&self) -> u64 {
        self.completed_orders_asked.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Take every completed order waiting, leaving none, each that does not
    /// carry the venue's own record with the one held under its number.
    ///
    /// Taken together, under the queue's lock and then the records': another
    /// order taking the number moves the record onto a completion still queued
    /// under the same two locks, so it finds each completion either still
    /// queued or already carrying its record. Read under the number after the
    /// queue was let go, a completion taken in between was read against a
    /// record already gone, or against the other order's.
    pub fn drain_completed_orders(&self) -> Vec<CompletedOrder> {
        let mut queued = self.completed_orders.lock().unwrap();
        let cache = self.order_cache.lock().unwrap();
        queued.drain(..).map(|mut order| {
            if order.stated.is_none() && order.held.is_none() {
                order.held = cache.get(&order.order_id).map(|info| Box::new((
                    info.contract.clone(), info.order.clone(), info.order_state.clone(),
                )));
            }
            order
        }).collect()
    }

    /// Take the orders the venue has taken back, leaving none.
    ///
    /// Read before the completions beside them: an order taken back and then
    /// finished again is one order that finished once, and applied the other
    /// way round the new record is retracted and the superseded one kept.
    pub fn drain_order_corrections(&self) -> Vec<(u64, String)> {
        self.order_corrections.lock().unwrap().drain(..).collect()
    }

    /// Snapshot enriched entries that belong in the open-order book: a
    /// genuinely open IB state, or a genuinely-Inactive (39=I) order that can
    /// still reactivate. A rejected order also stringifies to "Inactive"
    /// (ibapi has no Rejected string) but always carries a non-empty
    /// `completed_status`, which is how the two are told apart
    /// `is_open_or_reactivatable`. Terminal entries (Filled /
    /// Cancelled / Rejected) are filtered out so `req_open_orders` does not
    /// leak historical orders that are still cached for `req_completed_orders`
    /// lookups.
    pub fn drain_open_orders(&self) -> Vec<(u64, RichOrderInfo)> {
        let lock = self.order_cache.lock().unwrap();
        lock.iter()
            .filter(|(_, v)| crate::types::order_status::is_open_or_reactivatable(
                &v.order_state.status, &v.order_state.completed_status))
            .map(|(&k, v)| (k, (**v).clone()))
            .collect()
    }

    /// Whether the venue has named this order as one it is working.
    ///
    /// An order the venue replayed at connect is known here and in no local
    /// book: this client did not place it. Asked only of the book of what
    /// this client placed, a replace of a replayed order reads as a first
    /// placement and is sent as one, under a number the venue is already
    /// working.
    pub fn venue_is_working(&self, order_id: u64) -> bool {
        self.order_cache.lock().unwrap().get(&order_id).is_some_and(|info| {
            crate::types::order_status::is_open_or_reactivatable(
                &info.order_state.status, &info.order_state.completed_status,
            )
        })
    }

    /// Get enriched order info by order_id.
    pub fn get_order_info(&self, order_id: u64) -> Option<RichOrderInfo> {
        self.order_cache.lock().unwrap().get(&order_id).map(|info| (**info).clone())
    }

    /// The permanent id an order's record states, where it states one.
    pub(crate) fn perm_id(&self, order_id: u64) -> Option<i64> {
        self.order_cache.lock().unwrap().get(&order_id)
            .map(|info| info.order.perm_id)
            .filter(|id| *id != 0)
    }

    /// Whether a fill for this order is still waiting to be read.
    ///
    /// A fill is read against the order's record, so the record outlives the
    /// fill rather than the other way round.
    pub fn has_pending_fill(&self, order_id: u64) -> bool {
        self.fills.any(|f| f.fill.order_id == order_id)
    }

    /// Remove an enriched entry when the venue says the order is unknown.
    pub fn remove_order_info(&self, order_id: u64) {
        self.order_cache.lock().unwrap().remove(&order_id);
    }

    /// Free a delivered completion, unless the order is working again.
    ///
    /// A correction the venue sends after the completion was delivered puts the
    /// order back in the book, and the cleanup armed by that completion runs
    /// afterwards: taken then, the row it removes is the live one, and what
    /// reads the order next finds nothing and seeds an empty contract and order
    /// in its place. The lock spans the test and the removal, so a correction
    /// cannot replace the finished row between them.
    pub fn remove_completed_order_info(&self, order_id: u64) {
        let mut cache = self.order_cache.lock().unwrap();
        if cache.get(&order_id).is_some_and(|info| {
            crate::types::order_status::is_open_or_reactivatable(
                &info.order_state.status, &info.order_state.completed_status,
            )
        }) {
            return;
        }
        cache.remove(&order_id);
    }

    /// Write the status an order has finished on into the entry kept for it.
    ///
    /// Not a removal, which is the other way to stop the entry being read as a
    /// working order. What is asked of it afterwards is what the order was:
    /// the completed-orders reader takes the contract, the quantity, the price
    /// and the venue's permanent number from here, and where the entry is gone
    /// it files an order carrying nothing but its own id. Restated instead,
    /// the open-order union skips it — it reads the status, and a finished one
    /// is not open — and everything the order was is still there to report.
    #[doc(hidden)] pub fn note_order_finished(
        &self, order_id: u64, status: &str, completed_status: &str,
    ) {
        if let Some(info) = self.order_cache.lock().unwrap().get_mut(&order_id) {
            let info = Arc::make_mut(info);
            info.order_state.status = status.into();
            // A refusal and an order the venue merely holds are the same word
            // here, and this is what tells them apart. Left as it was, a
            // refused order read as one that can come back and the union went
            // on listing it as working.
            if !completed_status.is_empty() {
                info.order_state.completed_status = completed_status.into();
            }
        }
    }

    // ── Hot-loop-side writers ──

    /// A fill with no report behind it. What the order's record states at
    /// the push is what the fill is read against, and it rides the record:
    /// read again at the read, a second print of the same order in the
    /// meantime left both reading the later one.
    #[doc(hidden)] pub fn push_fill(&self, fill: Fill) {
        if fill.remaining == 0 {
            self.numbers.lock().unwrap().finished.insert(fill.order_id);
            self.numbers_changed.store(true, Ordering::Release);
        }
        let report = self.order_cache.lock().unwrap().get(&fill.order_id).cloned();
        self.fills.push(FillRecord { fill, report, status: None });
    }

    /// A fill and the report it was booked off, which is the one that states
    /// its execution.
    #[doc(hidden)] pub fn push_fill_reported(&self, fill: Fill, report: RichOrderInfo) {
        if fill.remaining == 0 {
            self.numbers.lock().unwrap().finished.insert(fill.order_id);
            self.numbers_changed.store(true, Ordering::Release);
        }
        self.fills.push(FillRecord { fill, report: Some(Arc::new(report)), status: None });
    }

    /// A fill and the status the same report stated, as one record.
    ///
    /// The venue states both on one execution report. Queued apart and paired
    /// up again at the read by their quantities, an acknowledgement and a fill
    /// read together lost their order, and a status that did not match the
    /// fill's quantities went out on its own after it.
    #[doc(hidden)] pub fn push_fill_and_status(
        &self, fill: Fill, report: Option<RichOrderInfo>, status: OrderUpdate,
    ) {
        let report = report.map(Arc::new).or_else(|| self.order_cache.lock().unwrap().get(&fill.order_id).cloned());
        self.note_what_the_status_says(&status);
        if fill.remaining == 0 {
            self.numbers.lock().unwrap().finished.insert(fill.order_id);
            self.numbers_changed.store(true, Ordering::Release);
        }
        // A working status on an order already finished is the echo
        // `push_order_update` drops; the fill beside it still goes.
        let status = self.states_news(&status).then_some(status);
        self.fills.push(FillRecord { fill, report, status });
    }

    /// Note what a status says about the number it is under: a finish spends
    /// the number, as a caller's own record of the order spends it.
    fn note_what_the_status_says(&self, update: &OrderUpdate) {
        use crate::types::OrderStatus;
        if matches!(update.status, OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Rejected) {
            self.numbers.lock().unwrap().finished.insert(update.order_id);
            self.numbers_changed.store(true, Ordering::Release);
        }
    }

    /// Take the notice that finished numbers need their held terms removed.
    pub(crate) fn take_numbers_changed(&self) -> bool {
        self.numbers_changed.load(Ordering::Relaxed)
            && self.numbers_changed.swap(false, Ordering::AcqRel)
    }

    /// Whether the venue has finished an order under this number this
    /// session.
    pub fn number_finished(&self, order_id: u64) -> bool {
        self.numbers.lock().unwrap().finished.contains(&order_id)
    }

    /// The venue is working another order under a number an order finished
    /// under: what was kept under the number is about the order before.
    ///
    /// The finished order's record goes with its completion, where that is
    /// still waiting to be read, so the working order can be kept under the
    /// number and the finished one still reaches the caller as it was. Its own
    /// completion alone, by the venue's name for it, and under the queue's
    /// lock and then the records', as the queue is taken.
    pub(crate) fn number_taken_by_another_order(&self, order_id: u64) {
        let Some((_, named)) = self.completed.lock().unwrap().remove(&order_id) else { return };
        let mut queued = self.completed_orders.lock().unwrap();
        let mut cache = self.order_cache.lock().unwrap();
        let Some(finished) = cache.get(&order_id).filter(|row| {
            crate::types::order_status::is_terminal_status(
                &row.order_state.status, &row.order_state.completed_status,
            )
        }) else { return };
        if let Some(waiting) = queued.iter_mut().find(|waiting| {
            waiting.order_id == order_id && waiting.venue_order == named
                && waiting.stated.is_none() && waiting.held.is_none()
        }) {
            waiting.held = Some(Box::new((
                finished.contract.clone(), finished.order.clone(), finished.order_state.clone(),
            )));
        }
        cache.remove(&order_id);
    }

    /// Whether a status is news: a finish, an uncertain order, or a working
    /// status of an order that has not finished.
    fn states_news(&self, update: &OrderUpdate) -> bool {
        update.status.is_terminal()
            || update.status == crate::types::OrderStatus::Uncertain
            || !self.recently_completed(update.order_id)
    }

    /// A status change, with the order's state as it stands at the push.
    ///
    /// A working status for an order that has already finished is dropped
    /// here: the venue echoes one behind a fill, and delivered after the fill
    /// it reported a filled order as working with nothing filled. Checked as
    /// it is pushed rather than as it is read, so an acknowledgement pushed
    /// before the fill that finished the order is still delivered — read
    /// together, the fill had finished the order by then and took the
    /// acknowledgement with it.
    #[doc(hidden)] pub fn push_order_update(&self, update: OrderUpdate) {
        if !self.states_news(&update) {
            return;
        }
        self.note_what_the_status_says(&update);
        // Keep the report at this status without copying its order on the loop.
        let (state, client_id) = self.order_cache.lock().unwrap().get(&update.order_id)
            .map(|info| (Some(Arc::clone(info)), info.order.client_id))
            .unwrap_or_default();
        self.order_updates.push(UpdateRecord { update, state, client_id });
    }

    #[doc(hidden)] pub fn push_cancel_reject(&self, reject: CancelReject) {
        self.push_cancel_reject_sent(reject, None);
    }

    /// The same, said on a report of the venue's, with the time the venue
    /// sent that report where it stated one.
    #[doc(hidden)] pub fn push_cancel_reject_sent(&self, reject: CancelReject, sent: Option<i64>) {
        self.cancel_rejects.push((reject, sent));
    }

    /// The venue has taken the replacement outstanding on this order.
    #[doc(hidden)] pub fn note_replacement_taken(&self, order_id: u64) {
        self.replacements_taken.push(order_id);
    }

    /// The orders whose replacement the venue has taken since this was asked.
    pub fn drain_replacements_taken(&self) -> Vec<u64> {
        self.replacements_taken.drain()
    }

    /// Say why an order was refused or stopped working, under its number and
    /// with the operation on it the word answers.
    #[doc(hidden)] pub fn push_order_inactive(&self, order_id: u64, op: api::OrderOp, code: i32, message: String) {
        self.push_order_inactive_sent(order_id, op, code, message, None);
    }

    /// The same, said on a message of the venue's, with the time the venue
    /// sent that message where it stated one, which the error is stamped
    /// with as a gateway stamps it.
    #[doc(hidden)] pub fn push_order_inactive_sent(
        &self, order_id: u64, op: api::OrderOp, code: i32, message: String, sent: Option<i64>,
    ) {
        self.order_inactive.push((order_id, code, message, op, sent));
    }

    /// Say something about an order that goes anyway, on its own number: a
    /// warning, and not the end of the order.
    #[doc(hidden)] pub fn push_order_notice(&self, order_id: u64, op: api::OrderOp, code: i32, message: String) {
        self.push_order_notice_sent(order_id, op, code, message, None);
    }

    /// The same, with the time the venue sent the message it comes from.
    #[doc(hidden)] pub fn push_order_notice_sent(
        &self, order_id: u64, op: api::OrderOp, code: i32, message: String, sent: Option<i64>,
    ) {
        self.order_notices.push((order_id, code, message, op, sent));
    }

    /// Take every notice waiting, leaving none.
    pub fn drain_order_notices(&self) -> Vec<(u64, i32, String)> {
        self.order_notices.drain().into_iter().map(|(id, code, msg, ..)| (id, code, msg)).collect()
    }

    /// As [`drain_order_inactive_for_dispatch`](Self::drain_order_inactive_for_dispatch),
    /// for the notices.
    pub fn drain_order_notices_for_dispatch(&self, mine: impl Fn(u64) -> bool) -> Vec<(u64, i32, String)> {
        self.order_notices
            .take_if(|e| !mine(e.0))
            .into_iter()
            .map(|(id, code, msg, ..)| (id, code, msg))
            .collect()
    }

    #[doc(hidden)] pub fn push_what_if(&self, response: WhatIfResponse) {
        self.what_if_responses.push(response);
    }

    /// Whether the venue has named anything at all on this connection.
    ///
    /// Distinct from the naming being over: an account working nothing is
    /// named with nothing before the report that ends the naming. A caller
    /// that has to say what its withdrawal did not cover needs to know which
    /// of the two it is looking at — with nothing named there is nothing
    /// uncovered to warn about.
    #[doc(hidden)] pub fn note_naming_began(&self) {
        self.replay.lock().unwrap().began = true;
    }

    /// Whether the venue has named anything on this connection.
    pub fn naming_began(&self) -> bool {
        self.replay.lock().unwrap().began
    }

    #[doc(hidden)] pub fn set_replay_done(&self) {
        self.replay.lock().unwrap().done = true;
    }

    /// Whether the orders already working have been received.
    pub fn replay_done(&self) -> bool {
        self.replay.lock().unwrap().done
    }

    /// Wait for the venue to finish naming the orders already working, and
    /// say whether it did.
    ///
    /// The venue ends the naming with a report of its own, on an account
    /// working nothing as on any other; the wait is bounded all the same, so a
    /// naming that does not end does not hold the caller. The bound is one
    /// deadline per connection, anchored to the moment the
    /// connection came up rather than set by the first caller to wait: the
    /// venue starts the naming then, so a first request that waits the bound
    /// out does not spend a later caller's wait, and once it has passed
    /// nobody waits again until a reconnect.
    pub fn wait_for_replay(&self) -> bool {
        self.wait_for_replay_until(None, None) == ReplayWait::Settled(true)
    }

    /// [`wait_for_replay`](Self::wait_for_replay), also bounded by `until` and
    /// by `cancel`, both read at each 10 ms step: the wait ends at whichever
    /// comes first of the naming, its own bound, `until` and the cancel.
    pub fn wait_for_replay_until(
        &self,
        until: Option<Instant>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> ReplayWait {
        let (done, deadline) = {
            let mut replay = self.replay.lock().unwrap();
            // Read as a pair, because they are written as one: the bound
            // belongs to the connection whose naming this reports on, and the
            // two taken apart let a caller hold one connection's bound against
            // another's naming.
            //
            // The connection sets the deadline when it comes up; where one
            // never has, the first waiter marks the start of the wait.
            let deadline = *replay.deadline
                .get_or_insert_with(|| Instant::now() + REPLAY_WAIT);
            (replay.done, deadline)
        };
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
                return ReplayWait::TakenBack;
            }
            if done || self.replay_done() || Instant::now() >= deadline {
                return ReplayWait::Settled(done || self.replay_done());
            }
            if until.is_some_and(|until| Instant::now() >= until) {
                return ReplayWait::TimedOut;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Where the naming of what the account is working stands, read without
    /// waiting: `Some(true)` once the venue has finished it, `Some(false)` once
    /// its bound has passed without that, and `None` while it may still come.
    ///
    /// The engine holds what depends on the naming against this, in its own
    /// laps, rather than a caller waiting on it.
    pub fn replay_settled(&self) -> Option<bool> {
        let mut replay = self.replay.lock().unwrap();
        if replay.done {
            return Some(true);
        }
        let deadline = *replay.deadline.get_or_insert_with(|| Instant::now() + REPLAY_WAIT);
        (Instant::now() >= deadline).then_some(false)
    }

    /// The highest id named by the venue or retained from a previous session.
    /// The saved counter supplies the initial floor; replay can raise it.
    pub fn working_id_watermark(&self) -> u64 {
        self.working_id_watermark.load(Ordering::Acquire)
    }

    /// The highest id the venue has named that a request can also carry.
    ///
    /// See `narrow_id_watermark`.
    pub fn narrow_id_watermark(&self) -> u64 {
        self.narrow_id_watermark.load(Ordering::Acquire)
    }

    /// A new connection has not yet named what it has working.
    ///
    /// Set once and never cleared, this state outlived the connection that
    /// earned it: after a reconnect a caller asking what it already has on was
    /// answered straight away, from the old session's record, and never waited
    /// for the new one to say — which is how the same order gets placed twice.
    #[doc(hidden)] pub fn replay_is_pending(&self) {
        {
            let mut replay = self.replay.lock().unwrap();
            replay.done = false;
            // And it has named nothing on this connection. Carried across, the
            // flag reports for ever that some earlier connection had begun,
            // and a withdrawal of every order against an account working
            // nothing warns about orders that were never there.
            replay.began = false;
            // The venue starts the naming as the connection comes up, so the
            // bound a caller waits on starts here too. Anchored on the first
            // caller instead, a first request that waited it out spent it, and
            // a global cancel issued straight after waited nothing and said
            // nothing.
            replay.deadline = Some(Instant::now() + REPLAY_WAIT);
        }
    }

    /// File one order the venue has finished, replacing what an earlier event
    /// of the same order left waiting to be read.
    ///
    /// The answer to what the venue has finished states each order's whole
    /// life, one report per event, in the order they happened — and the last
    /// of them carries what became of it. Filed as a first sighting each time,
    /// the second event onwards was refused as a repeat and the caller was
    /// handed the first: a filled order reported as submitted, short every
    /// fill that followed. That refusal is for a live replay of an order this
    /// session already saw finish, which is a different thing from the next
    /// event of the same history — so this path does not consult it, and the
    /// same answer restated supersedes what it restates.
    ///
    /// Not remembered as a finish under its number, either. That memory is of
    /// what this session saw finish, so a replay of it can be refused; this is
    /// the venue's account of the past, and written under the number it said
    /// that an order this session holds under the same number had finished.
    #[doc(hidden)] pub fn refile_completed_order(&self, order: CompletedOrder) {
        let mut queued = self.completed_orders.lock().unwrap();
        match queued.iter_mut().find(|q| q.order_id == order.order_id
            && q.venue_order == order.venue_order)
        {
            Some(waiting) => *waiting = order,
            // Queued whatever the memory of it says. That memory refuses a
            // *live* replay of an order already seen to finish; this is the
            // same answer restated, which happens when a caller was released
            // before the venue had finished and the rest arrived afterwards.
            // Refused on the strength of the memory, the caller kept the
            // half-built record for good.
            None => queued.push(order),
        }
    }

    /// File a completion once. The venue resends terminal reports — a
    /// reconnect replays recent activity, and a report can simply arrive
    /// twice — and a replay finds the order retired, with nothing tracked to
    /// file it against. Filed again anyway, reconciliation counted the same
    /// finished order twice and read it with no contract and nothing filled,
    /// contradicting the terminal status the caller had already been given.
    /// An order still remembered as completed is therefore not filed again;
    /// the memory itself is refreshed, so a notice extends the window in which
    /// a replay of it can still be refused.
    ///
    /// The same order is the same number under the same venue name. A number
    /// is free again once its order is done, so a second order the venue
    /// finished under it is a second completion, not a repeat of the first.
    #[doc(hidden)] pub fn push_completed_order(&self, order: CompletedOrder) {
        let already = self.finished_under(order.order_id)
            .is_some_and(|named| named == order.venue_order);
        self.remember_completed(order.order_id, &order.venue_order);
        if already {
            return;
        }
        self.completed_orders.lock().unwrap().push(order);
    }

    /// Remember that this order finished, and keep that memory bounded.
    ///
    /// One place, because both the live path and the history path need it and
    /// one of them had only half of it: pruning what had expired but not
    /// evicting the oldest survivors when nothing had, so a burst faster than
    /// the retention window grew past the cap it advertises.
    fn remember_completed(&self, order_id: u64, venue_order: &str) {
        let now = Instant::now();
        let mut completed = self.completed.lock().unwrap();
        completed.insert(order_id, (now, venue_order.to_string()));
        // Pruned here rather than on every read: this runs once per order,
        // and a read is on the message path.
        if completed.len() > COMPLETED_MAX {
            completed.retain(|_, (at, _)| now.duration_since(*at) < COMPLETED_RETENTION);
        }
        // A burst faster than the retention window leaves nothing expired
        // for `retain` to find, so the map can still be over the cap here.
        // Evict the oldest survivors until it isn't — the actual bound,
        // not just the common case.
        if completed.len() > COMPLETED_MAX {
            let mut by_age: Vec<(u64, Instant)> =
                completed.iter().map(|(&id, (at, _))| (id, *at)).collect();
            by_age.sort_unstable_by_key(|&(_, at)| at);
            for (id, _) in by_age.into_iter().take(completed.len() - COMPLETED_MAX) {
                completed.remove(&id);
            }
        }
    }

    /// Whether this order completed recently enough that a frame reopening it
    /// is a replay rather than news.
    pub(crate) fn recently_completed(&self, order_id: u64) -> bool {
        self.finished_under(order_id).is_some()
    }

    /// The venue's own name for the order that recently finished under this
    /// number, empty where it stated none.
    pub(crate) fn finished_under(&self, order_id: u64) -> Option<String> {
        self.completed.lock().unwrap().get(&order_id)
            .filter(|(at, _)| at.elapsed() < COMPLETED_RETENTION)
            .map(|(_, named)| named.clone())
    }

    /// Note that this client put an order's message on the wire.
    ///
    /// Distinct from anything the venue then says about it. A caller, and a
    /// phase, cannot otherwise tell "the venue answered nothing" from "we
    /// never asked": both look like silence from outside, and a live phase
    /// that skipped on that silence passed whether or not anything reached the
    /// socket.
    #[doc(hidden)] pub fn note_the_order_went_out(&self, order_id: u64) {
        self.orders_sent.lock().unwrap().insert(order_id);
    }

    /// Whether this client put this order's message on the wire.
    pub fn the_order_went_out(&self, order_id: u64) -> bool {
        self.orders_sent.lock().unwrap().contains(&order_id)
    }

    /// The venue states an API order id for this finished order.
    #[doc(hidden)] pub fn note_api_numbered(&self, order_id: u64) {
        self.api_numbered.lock().unwrap().insert(order_id);
    }

    /// Whether it did, which is what tells an order placed through an API from
    /// one typed in by hand.
    fn was_api_numbered(&self, order_id: u64) -> bool {
        self.api_numbered.lock().unwrap().contains(&order_id)
    }

    /// Whether a finished order was entered through an API rather than by hand.
    ///
    /// Two ways to know, and an order needs only one. This session put it on
    /// the wire, so it went through this API whatever the venue says about it;
    /// or the venue states an API order id for it, which it does for an order
    /// some API placed and does not for one typed in. The first is how the
    /// reference client knows its own — it holds the source of every order it
    /// sent — and the second is the only thing the wire says.
    ///
    /// Both of the order's numbers are asked, because the record the archive
    /// holds is built two ways: an order the venue finished long ago is filed
    /// under the id the report named, and one that finished while this session
    /// watched keeps the number it was placed under and states no permanent id
    /// at all. Asked under one of the two, an order another API placed and
    /// finished live was left out of the answer the venue itself had marked.
    pub fn was_entered_through_an_api(&self, order_id: u64, perm_id: u64) -> bool {
        self.the_order_went_out(order_id)
            || self.was_api_numbered(order_id)
            || self.was_api_numbered(perm_id)
    }

    /// The venue has named this id, whatever became of the order under it.
    ///
    /// A withdrawn id is free again and a filled one is not, so counting past
    /// the working set alone handed out an id a fill had spent and the venue
    /// refused it. Said on its own rather than as a consequence of keeping a
    /// row: a record the venue replays and this client does not keep — the
    /// history of an order that partly filled and then went — named an id all
    /// the same, and every guard that stopped the row from being kept stopped
    /// the mark with it.
    #[doc(hidden)] pub fn note_the_venue_named(&self, order_id: u64) {
        self.working_id_watermark.fetch_max(order_id, Ordering::AcqRel);
        if order_id <= u32::MAX as u64 {
            self.narrow_id_watermark.fetch_max(order_id, Ordering::AcqRel);
        }
    }

    /// Cache the enriched view of an order.
    ///
    /// An order that has completed is not returned to a working status. Nothing
    /// remembered that an order was done, so a replayed frame — the reconnect
    /// open-order burst racing a fill, or any message the venue resends —
    /// wrote `Submitted` over the terminal entry, and `req_open_orders` then
    /// reported a completed order as live.
    ///
    /// The cached status alone cannot carry that knowledge, because completing
    /// an order evicts its cache row: the replayed frame finds nothing to refuse
    /// and inserts itself. The completed-id memory is what survives the
    /// eviction, and an intervening terminal report cannot overwrite the
    /// evidence the way a cached string could.
    ///
    /// A correction from the venue is not a replay and goes through
    /// [`push_order_correction`](Self::push_order_correction).
    #[doc(hidden)] pub fn push_order_info(&self, order_id: u64, info: RichOrderInfo) {
        self.note_the_venue_named(order_id);
        if crate::types::order_status::is_open_status(&info.order_state.status) {
            if self.recently_completed(order_id) {
                return;
            }
            // Held from the test to the insert. Taken and dropped and taken
            // again, a removal landing in the gap let a completed order be
            // written back as an open one — the test saw the record, the
            // removal took it away, and the insert put a stale view of it back.
            let mut cache = self.order_cache.lock().unwrap();
            if cache.get(&order_id).is_some_and(|e| {
                crate::types::order_status::is_terminal_status(
                    &e.order_state.status,
                    &e.order_state.completed_status,
                )
            }) {
                return;
            }
            cache.insert(order_id, Arc::new(info));
            return;
        }
        self.order_cache.lock().unwrap().insert(order_id, Arc::new(info));
    }

    /// Cache a view that supersedes a completed one.
    ///
    /// A trade cancel or trade correction restates an execution the venue has
    /// already reported, so it can legitimately return a filled order to a
    /// working quantity. That is the venue's statement rather than a
    /// replay of an older one, so it is not refused, and the order stops being
    /// remembered as completed.
    ///
    /// The order is its number under the venue's own name for it, so what an
    /// earlier order under the same number finished as is left standing.
    #[doc(hidden)] pub fn push_order_correction(&self, order_id: u64, venue_order: &str, info: RichOrderInfo) {
        self.completed.lock().unwrap().remove(&order_id);
        // And the notice itself, where it has not been read yet. Only the
        // memory that refuses a replay was cleared, so a completion already
        // queued still went out after the correction had put the order back to
        // working — a caller was told the same order was open and finished.
        self.completed_orders.lock().unwrap().retain(|c| {
            c.order_id != order_id || c.venue_order != venue_order
        });
        // And the copy the caller's side kept, which this cannot reach. Said
        // there instead: the queue empties on read, so a completion already
        // read is held on the far side of it and went on being reported as
        // this order's outcome after the venue had withdrawn it.
        self.order_corrections.lock().unwrap().push((order_id, venue_order.to_string()));
        self.order_cache.lock().unwrap().insert(order_id, Arc::new(info));
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;

    #[test]
    fn cached_reports_are_shared_until_the_order_changes() {
        let orders = OrderState::new();
        orders.push_order_info(7, RichOrderInfo {
            contract: api::Contract { symbol: "SPY".into(), ..Default::default() },
            order: api::Order::default(),
            order_state: api::OrderState { status: "Submitted".into(), warning_text: "held".into(), ..Default::default() },
            last_exec: api::Execution::default(),
        });
        let cached = orders.order_cache.lock().unwrap()[&7].clone();
        let status = OrderUpdate { order_id: 7, instrument: 0, status: OrderStatus::Submitted,
            filled_qty: 0.0, remaining_qty: 1.0, avg_price: 0, perm_id: 0, parent_id: 0, timestamp_ns: 0 };
        let fill = Fill { order_id: 7, instrument: 0, side: Side::Buy, price: 100,
            qty: 1, remaining: 1, timestamp_ns: 0, cum_qty: 1, avg_price: 100 };
        orders.push_order_update(status);
        orders.push_fill(fill);
        orders.push_fill_and_status(fill, None, status);
        let updates = orders.order_updates.drain();
        assert!(Arc::ptr_eq(updates[0].state.as_ref().unwrap(), &cached));
        for fill in orders.fills.drain() {
            assert!(Arc::ptr_eq(fill.report.as_ref().unwrap(), &cached));
        }
        orders.note_order_finished(7, "Filled", "Filled");
        assert_eq!(cached.order_state.status, "Submitted");
        assert_eq!(orders.get_order_info(7).unwrap().order_state.status, "Filled");
    }
}
