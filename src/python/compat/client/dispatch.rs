//! Event dispatch: one read of the session, in the session's one order, into
//! the Python wrapper.

use crate::types::qty_to_f64;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pyo3::prelude::*;

use crate::bridge::{Answer, FillRecord, Record, Reply, SharedState, Take, UpdateRecord};
use crate::client_core::Polled;
use crate::types::order_status::order_status_str;
use crate::types::*;

use crate::types::model::{
    Execution as ApiExecution,
    CommissionAndFeesReport as ApiCommissionAndFeesReport,
};
use super::EClient;
use super::super::contract::{Contract, ContractDescription, ContractDetails, BarData, CommissionAndFeesReport, DepthMktDataDescriptionPy, Execution, Order, OrderState};
use super::super::tick_types::*;
use super::super::super::types::PRICE_SCALE_F;

/// Tick type 13: the venue's model computation.
const MODEL_OPTION_COMPUTATION: i32 = 13;

/// The same on a delayed feed, which the reference client numbers apart.
///
/// A program that asked for delayed data reads its model there; delivered
/// under 13 it arrived indistinguishable from a live reading, on a feed the
/// caller had been told was delayed.
const DELAYED_MODEL_OPTION_COMPUTATION: i32 = 83;

/// Tick type 53: a computation this client was asked for.
///
/// The stream and the answer are two different things, and the venue names
/// them apart: a caller watching a contract reads the model on 13, and a
/// caller who asked what a volatility implies reads their answer on 53. Sent
/// under 13, an answer arrived indistinguishable from the stream.
const ASKED_OPTION_COMPUTATION: i32 = 53;

/// A figure the venue did not state, as the reference client states it.
///
/// The reference decoder turns the wire's indicators into `None` before
/// calling the wrapper. This surface calls the wrapper itself, so neither
/// those indicators nor this client's internal unset double is a number
/// the caller should receive.
fn unstated_as(value: f64, sentinel: f64) -> Option<f64> {
    if value == f64::MAX || value.is_nan() || value == sentinel { None } else { Some(value) }
}

/// A price, a volatility or a dividend the venue did not state.
fn or_unstated_price(value: f64) -> Option<f64> { unstated_as(value, -1.0) }

/// A greek the venue did not state.
fn or_unstated_greek(value: f64) -> Option<f64> { unstated_as(value, -2.0) }

/// Fire a callback on the caller's wrapper.
///
/// An ordinary exception escapes after closing the session, as the reference
/// loop's `finally` does. Interrupts still pass straight back to the caller.
///
/// Routed through the dispatcher that also tries the name the reference client
/// gives the callback: a wrapper written against that client defines those
/// names, and a call made only under this client's names lands on the base
/// class's do-nothing default instead of on the caller's code.
macro_rules! call_wrapper {
    ($client:ident, $py:expr, $shared:ident, $method:expr, $args:expr) => {
        if let Err(e) = crate::python::compat::client::call_named($py, &$client.wrapper, $method, $args) {
            if !e.is_instance_of::<pyo3::exceptions::PyException>($py) {
                return Err(e);
            }
            $client.disconnect($py)?;
            $client.tell_the_caller_it_closed($py)?;
            return Err(e);
        }
        if !$client.is_current_session($shared) {
            return Ok(());
        }
    };
}

/// Deliver an error as `call_wrapper!` delivers any callback: on `error_from`
/// with what it is about, where the wrapper's class has that method, and on
/// `error` otherwise.
macro_rules! say_error {
    ($client:ident, $py:expr, $shared:ident, $origin:expr, $time:expr, $code:expr, $msg:expr) => {
        let (name, args) = $client.error_callback($py, $origin, $time, $code, $msg)?;
        call_wrapper!($client, $py, $shared, name, args.bind($py).clone());
    };
}

/// Call a callback owed for one of the caller's calls: a refusal made at the
/// call, or an answer composed at its marker. An ordinary exception its
/// handler raises is logged and the rest of the answer still goes, as it
/// always did for these; an interrupt leaves the read.
macro_rules! answer_wrapper {
    ($client:ident, $py:expr, $shared:ident, $method:expr, $args:expr) => {
        $client.notify($py, $method, $args)?;
        if !$client.is_current_session($shared) {
            return Ok(());
        }
    };
}

/// Add a callback to an answer being composed, its arguments built now.
macro_rules! owed {
    ($out:ident, $py:expr, $method:expr, $args:expr) => {
        $out.push(($method, $args.into_pyobject($py)?.unbind()))
    };
}

/// One thing a read hands to the wrapper: an engine record, or an answer this
/// surface built at its call from Python objects.
pub(crate) enum Delivery {
    /// A record the engine pushed.
    Record(Record),
    /// A callback and its arguments, built at a call.
    Answer(&'static str, Py<pyo3::types::PyTuple>),
}

/// What a read took and could not deliver, because the caller's handler raised
/// an interrupt part way through: handed over first on the next read, which is
/// its place in the order.
pub(crate) type Undelivered = (std::collections::VecDeque<(u64, Delivery)>, Option<Polled>);

thread_local! {
    /// Which client this thread is inside a read of, if any. A read begun
    /// inside a read is served by the one it is inside.
    static READING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Mark this thread as inside a read of one session, until dropped.
pub(super) struct Reading(usize);

impl Reading {
    fn begin(session: usize) -> Self {
        Self(READING.replace(session))
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        READING.set(self.0);
    }
}

impl EClient {
    /// A scan's row as a contract's details: the contract the row names, filled
    /// in from what this session holds about it.
    pub(crate) fn scanned_details(
        &self, py: Python<'_>, entry: &crate::control::scanner::ScannerEntry, shared: &SharedState,
    ) -> PyResult<Py<ContractDetails>> {
        let cd = ContractDetails::new_default(py);
        {
            let mut contract = cd.contract.borrow_mut(py);
            contract.con_id = entry.con_id as i64;
            // Look up cached contract for symbol info
            if let Some(ac) = self.core.get_contract(entry.con_id as i64, shared) {
                contract.symbol = ac.symbol;
                contract.sec_type = ac.sec_type;
                contract.exchange = ac.exchange;
                contract.currency = ac.currency;
                contract.local_symbol = ac.local_symbol;
                contract.primary_exchange = ac.primary_exchange;
                contract.trading_class = ac.trading_class;
            }
        }
        Py::new(py, cd)
    }

    /// One read of the session.
    ///
    /// Takes the session's turn, as the other surface's read does: the queues
    /// empty as they are read, and two threads reading one session at once —
    /// which the free-threaded interpreter allows — would hand one record to
    /// both or neither, and out of order. Then the conflated state, the cut,
    /// every record stamped below it and every answer built at a call below
    /// it, delivered in stamp order, and the state after them.
    pub(crate) fn dispatch_once(&self, py: Python<'_>, shared: &Arc<SharedState>) -> PyResult<()> {
        if !self.is_current_session(shared) { return Ok(()); }
        let session = self as *const Self as usize;
        // A read from inside a read is served by the one it is inside: the
        // turn is not re-entrant, and what this would take is the outer read's
        // to deliver.
        if READING.with(std::cell::Cell::get) == session {
            return Ok(());
        }
        let (_turn, _reading) = self.lifecycle_turn(py);
        if !self.is_current_session(shared) { return Ok(()); }
        // The session's last record has been delivered.
        if self.session_ended.load(Ordering::Acquire) { return Ok(()); }
        // A test session's engine takes what the calls sent before the read,
        // as a session's own loop has taken it by then.
        #[cfg(feature = "test-helpers")]
        self._test_pump();
        self.free_what_the_fills_held_back(shared);

        // What an interrupted read left: stamped below anything taken now.
        let (mut items, left_polled) = self.undelivered.lock().unwrap().take()
            .unwrap_or_default();
        // (a) The conflated state, before the cut.
        let polled = self.poll_the_state(shared);
        let mut polled = Some(match left_polled {
            Some(earlier) => earlier.then(polled),
            None => polled,
        });
        // (b) and (c).
        let cut = shared.next_seq();
        let bulletins = self.core.bulletin_subscribed.load(Ordering::Acquire);
        let mut taken: Vec<(u64, Delivery)> = shared
            .take_records(cut, Take::Dispatch { bulletins })
            .into_iter()
            .map(|(seq, r)| (seq, Delivery::Record(r)))
            .collect();
        {
            let mut waiting = self.waiting_answers.lock().unwrap();
            while waiting.front().is_some_and(|(seq, ..)| *seq < cut) {
                let (seq, name, args) = waiting.pop_front().unwrap();
                taken.push((seq, Delivery::Answer(name, args)));
            }
        }
        taken.sort_unstable_by_key(|(seq, _)| *seq);
        items.extend(taken);

        // (d) In stamp order.
        while let Some((seq, item)) = items.pop_front() {
            let delivered = match item {
                Delivery::Answer(name, args) => self.notify(py, name, args.bind(py).clone()),
                Delivery::Record(Record::Closed) => {
                    // A barrier: every writer has stopped, so the state taken
                    // again is final and delivered in place of the earlier.
                    let last = self.poll_the_state(shared);
                    let state = match polled.take() {
                        Some(earlier) => earlier.then(last),
                        None => last,
                    };
                    if let Err(e) = self.deliver_state(py, shared, state) {
                        // An interrupt in the final state: the close is still
                        // owed, and is the first thing the next read delivers.
                        if self.is_current_session(shared)
                            && !self.session_ended.load(Ordering::Acquire)
                        {
                            items.push_front((seq, Delivery::Record(Record::Closed)));
                            *self.undelivered.lock().unwrap() = Some((items, None));
                        }
                        return Err(e);
                    }
                    if !self.is_current_session(shared) { return Ok(()); }
                    return self.deliver_the_close(py);
                }
                Delivery::Record(Record::Answer(answer)) => {
                    match self.compose_answer(py, shared, answer, &mut polled) {
                        Ok(owed) => {
                            for (name, args) in owed.into_iter().rev() {
                                items.push_front((seq, Delivery::Answer(name, args)));
                            }
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
                Delivery::Record(record) => self.deliver_record(py, shared, record, &mut polled),
            };
            if let Err(e) = delivered {
                // An interrupt: what is left is handed over first next time,
                // where it stands. An ordinary exception has closed the
                // session, and nothing of it is delivered after that.
                if self.is_current_session(shared) && !self.session_ended.load(Ordering::Acquire) {
                    *self.undelivered.lock().unwrap() = Some((items, polled));
                }
                return Err(e);
            }
            if !self.is_current_session(shared) { return Ok(()); }
        }
        // (e) The state taken in (a), under the mapping the records left.
        if let Some(state) = polled {
            self.deliver_state(py, shared, state)?;
        }
        Ok(())
    }

    /// A close and a replacement take the same turn as a read. Calls from a
    /// callback already hold it, including a callback that replaced the session.
    pub(super) fn lifecycle_turn(&self, py: Python<'_>) -> (Option<std::sync::MutexGuard<'_, ()>>, Reading) {
        let client = self as *const Self as usize;
        let turn = (READING.with(std::cell::Cell::get) != client).then(|| self.take_the_read_turn(py));
        (turn, Reading::begin(client))
    }

    /// Take the session's read turn, letting the interpreter go while another
    /// thread holds it: the holder runs Python callbacks, and waiting with the
    /// interpreter held would stop them.
    fn take_the_read_turn(&self, py: Python<'_>) -> std::sync::MutexGuard<'_, ()> {
        loop {
            match self.reading.try_lock() {
                Ok(turn) => return turn,
                Err(std::sync::TryLockError::Poisoned(e)) => return e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    py.detach(|| std::thread::sleep(std::time::Duration::from_micros(50)));
                }
            }
        }
    }

    /// Records held back while a fill for them was queued, freed once that
    /// fill has been read. Freed here and nowhere else: this is the side that
    /// reads the fills, so a record cannot be freed between a fill being taken
    /// off the queue and the report that is built from it.
    fn free_what_the_fills_held_back(&self, shared: &SharedState) {
        if !self.deferred_evictions.lock().unwrap().is_empty() {
            self.deferred_evictions.lock().unwrap().retain(|oid| {
                if shared.orders.has_pending_fill(*oid) {
                    return true;
                }
                // The row goes only while the order is finished. A correction
                // the venue sends after the completion was delivered puts the
                // order back in the book, and taken then, the row removed is
                // the live one.
                shared.orders.remove_completed_order_info(*oid);
                false
            });
        }
    }

    /// The conflated state, as a read's first step.
    fn poll_the_state(&self, shared: &SharedState) -> Polled {
        let multi = self.core.multi_watchers();
        let positions_watched = self.positions_requested.load(Ordering::Acquire)
            || !self.core.positions_watchers().is_empty();
        self.core.poll_conflated(shared, positions_watched, &multi)
    }

    /// The session's last record: `connection_closed`, once, after everything
    /// before it and the final state, and nothing for the session after it.
    /// What this side keeps about the session's requests is reset once it has
    /// been said.
    fn deliver_the_close(&self, py: Python<'_>) -> PyResult<()> {
        self.connected.store(false, Ordering::Release);
        self.session_ended.store(true, Ordering::Release);
        let said = self.tell_the_caller_it_closed(py);
        self.core.reset();
        said
    }

    /// The per-request position watchers, in order.
    fn multi_position_watchers(&self) -> Vec<i64> {
        self.core.positions_watchers()
    }

    // ── Records ──

    fn deliver_record(
        &self, py: Python<'_>, shared: &Arc<SharedState>, record: Record, polled: &mut Option<Polled>,
    ) -> PyResult<()> {
        match record {
            Record::Closed => {}
            // The connection going, said under 1100 unless it was asked for,
            // and its return under 1102. Each is a record, pushed as the
            // connection flag flips, so they are said in the order they
            // happened and each once.
            Record::ConnectionLost { by_design } => {
                self.connected.store(false, Ordering::Release);
                if !by_design {
                    say_error!(self, py, shared, crate::types::model::ErrorOrigin::Session, 0, 1100,
                        "Connectivity between client and server has been lost");
                }
            }
            Record::ConnectionRestored => {
                self.connected.store(true, Ordering::Release);
                say_error!(self, py, shared, crate::types::model::ErrorOrigin::Session, 0, 1102,
                    "Connectivity between client and server has been restored - data maintained");
            }
            // One of the connections the venue keeps data on went away or
            // came back, under the number the venue reports it under.
            Record::VenueData((which, up)) => {
                if matches!(which, crate::bridge::VenueDataConnection::MarketData) && !up {
                    self.core.forget_last_quotes();
                }
                let (broken, ok) = which.codes();
                say_error!(self, py, shared, crate::types::model::ErrorOrigin::Session, 0, if up { ok } else { broken }, which.says(up));
            }
            Record::SlotTaken { slot, generation } => self.core.note_slot_taken(slot, generation),
            Record::SlotReleased { slot, generation } => self.core.note_slot_released(slot, generation),
            Record::OrderBook(entry) => self.core.keep_the_book(shared, entry),
            // A withdrawal the engine confirmed, where it stands: what the
            // withdrawn exchange watched stops here, after everything it
            // answered. Nothing is said: a gateway says nothing at a cancel,
            // and the reference client has no callback for this.
            Record::Retired(what) => match what {
                crate::types::Retirement::Question(crate::types::model::Question::Positions) => {
                    self.positions_requested.store(false, Ordering::Release);
                }
                crate::types::Retirement::Question(crate::types::model::Question::AccountUpdates) => {
                    if let Some(state) = polled { state.account = None; state.portfolio.clear(); }
                    self.core.subscribe_account_updates(false);
                }
                crate::types::Retirement::Question(_) => {}
                crate::types::Retirement::PositionsMulti(req_id) => {
                    self.core.forget_positions_account(req_id);
                }
                crate::types::Retirement::AccountUpdatesMulti(req_id) => {
                    self.core.forget_account_figures_for(req_id);
                    self.core.forget_multi_account(req_id);
                    self.core.ledger_only_for(req_id, false);
                }
            },

            // What was said about an order that went anyway, on its number.
            Record::OrderNotice((order_id, code, msg, op)) => {
                say_error!(self, py, shared, crate::types::model::ErrorOrigin::Order { id: self.core.api_order_id(order_id), op }, 0, i64::from(code), &msg);
            }
            Record::Fill(fill) => self.deliver_fill(py, shared, fill)?,
            Record::OrderUpdate(update) => self.deliver_update(py, shared, update)?,
            // What the venue says a fill cost, naming the execution it
            // belongs to: pushed after the fill, so the fill is stored.
            Record::Charge(charge) => {
                self.core.record_charge(&charge);
                let report = CommissionAndFeesReport {
                    exec_id: charge.exec_id.clone(),
                    commission_and_fees: charge.commission_and_fees,
                    currency: charge.currency.clone(),
                    realized_pnl: charge.realized_pnl,
                    yield_amount: charge.yield_amount,
                    yield_redemption_date: charge.yield_redemption_date,
                };
                let report_py = Py::new(py, report)?.into_any();
                call_wrapper!(self, py, shared, "commission_and_fees_report", (&report_py,));
            }
            // Executions the venue restated rather than announced. Filed for
            // `req_executions` and reported to nobody.
            Record::RestatedExecution(restated) => {
                let (contract, execution) = *restated;
                self.core.push_execution(contract, execution, ApiCommissionAndFeesReport::default());
            }
            // A replacement the venue has taken spends the terms kept against
            // a refusal of it, in its place.
            Record::ReplacementTaken(order_id) => self.core.settle_replacement(order_id),
            Record::CancelReject(reject) => {
                let (code, msg) = self.core.retire_rejected(&reject);
                let origin = crate::types::model::ErrorOrigin::Order { id: self.core.api_order_id(reject.order_id), op: reject.refuses() };
                say_error!(self, py, shared, origin, 0, code, &msg);
            }
            Record::OrderInactive((order_id, code, msg, op)) => {
                // A refusal is the end of a preview: it states what an order
                // would have cost, and nothing reached the book.
                if self.core.tracked_order(order_id).is_some_and(|o| o.what_if) {
                    self.core.untrack_order(order_id);
                }
                say_error!(self, py, shared, crate::types::model::ErrorOrigin::Order { id: self.core.api_order_id(order_id), op }, 0, i64::from(code), &msg);
            }
            // A preview and nothing else, answered on the order itself.
            Record::WhatIf(wi) => {
                let state = OrderState::from_api(&crate::types::model::OrderState::from(&wi));
                // The preview is complete before the callback can place the
                // order under this number or interrupt the read.
                let tracked = self.core.open_orders.lock().unwrap().remove(&wi.order_id);
                let (contract_py, order_py) = if let Some(t) = tracked {
                    let c = Contract::from_api(py, &t.contract)?;
                    let o = Order::from_api(py, &t.order)?;
                    (Py::new(py, c)?.into_any(), Py::new(py, o)?.into_any())
                } else {
                    (Py::new(py, Contract::default())?.into_any(),
                     Py::new(py, Order::default())?.into_any())
                };
                let state_py = Py::new(py, state)?.into_any();
                call_wrapper!(self, py, shared, "open_order",
                    (self.core.api_order_id(wi.order_id), &contract_py, &order_py, &state_py));
            }

            // What each subscription was acknowledged with, to whoever joined
            // it and to everyone watching the contract that held the slot.
            Record::TickReqParamsFor((req_id, _)) => {
                if let Some(p) = self.core.watching(req_id)
                    .and_then(|instrument| shared.market.tick_req_params_for_follower(instrument))
                    && self.core.should_send_tick_req_params(req_id)
                {
                    call_wrapper!(self, py, shared, "tick_req_params",
                        (req_id, p.min_tick, p.bbo_exchange.as_str(), p.snapshot_permissions));
                }
            }
            Record::SubscriptionFailureFor((req_id, reason)) => {
                    say_error!(self, py, shared, crate::types::model::ErrorOrigin::Request { id: req_id, ends: true }, 0, 200, &reason);
            }
            Record::TickReqParams((instrument, generation, _)) => {
                if generation == self.core.generation_held(instrument) {
                    for req_id in self.core.watchers_of(instrument) {
                        if let Some(p) = shared.market.tick_req_params_for_follower(instrument)
                            && self.core.should_send_tick_req_params(req_id)
                        {
                            call_wrapper!(self, py, shared, "tick_req_params",
                        (req_id, p.min_tick, p.bbo_exchange.as_str(), p.snapshot_permissions));
                        }
                    }
                }
            }
            Record::TbtTrade(trade) => {
                // As the caller numbered it, from the record itself.
                let req_id = trade.req_id;
                let price = trade.price as f64 / PRICE_SCALE_F;
                let size = trade.size as f64 / crate::types::QTY_SCALE as f64;
                // What the venue said about this print, not what a default says.
                let attrib = super::super::tick_types::TickAttribLast {
                    past_limit: trade.past_limit,
                    unreported: trade.unreported,
                };
                let attrib_obj = Py::new(py, attrib)?.into_any();
                // The stream this request asked for, as the record carries it
                // from its stream: 1 = Last, 2 = AllLast.
                let kind = if trade.kind == crate::types::TbtType::AllLast { 2 } else { 1 };
                call_wrapper!(self, py, shared, "tick_by_tick_all_last", (req_id, kind, trade.timestamp as i64, price, size,
                     &attrib_obj, trade.exchange.as_str(), trade.conditions.as_str()));
            }
            Record::TbtQuote(quote) => {
                let attrib = super::super::tick_types::TickAttribBidAsk {
                    bid_past_low: quote.bid_past_low,
                    ask_past_high: quote.ask_past_high,
                };
                let attrib_obj = Py::new(py, attrib)?.into_any();
                call_wrapper!(self, py, shared, "tick_by_tick_bid_ask", (quote.req_id, quote.timestamp as i64,
                     quote.bid as f64 / PRICE_SCALE_F, quote.ask as f64 / PRICE_SCALE_F,
                     quote.bid_size as f64 / crate::types::QTY_SCALE as f64,
                     quote.ask_size as f64 / crate::types::QTY_SCALE as f64, &attrib_obj));
            }
            // The point between the two, each time it moved.
            Record::TbtMid(mid) => {
                call_wrapper!(self, py, shared, "tick_by_tick_mid_point",
                    (mid.req_id, mid.timestamp as i64, mid.price as f64 / PRICE_SCALE_F));
            }
            // A book this client could not keep whole, on the request that
            // asked for it. 354 is what the reference client reports when data
            // asked for is not served.
            Record::DepthDrop((req_id, reason)) => {
                let origin = crate::types::model::ErrorOrigin::Request { id: i64::from(req_id), ends: true };
                say_error!(self, py, shared, origin, super::raised_now(), 354, &reason);
            }
            Record::DepthUpdate(du) => {
                if du.market_maker.is_empty() {
                    call_wrapper!(self, py, shared, "update_mkt_depth", (du.req_id as i64, du.position, du.operation, du.side, du.price, du.size));
                } else {
                    call_wrapper!(self, py, shared, "update_mkt_depth_l2", (du.req_id as i64, du.position, du.market_maker.as_str(),
                         du.operation, du.side, du.price, du.size, du.is_smart_depth));
                }
            }
            // Once per caller watching the contract that held the slot.
            Record::TickNews((generation, news)) => {
                if generation == self.core.generation_held(news.instrument) {
                    for id in self.core.watchers_of(news.instrument) {
                        call_wrapper!(self, py, shared, "tick_news", (id, news.timestamp as i64, news.provider_code.as_str(),
                             news.article_id.as_str(), news.headline.as_str(), ""));
                    }
                }
            }
            // A calculation's answer to the request that asked; the venue's
            // model to every request watching the contract.
            Record::OptionComputation((generation, comp)) => {
                let (to, tick_type): (Vec<i64>, i32) = match comp.answers {
                    Some(asked) => (vec![asked], ASKED_OPTION_COMPUTATION),
                    None if generation != self.core.generation_held(comp.instrument) => {
                        (Vec::new(), MODEL_OPTION_COMPUTATION)
                    }
                    None => (
                        self.core.watchers_of(comp.instrument),
                        if self.core.feed_is_delayed(comp.instrument) {
                            DELAYED_MODEL_OPTION_COMPUTATION
                        } else {
                            MODEL_OPTION_COMPUTATION
                        },
                    ),
                };
                for req_id in to {
                    // The model is one of the kinds an option's snapshot
                    // waits for.
                    self.core.note_snapshot_tick(req_id, tick_type);
                    call_wrapper!(self, py, shared, "tick_option_computation",
                        (req_id, tick_type, 0i32,
                         or_unstated_price(comp.implied_vol).filter(|v| *v >= 0.0), or_unstated_greek(comp.delta),
                         or_unstated_price(comp.opt_price), or_unstated_price(comp.pv_dividend),
                         or_unstated_greek(comp.gamma), or_unstated_greek(comp.vega),
                         or_unstated_greek(comp.theta), or_unstated_price(comp.und_price)));
                }
            }
            Record::VenueError(text) => {
                say_error!(self, py, shared, crate::types::model::ErrorOrigin::Session, super::raised_now(), 2148, &text);
            }
            // A lookup that named a contract another slot already holds.
            // A market-data request the engine has taken, and one withdrawn.
            Record::MarketDataTaken(taken) => self.core.note_mkt_data_taken(shared, &taken),
            Record::MarketDataWithdrawn(req_id) => self.core.unregister_mkt_data(req_id),
            // Everyone watching the contract that held the slot.
            Record::MarketDataType((instrument, generation, data_type)) => {
                if generation == self.core.generation_held(instrument) {
                    self.core.note_mkt_data_type(instrument, data_type);
                }
            }
            Record::SubscriptionNotice((instrument, generation, notice)) => {
                if generation == self.core.generation_held(instrument) {
                    for req_id in self.core.watchers_of(instrument) {
                        let origin = crate::types::model::ErrorOrigin::Request { id: req_id, ends: false };
                        say_error!(self, py, shared, origin, 0, i64::from(notice.code), &notice.message);
                    }
                }
            }
            Record::SubscriptionFailure((instrument, generation, reason)) => {
                if generation == self.core.generation_held(instrument) {
                    for req_id in self.core.watchers_of(instrument) {
                        let origin = crate::types::model::ErrorOrigin::Request { id: req_id, ends: true };
                        say_error!(self, py, shared, origin, 0, 200, &reason);
                    }
                }
            }
            Record::CompanionRefusal((instrument, generation, _kind, reason)) => {
                if generation == self.core.generation_held(instrument) {
                    for req_id in self.core.watchers_of(instrument) {
                        // The quote it rides beside goes on.
                        let origin = crate::types::model::ErrorOrigin::Request { id: req_id, ends: false };
                        say_error!(self, py, shared, origin, 0, 321, &reason);
                    }
                }
            }
            Record::NewsBulletin(b) => {
                call_wrapper!(self, py, shared, "update_news_bulletin", (b.msg_id as i64, b.msg_type, b.message.as_str(), b.exchange.as_str()));
            }
            Record::RealTimeBar((req_id, bar)) => {
                if self.core.hist_initial_complete.lock().unwrap().contains(&req_id) {
                    // keepUpToDate bar → dispatch as historical_data_update,
                    // dated as the history before it was.
                    let bar_obj = BarData::new(
                        self.core.bar_time_for_epoch(req_id as i64, i64::from(bar.timestamp)),
                        bar.open, bar.high, bar.low, bar.close,
                        bar.volume as i64, bar.wap, bar.count,
                        String::new(), // streaming bars carry no timezone
                        // A forming bar has not ended, and the stream states no
                        // end for one.
                        String::new(),
                    );
                    let bar_py = Py::new(py, bar_obj)?.into_any();
                    call_wrapper!(self, py, shared, "historical_data_update", (req_id as i64, &bar_py));
                } else {
                    call_wrapper!(self, py, shared, "real_time_bar", (
                        req_id as i64,
                        bar.timestamp as i64,
                        bar.open, bar.high, bar.low, bar.close,
                        bar.volume, bar.wap, bar.count,
                    ));
                }
            }

            // What the venue refused under a request, in its place.
            Record::HistoricalError((origin, code, msg)) => {
                say_error!(self, py, shared, origin, 0, i64::from(code), &msg);
            }
            Record::HistoricalData((req_id, response)) => {
                let is_update = self.core.hist_initial_complete.lock().unwrap().contains(&req_id);
                self.core.note_historical_zone(req_id as i64, &response.timezone);
                for bar in &response.bars {
                    let bar_obj = BarData::new(
                        self.core.historical_bar_time_for(req_id as i64, bar, &response.timezone),
                        bar.open, bar.high, bar.low, bar.close,
                        bar.volume, bar.wap, bar.count,
                        response.timezone.clone(),
                        bar.end.clone(),
                    );
                    let bar_py = Py::new(py, bar_obj)?.into_any();
                    if is_update {
                        call_wrapper!(self, py, shared, "historical_data_update", (req_id as i64, &bar_py));
                    } else {
                        call_wrapper!(self, py, shared, "historical_data", (req_id as i64, &bar_py));
                    }
                }
                if response.is_complete && !is_update {
                    self.core.hist_initial_complete.lock().unwrap().insert(req_id);
                    // The range the request covered, which a caller paging
                    // backwards feeds in as its next end.
                    let (from, to) =
                        self.core.historical_range_for(req_id as i64, &response.timezone);
                    call_wrapper!(self, py, shared, "historical_data_end",
                        (req_id as i64, from.as_str(), to.as_str()));
                }
            }
            Record::OrderBound((perm_id, client_id, order_id)) => {
                call_wrapper!(self, py, shared, "order_bound", (perm_id, client_id, order_id));
            }
            Record::HeadTimestamp((req_id, response)) => {
                // Seconds since the epoch where the caller asked for them.
                let stated = if self.core.asked_date_format(req_id as i64) == 2 {
                    self.core.bar_time_for(req_id as i64, &response.head_timestamp, "")
                } else {
                    response.head_timestamp.clone()
                };
                call_wrapper!(self, py, shared, "head_timestamp", (req_id as i64, stated.as_str()));
            }
            Record::ContractDetails((req_id, def)) => {
                let details = ContractDetails::from_definition(py, &def);
                let details_py = Py::new(py, details)?.into_any();
                // Fixed income answers on its own callback.
                let named = if def.sec_type.is_fixed_income() {
                    "bond_contract_details"
                } else {
                    "contract_details"
                };
                call_wrapper!(self, py, shared, named, (req_id as i64, &details_py));
            }
            Record::ContractDetailsEnd(req_id) => {
                call_wrapper!(self, py, shared, "contract_details_end", (req_id as i64,));
            }
            Record::CalendarMeta((req_id, json)) => {
                call_wrapper!(self, py, shared, "wsh_meta_data", (req_id as i64, json.as_str()));
            }
            Record::CalendarEvents((req_id, json)) => {
                call_wrapper!(self, py, shared, "wsh_event_data", (req_id as i64, json.as_str()));
            }
            Record::MatchingSymbols((req_id, matches)) => {
                let descriptions: Vec<Py<ContractDescription>> = matches.iter().map(|m| {
                    Py::new(py, ContractDescription {
                        contract: Py::new(py, Contract {
                            con_id: m.con_id as i64,
                            symbol: m.symbol.clone(),
                            // The user-visible spelling, the same one the Rust
                            // surface hands back.
                            sec_type: m.sec_type.to_api_str().to_string(),
                            currency: m.currency.clone(),
                            primary_exchange: m.primary_exchange.clone(),
                            // The venue's own words, and the id it gives an
                            // issuer.
                            description: m.description.clone(),
                            issuer_id: m.issuer_id.clone(),
                            ..Default::default()
                        }).unwrap(),
                        derivative_sec_types: crate::python::compat::class_contracts::ListField::of(py, m.derivative_types.clone()).unwrap_or_default(),
                    }).unwrap()
                }).collect();
                let list = pyo3::types::PyList::new(py, &descriptions)?;
                call_wrapper!(self, py, shared, "symbol_samples", (req_id as i64, list.as_any()));
            }
            Record::OptionParams((req_id, underlying_con_id, scopes)) => {
                for scope in &scopes {
                    let expirations = pyo3::types::PyList::new(py, &scope.expirations)?;
                    let strikes = pyo3::types::PyList::new(py, &scope.strikes)?;
                    call_wrapper!(self, py, shared, "security_definition_option_parameter",
                        (req_id as i64, scope.exchange.as_str(), underlying_con_id,
                         scope.trading_class.as_str(), scope.multiplier.as_str(),
                         expirations.as_any(), strikes.as_any()));
                }
                call_wrapper!(self, py, shared, "security_definition_option_parameter_end", (req_id as i64,));
            }
            Record::DepthExchanges(depth_exchanges) => {
                let descriptions: Vec<Py<DepthMktDataDescriptionPy>> = depth_exchanges.iter().map(|d| {
                    Py::new(py, DepthMktDataDescriptionPy {
                        exchange: d.exchange.clone(),
                        sec_type: d.sec_type.clone(),
                        listing_exch: d.listing_exch.clone(),
                        service_data_type: d.service_data_type.clone(),
                        agg_group: d.agg_group,
                    }).unwrap()
                }).collect();
                let list = pyo3::types::PyList::new(py, &descriptions)?;
                call_wrapper!(self, py, shared, "mkt_depth_exchanges", (list.as_any(),));
            }
            Record::ScannerParams(xml) => {
                call_wrapper!(self, py, shared, "scanner_parameters", (xml.as_str(),));
            }
            // The advisor's own configuration: a partition the caller asked
            // for, the end of one they replaced, and the venue's account of a
            // replacement it would not take.
            Record::AdvisorConfig((fa_data_type, xml)) => {
                call_wrapper!(self, py, shared, "receive_fa", (fa_data_type, xml.as_str()));
            }
            Record::AdvisorReplaced((req_id, text)) => {
                call_wrapper!(self, py, shared, "replace_fa_end", (req_id, text.as_str()));
            }
            Record::AdvisorRefused((origin, code, text)) => {
                say_error!(self, py, shared, origin, super::raised_now(), i64::from(code), &text);
            }
            Record::ScannerData((req_id, result)) => {
                // A refused scan arrives in the shape of a completed one and
                // carries the reason, reported against the requesting id.
                if !result.error_text.is_empty() {
                    let origin = crate::types::model::ErrorOrigin::Request { id: i64::from(req_id), ends: true };
                    say_error!(self, py, shared, origin, 0, 321, &result.error_text);
                }
                for (rank, entry) in result.entries.iter().enumerate() {
                    let cd_py = self.scanned_details(py, entry, shared)?.into_any();
                    call_wrapper!(self, py, shared, "scanner_data", (req_id as i64, rank as i32, &cd_py, "", "", "", ""));
                }
                call_wrapper!(self, py, shared, "scanner_data_end", (req_id as i64,));
            }
            Record::HistoricalNews((req_id, headlines, has_more)) => {
                for h in &headlines {
                    call_wrapper!(self, py, shared, "historical_news", (req_id as i64, h.time.as_str(), h.provider_code.as_str(),
                         h.article_id.as_str(), h.headline.as_str()));
                }
                call_wrapper!(self, py, shared, "historical_news_end", (req_id as i64, has_more));
            }
            Record::NewsArticle((req_id, article_type, text)) => {
                call_wrapper!(self, py, shared, "news_article", (req_id as i64, article_type, text.as_str()));
            }
            Record::FundamentalData((req_id, data)) => {
                call_wrapper!(self, py, shared, "fundamental_data", (req_id as i64, data.as_str()));
            }
            Record::HistogramData((req_id, entries)) => {
                // Each bucket as the reference client hands it over: a record
                // naming `price` and `size`, not a pair.
                let mut buckets = Vec::with_capacity(entries.len());
                for e in entries.iter() {
                    buckets.push(Py::new(py, crate::python::compat::class_reports::HistogramDataPy {
                        price: e.price,
                        size: e.count as f64,
                    })?);
                }
                let py_list = pyo3::types::PyList::new(py, buckets)?;
                call_wrapper!(self, py, shared, "histogram_data", (req_id as i64, py_list));
            }
            Record::HistoricalTicks((req_id, data, _what, done)) => {
                self.deliver_historical_ticks(py, shared, req_id, data, done)?;
            }
            Record::HistoricalSchedule((req_id, resp)) => {
                // Each session as the reference client states one.
                let mut sessions = Vec::with_capacity(resp.sessions.len());
                for s in resp.sessions.iter() {
                    sessions.push(Py::new(py, crate::python::compat::class_reports::HistoricalSessionPy {
                        start_date_time: s.open_time.clone(),
                        end_date_time: s.close_time.clone(),
                        ref_date: s.ref_date.clone(),
                    })?);
                }
                let py_sessions = pyo3::types::PyList::new(py, sessions)?;
                call_wrapper!(self, py, shared, "historical_schedule", (
                    req_id as i64,
                    resp.start_date_time.as_str(),
                    resp.end_date_time.as_str(),
                    resp.timezone.as_str(),
                    py_sessions,
                ));
            }

            // A refusal made at a call, in its place: after everything pushed
            // before the call.
            Record::Refused((origin, code, msg)) => {
                let (name, args) = self.error_callback(py, origin, super::raised_now(), code, &msg)?;
                answer_wrapper!(self, py, shared, name, args.bind(py).clone());
            }
            // Composed by the read's loop, which hands its callbacks over one
            // at a time.
            Record::Answer(_) => {}
            Record::Reply(reply) => self.deliver_reply(py, shared, reply)?,
        }
        Ok(())
    }

    /// Historical ticks, each as the reference client hands one over: a
    /// record with names on it.
    fn deliver_historical_ticks(
        &self, py: Python<'_>, shared: &Arc<SharedState>, req_id: u32,
        data: crate::types::HistoricalTickData, done: bool,
    ) -> PyResult<()> {
        // The venue states the moment as it spells it; the reference client
        // states it in seconds. A stamp that cannot be read back leaves the
        // tick out rather than putting it in 1970.
        let at = crate::protocol::datetime::ib_datetime_to_unix;
        let dropped = std::cell::Cell::new(0usize);
        match data {
            crate::types::HistoricalTickData::Midpoint(ticks) => {
                let py_ticks: Vec<crate::python::compat::tick_types::HistoricalTick> = ticks.iter().filter_map(|t| Some(crate::python::compat::tick_types::HistoricalTick {
                    time: at(&t.time).or_else(|| { dropped.set(dropped.get() + 1); None })?,
                    price: t.price,
                    // A midpoint has no size, and the reference client states
                    // zero for it.
                    size: 0.0,
                })).collect();
                let list = pyo3::types::PyList::new(py, py_ticks)?;
                call_wrapper!(self, py, shared, "historical_ticks", (req_id as i64, list, done));
            }
            crate::types::HistoricalTickData::Last(ticks) => {
                let py_ticks: Vec<crate::python::compat::tick_types::HistoricalTickLast> = ticks.iter().filter_map(|t| Some(crate::python::compat::tick_types::HistoricalTickLast {
                    time: at(&t.time).or_else(|| { dropped.set(dropped.get() + 1); None })?,
                    tick_attrib_last: crate::python::compat::tick_types::TickAttribLast {
                        past_limit: t.past_limit,
                        unreported: t.unreported,
                    },
                    price: t.price,
                    size: t.size,
                    exchange: t.exchange.clone(),
                    special_conditions: t.special_conditions.clone(),
                })).collect();
                let list = pyo3::types::PyList::new(py, py_ticks)?;
                call_wrapper!(self, py, shared, "historical_ticks_last", (req_id as i64, list, done));
            }
            crate::types::HistoricalTickData::BidAsk(ticks) => {
                let py_ticks: Vec<crate::python::compat::tick_types::HistoricalTickBidAsk> = ticks.iter().filter_map(|t| Some(crate::python::compat::tick_types::HistoricalTickBidAsk {
                    time: at(&t.time).or_else(|| { dropped.set(dropped.get() + 1); None })?,
                    tick_attrib_bid_ask: crate::python::compat::tick_types::TickAttribBidAsk {
                        bid_past_low: t.bid_past_low,
                        ask_past_high: t.ask_past_high,
                    },
                    price_bid: t.bid_price,
                    price_ask: t.ask_price,
                    size_bid: t.bid_size,
                    size_ask: t.ask_size,
                })).collect();
                let list = pyo3::types::PyList::new(py, py_ticks)?;
                call_wrapper!(self, py, shared, "historical_ticks_bid_ask", (req_id as i64, list, done));
            }
        }
        if dropped.get() > 0 {
            // Told to the caller, not only to the log: a shortened series and
            // a complete one look the same to a program charting it.
            let why = format!(
                "{} historical tick(s) state a moment that cannot be read back, and are \
                 left out of this answer rather than dated to 1970",
                dropped.get(),
            );
            // A notice: the ticks it leaves out are the only ones left out.
            let origin = crate::types::model::ErrorOrigin::Request { id: i64::from(req_id), ends: false };
            say_error!(self, py, shared, origin, super::raised_now(), crate::error_codes::Refusal::VALIDATION as i64, &why);
        }
        Ok(())
    }

    /// A fill and the status stated on the same report: `order_status`, then
    /// `exec_details`, each with what the report stated.
    fn deliver_fill(&self, py: Python<'_>, shared: &Arc<SharedState>, record: FillRecord) -> PyResult<()> {
        let FillRecord { fill, report: rich_info, status: with_it } = record;
        // A fill nobody asked for is numbered -1. The reference wrapper
        // decides a fill is live by the request id not matching one it is
        // waiting on, so any other id files the fill as the answer to that
        // request and suppresses the fill event.
        let req_id = -1i64;
        // The venue's two words for a side. A short sale is sold.
        let side_str = match fill.side {
            Side::Buy => "BOT",
            Side::Sell | Side::ShortSell => "SLD",
        };
        let price = fill.price as f64 / PRICE_SCALE_F;
        // The status the report carries. Derived from the remaining quantity
        // only when the report states none.
        let status = with_it
            .as_ref()
            .map(|u| order_status_str(u.status))
            .unwrap_or(if fill.remaining == 0 { "Filled" } else { "Submitted" });
        if let Some(u) = &with_it {
            self.core.update_order_status(
                shared, u.order_id, u.status, u.filled_qty, u.remaining_qty, u.instrument,
            );
        }
        self.core.learn_order_identity(shared, fill.order_id);
        let (perm_id, parent_id) = self.core.perm_and_parent_stated(
            fill.order_id, rich_info.as_deref(), with_it.as_ref(),
        );
        let client = self.core.client_stated(fill.order_id, rich_info.as_deref());

        // What the report stated beyond the print, taken before anything
        // consumes the record. The venue's own execution id and time, not
        // ones composed here.
        let from_the_report = rich_info
            .as_ref()
            .map(|info| info.last_exec.clone())
            .unwrap_or_default();
        let exec_id = rich_info.as_ref().map(|i| i.last_exec.exec_id.clone()).unwrap_or_default();
        let now_str = rich_info.as_ref().map(|i| i.last_exec.time.clone()).unwrap_or_default();
        let exec_exchange = rich_info.as_ref()
            .map(|i| i.last_exec.exchange.as_str()).unwrap_or("").to_string();
        // What the report stated about the order so far, and nothing where it
        // stated nothing.
        let cum_qty = rich_info.as_ref().map(|i| i.last_exec.cum_qty).unwrap_or_default();
        let avg_price = rich_info.as_ref().map(|i| i.last_exec.avg_price).unwrap_or_default();
        // The contract the venue stated on the report, filled in from the
        // reference cache by its id, and the one the caller typed only where
        // there is no report.
        let api_contract = rich_info
            .as_ref()
            .map(|info| {
                if info.contract.con_id != 0 {
                    self.core.get_contract(info.contract.con_id, shared).unwrap_or_else(|| info.contract.clone())
                } else {
                    info.contract.clone()
                }
            })
            .or_else(|| self.core.open_orders.lock().unwrap().get(&fill.order_id).map(|o| o.contract.clone()))
            .unwrap_or_default();
        // Everything the report stated, with the print's own numbers over it.
        let api_exec = ApiExecution {
            exec_id,
            time: now_str,
            exchange: exec_exchange,
            side: side_str.to_string(),
            shares: qty_to_f64(fill.qty),
            price,
            order_id: self.core.api_order_id(fill.order_id),
            perm_id,
            // The report's own where it names one, and the client that placed
            // the order where it names none.
            client_id: match rich_info.as_ref() {
                Some(info) if info.last_exec.client_id != 0 => info.last_exec.client_id,
                _ => i64::from(client),
            },
            cum_qty,
            avg_price,
            ..from_the_report
        };
        // What it cost arrives on a record of its own, after this. Stored
        // unstated so a replay of this execution says the charge is unknown.
        let api_commission = ApiCommissionAndFeesReport::default();

        let c_py = Py::new(py, Contract::from_api(py, &api_contract)?)?.into_any();
        let exec_py = Py::new(py, Execution::from_api(&api_exec))?.into_any();
        // Kept for `req_executions` to answer from, before either callback
        // about the print.
        self.core.push_execution(api_contract, api_exec, api_commission);
        // `filled` and `avgFillPrice` describe the order so far;
        // `lastFillPrice` describes this print.
        call_wrapper!(self, py, shared, "order_status", (self.core.api_order_id(fill.order_id), status, qty_to_f64(fill.cum_qty), qty_to_f64(fill.remaining),
             fill.avg_price as f64 / PRICE_SCALE_F, perm_id, parent_id, price,
             i64::from(client), "", 0.0f64));
        call_wrapper!(self, py, shared, "exec_details", (req_id, &c_py, &exec_py));
        self.core.update_order_fill(fill.order_id, status, qty_to_f64(fill.cum_qty), qty_to_f64(fill.remaining));
        Ok(())
    }

    /// A status change with no fill on the same report: `open_order` with the
    /// order's state as the report stated it, then `order_status`.
    fn deliver_update(&self, py: Python<'_>, shared: &Arc<SharedState>, record: UpdateRecord) -> PyResult<()> {
        let UpdateRecord { update, state: stated_state, client_id } = record;
        let status = order_status_str(update.status);
        // The engine reads no parent from the report, but this client placed
        // the order and was told.
        let parent_id = self.core.tracked_parent_id(update.order_id)
            .unwrap_or(update.parent_id);
        let avg = update.avg_price as f64 / crate::types::PRICE_SCALE as f64;
        // The order as this client sent it, beside the status it is now in.
        // Copied out before the callback rather than read across it.
        let tracked = self.core.open_orders.lock().unwrap().get(&update.order_id).cloned();
        let client = tracked
            .as_ref()
            .map(|t| t.order.client_id)
            .filter(|c| *c != 0)
            .unwrap_or(client_id);
        if let Some(tracked) = tracked {
            let contract_py = Py::new(py, Contract::from_api(py, &tracked.contract)?)?.into_any();
            let order_py = Py::new(py, Order::from_api(py, &tracked.order)?)?.into_any();
            // What the venue said about the order, under the status this
            // client names it by — as the report that changed the status
            // stated it.
            let stated = crate::types::model::OrderState {
                status: status.to_string(),
                ..stated_state.map(|stated| stated.order_state.clone()).unwrap_or_default()
            };
            let state_py = Py::new(py, OrderState::from_api(&stated))?.into_any();
            call_wrapper!(self, py, shared, "open_order",
                (self.core.api_order_id(update.order_id), &contract_py, &order_py, &state_py));
        }
        call_wrapper!(self, py, shared, "order_status", (self.core.api_order_id(update.order_id), status, update.filled_qty,
             update.remaining_qty, avg, update.perm_id, parent_id, 0.0f64,
             i64::from(client), "", 0.0f64));
        self.core.update_order_status(shared, update.order_id, update.status, update.filled_qty, update.remaining_qty, update.instrument);
        Ok(())
    }

    /// An answer given at the call and carried as a record.
    fn deliver_reply(&self, py: Python<'_>, shared: &Arc<SharedState>, reply: Reply) -> PyResult<()> {
        match reply {
            Reply::DisplayGroupList(req_id, groups) => {
                call_wrapper!(self, py, shared, "display_group_list", (req_id, groups));
            }
            Reply::DisplayGroupUpdated(req_id, info) => {
                call_wrapper!(self, py, shared, "display_group_updated", (req_id, info));
            }
            Reply::CurrentTime(t) => { call_wrapper!(self, py, shared, "current_time", (t,)); }
            Reply::CurrentTimeInMillis(t) => {
                call_wrapper!(self, py, shared, "current_time_in_millis", (t,));
            }
            Reply::ManagedAccounts(list) => {
                call_wrapper!(self, py, shared, "managed_accounts", (list.as_str(),));
            }
            Reply::UserInfo(req_id, id) => { call_wrapper!(self, py, shared, "user_info", (req_id, id)); }
            Reply::SmartComponents(req_id, components) => {
                let list = super::stubs::smart_components_list(py, &components)?;
                call_wrapper!(self, py, shared, "smart_components", (req_id, list.as_any()));
            }
            // This surface answers these with the objects it builds at the
            // call; they are not pushed as records here.
            Reply::NewsProviders(_) | Reply::SoftDollarTiers(..) | Reply::FamilyCodes(_)
            | Reply::MarketRule(..) => {}
        }
        Ok(())
    }

    // ── Answers composed where they stand ──

    /// An answer this side composes, as its state stands at the marker's
    /// place in the order: the callbacks it owes, in order, which the read
    /// then hands over one at a time. An interrupt part way through leaves
    /// the rest for the next read, as it leaves any record.
    fn compose_answer(
        &self, py: Python<'_>, shared: &Arc<SharedState>, answer: Answer, polled: &mut Option<Polled>,
    ) -> PyResult<Vec<(&'static str, Py<pyo3::types::PyTuple>)>> {
        let mut out = Vec::new();
        match answer {
            Answer::Positions => {
                // What moved before this answer is in it. Taken, and handed to
                // the per-request watchers alone, so `position` does not say
                // them twice.
                let mut already_stated = shared.portfolio.drain_position_changes();
                if let Some(state) = polled.as_mut() {
                    already_stated.append(&mut state.positions);
                }
                self.positions_requested.store(true, Ordering::Release);
                let account = self.account();
                for pi in &shared.portfolio.position_infos() {
                    let c_py = Py::new(py, self.position_contract(py, pi, shared)?)?.into_any();
                    let avg_cost = pi.avg_cost as f64 / PRICE_SCALE_F;
                    owed!(out, py, "position", (account.as_str(), &c_py, pi.position, avg_cost));
                }
                owed!(out, py, "position_end", ());
                let watching = self.multi_position_watchers();
                for pi in &already_stated {
                    let c_py = Py::new(py, self.position_contract(py, pi, shared)?)?.into_any();
                    let avg_cost = pi.avg_cost as f64 / PRICE_SCALE_F;
                    for req_id in &watching {
                        if self.core.positions_account(shared, *req_id) != account { continue; }
                        owed!(out, py, "position_multi",
                            (*req_id, account.as_str(), self.core.positions_model(*req_id), &c_py, pi.position, avg_cost));
                    }
                }
            }
            Answer::PositionsMulti { req_id, account, model_code } => {
                let on = shared.account_name(&account);
                let portfolio = shared.portfolio_for(&on);
                let mut moves = std::collections::BTreeMap::new();
                if let Some(state) = polled.as_mut() {
                    if on == self.account() {
                        moves.extend(state.positions.drain(..).map(|p| (p.con_id, p)));
                    } else {
                        state.named_positions.retain(|(a, p)| {
                            if a != &on { return true; }
                            moves.insert(p.con_id, p.clone());
                            false
                        });
                    }
                }
                moves.extend(portfolio.drain_position_changes().into_iter().map(|p| (p.con_id, p)));
                for pi in moves.values() {
                    let c_py = Py::new(py, self.position_contract(py, pi, shared)?)?.into_any();
                    let cost = pi.avg_cost as f64 / PRICE_SCALE_F;
                    if on == self.account() && self.positions_requested.load(Ordering::Acquire) {
                        owed!(out, py, "position", (on.as_str(), &c_py, pi.position, cost));
                    }
                    for old in self.multi_position_watchers() {
                        if old != req_id && self.core.positions_account(shared, old) == on {
                            owed!(out, py, "position_multi", (old, on.as_str(), self.core.positions_model(old), &c_py, pi.position, cost));
                        }
                    }
                }
                self.core.select_positions_account(req_id, &on, &model_code);
                for pi in portfolio.position_infos().iter().filter(|pi| pi.position != 0.0) {
                    let c_py = Py::new(py, self.position_contract(py, pi, shared)?)?.into_any();
                    let avg_cost = pi.avg_cost as f64 / PRICE_SCALE_F;
                    owed!(out, py, "position_multi",
                        (req_id, on.as_str(), model_code.as_str(), &c_py, pi.position, avg_cost));
                }
                owed!(out, py, "position_multi_end", (req_id,));
            }
            Answer::AccountUpdatesMulti { req_id, account, model_code, ledger_and_nlv } => {
                self.core.select_multi_account(req_id, &account, &model_code);
                // Held open from here, and answered with the account whole.
                self.core.ledger_only_for(req_id, ledger_and_nlv);
                self.core.forget_account_figures_for(req_id);
                if let Some(state) = polled.as_mut() {
                    state.multi.retain(|(id, _)| *id != req_id);
                }
                let acct_name = shared.account_name(&account);
                for field in self.core.account_figures_that_moved(shared, req_id) {
                    owed!(out, py, "account_update_multi",
                        (req_id, acct_name.as_str(), model_code.as_str(),
                         field.key.as_str(), field.value.as_str(), field.currency.as_str()));
                }
                owed!(out, py, "account_update_multi_end", (req_id,));
            }
            Answer::AccountUpdates { account } => {
                self.core.select_account_updates(&account);
                // Subscribed from here, where the answer stands.
                self.core.subscribe_account_updates(true);
                if let Some(state) = polled.as_mut() {
                    state.account = None;
                    state.portfolio.clear();
                }
                if let Some(batch) = self.core.prepare_account_updates(shared) {
                    let portfolio = self.core.prepare_portfolio_updates(shared);
                    self.deliver_account_batch(py, shared, batch, portfolio)?;
                }
            }
            Answer::OpenOrders => {
                let orders = self.core.collect_open_orders(shared);
                for (order_id, tracked) in &orders {
                    let c_py = Py::new(py, Contract::from_api(py, &tracked.contract)?)?.into_any();
                    let o_py = Py::new(py, Order::from_api(py, &tracked.order)?)?.into_any();
                    let state = super::super::contract::OrderState {
                        status: tracked.status.clone(),
                        ..Default::default()
                    };
                    let state_py = Py::new(py, state)?.into_any();
                    owed!(out, py, "open_order", (self.core.api_order_id(*order_id), &c_py, &o_py, &state_py));
                    owed!(out, py, "order_status",
                        (self.core.api_order_id(*order_id), tracked.status.as_str(), tracked.filled, tracked.remaining,
                         // What the venue said the fills went at.
                         tracked.avg_fill_price, tracked.order.perm_id, tracked.order.parent_id,
                         tracked.last_fill_price,
                         // The client the order was placed under.
                         tracked.order.client_id as i64, "", 0.0f64));
                }
                owed!(out, py, "open_order_end", ());
            }
            Answer::CompletedOrders { api_only } => {
                self.archive_completed_orders(shared);
                // Copied before anything is called back: a callback may ask
                // for these again, and the lock is not re-entrant.
                let completed = self.completed.lock().unwrap().clone();
                for (contract, order, state) in &completed {
                    // Kept whole in the archive and filtered on the way out.
                    if api_only && !shared.orders.was_entered_through_an_api(
                        order.order_id.max(0) as u64, order.perm_id.max(0) as u64,
                    ) {
                        continue;
                    }
                    let c_py = Py::new(py, Contract::from_api(py, contract)?)?.into_any();
                    let o_py = Py::new(py, Order::from_api(py, order)?)?.into_any();
                    let state_py = Py::new(py, OrderState::from_api(state))?.into_any();
                    owed!(out, py, "completed_order", (&c_py, &o_py, &state_py));
                }
                owed!(out, py, "completed_orders_end", ());
            }
            Answer::NextValidId => {
                owed!(out, py, "next_valid_id", (self.stated_order_id() as i64,));
            }
            Answer::Executions { req_id, filter } => {
                let accounts = self.accounts.lock().unwrap().clone();
                let (snapshot, unheld) = match self.core.executions_for_request(
                    shared, &accounts, &filter, jiff::Timestamp::now(),
                ) {
                    Ok(answer) => answer,
                    Err(why) => {
                        out.push(self.error_callback(
                            py, crate::types::model::ErrorOrigin::Request { id: req_id, ends: true }, super::raised_now(),
                            i64::from(why.code), &why.message,
                        )?);
                        // The end still comes, as it does for every request
                        // refused on this surface.
                        owed!(out, py, "exec_details_end", (req_id,));
                        return Ok(out);
                    }
                };
                if let Some(why) = crate::client_core::ClientCore::unheld_days_notice(&unheld) {
                    log::warn!("{why}");
                    // A notice the executions and their end follow.
                    out.push(self.error_callback(
                        py, crate::types::model::ErrorOrigin::Request { id: req_id, ends: false }, super::raised_now(),
                        crate::error_codes::Refusal::VALIDATION as i64, &why,
                    )?);
                }
                for se in snapshot {
                    let c_py = Py::new(py, Contract::from_api(py, &se.contract)?)?.into_any();
                    let exec_py = Py::new(py, Execution::from_api(&se.execution))?.into_any();
                    owed!(out, py, "exec_details", (req_id, &c_py, &exec_py));
                    // Only where the venue has said what it cost.
                    if !se.commission_and_fees.exec_id.is_empty() {
                        let report = CommissionAndFeesReport {
                            exec_id: se.commission_and_fees.exec_id.clone(),
                            commission_and_fees: se.commission_and_fees.commission_and_fees,
                            currency: se.commission_and_fees.currency.clone(),
                            realized_pnl: se.commission_and_fees.realized_pnl,
                            yield_amount: se.commission_and_fees.yield_amount,
                            yield_redemption_date: se.commission_and_fees.yield_redemption_date,
                        };
                        let report_py = Py::new(py, report)?.into_any();
                        owed!(out, py, "commission_and_fees_report", (&report_py,));
                    }
                }
                owed!(out, py, "exec_details_end", (req_id,));
            }
        }
        Ok(out)
    }

    /// One batch of the account's own figures, its holdings beside them, and
    /// the end where the account has just been stated whole.
    fn deliver_account_batch(
        &self, py: Python<'_>, shared: &Arc<SharedState>,
        batch: crate::client_core::AccountUpdateBatch,
        portfolio: Vec<crate::client_core::PortfolioUpdateEntry>,
    ) -> PyResult<()> {
        let account_name = self.core.updates_account(shared);
        for field in &batch.fields {
            call_wrapper!(self, py, shared, "update_account_value", (field.key.as_str(), field.value.as_str(), field.currency.as_str(), account_name.as_str()));
        }
        for entry in &portfolio {
            let c = match self.core.get_contract(entry.con_id, shared) {
                Some(ac) => Contract::from_api(py, &ac)?,
                None => Contract { con_id: entry.con_id, ..Default::default() },
            };
            let c_py = pyo3::Py::new(py, c).unwrap().into_any();
            call_wrapper!(self, py, shared, "update_portfolio",
                (&c_py, entry.position, entry.market_price, entry.market_value,
                 entry.avg_cost, entry.unrealized_pnl, entry.realized_pnl, account_name.as_str()));
        }
        if batch.finished {
            call_wrapper!(self, py, shared, "update_account_time", ("",));
            call_wrapper!(self, py, shared, "account_download_end", (account_name.as_str(),));
        }
        Ok(())
    }

    // ── State, after the records ──

    /// The state a read took before its cut, under the mapping its records
    /// left: holdings, quotes, the maps of venues answered, and the figures.
    fn deliver_state(&self, py: Python<'_>, shared: &Arc<SharedState>, polled: Polled) -> PyResult<()> {
        let Polled {
            quotes, positions, named_positions, pnl, pnl_single, account, portfolio, multi, summaries,
            smart_components,
        } = polled;

        // A holding that moved since the caller last heard, to whoever still
        // watches. Drained once and given to everyone watching.
        if !positions.is_empty() {
            let on_position = self.positions_requested.load(Ordering::Acquire);
            let per_request = self.multi_position_watchers();
            let account = self.account();
            for pi in &positions {
                let c_py = Py::new(py, self.position_contract(py, pi, shared)?)?.into_any();
                let avg_cost = pi.avg_cost as f64 / crate::types::PRICE_SCALE as f64;
                if on_position {
                    call_wrapper!(self, py, shared, "position",
                        (account.as_str(), &c_py, pi.position, avg_cost));
                }
                for req_id in &per_request {
                    if self.core.positions_account(shared, *req_id) != account { continue; }
                    call_wrapper!(self, py, shared, "position_multi",
                        (*req_id, account.as_str(), self.core.positions_model(*req_id), &c_py, pi.position, avg_cost));
                }
            }
        }

        for (account, pi) in named_positions {
            let c_py = Py::new(py, self.position_contract(py, &pi, shared)?)?.into_any();
            for req_id in self.multi_position_watchers() {
                if self.core.positions_account(shared, req_id) == account {
                    call_wrapper!(self, py, shared, "position_multi",
                        (req_id, account.as_str(), self.core.positions_model(req_id), &c_py, pi.position, pi.avg_cost as f64 / PRICE_SCALE_F));
                }
            }
        }

        self.deliver_quotes(py, shared, quotes)?;
        if !self.is_current_session(shared) { return Ok(()); }

        // Maps of venues asked for before they arrived: answered once they
        // have, or refused once the wait a gateway allows has run out.
        for (req_id, answer) in smart_components {
            match answer {
                Ok(components) => {
                    let list = super::stubs::smart_components_list(py, &components)?;
                    call_wrapper!(self, py, shared, "smart_components", (req_id, list.as_any()));
                }
                Err(why) => {
                    let origin = crate::types::model::ErrorOrigin::Request { id: req_id, ends: true };
                    say_error!(self, py, shared, origin, 0, i64::from(why.code), &why.message);
                }
            }
        }

        for update in pnl {
            call_wrapper!(self, py, shared, "pnl", (update.req_id, update.daily_pnl, update.unrealized_pnl, update.realized_pnl));
        }
        for update in pnl_single {
            call_wrapper!(self, py, shared, "pnl_single", (update.req_id, update.pos, update.daily_pnl,
                 update.unrealized_pnl, update.realized_pnl, update.value));
        }

        if let Some(batch) = account {
            self.deliver_account_batch(py, shared, batch, portfolio)?;
            if !self.is_current_session(shared) { return Ok(()); }
        }

        // The multi-account subscription, a live feed of its own, under each
        // request still watching.
        let watching = self.core.multi_watchers();
        for (req_id, fields) in multi {
            let on = self.core.multi_account(shared, req_id);
            if !watching.contains(&req_id) {
                continue;
            }
            for field in fields {
                call_wrapper!(self, py, shared, "account_update_multi",
                    (req_id, on.as_str(), self.core.multi_model(req_id),
                     field.key.as_str(), field.value.as_str(), field.currency.as_str()));
            }
        }

        // The account summaries due.
        for batch in summaries {
            for entry in &batch.entries {
                call_wrapper!(self, py, shared, "account_summary", (batch.req_id, entry.account.as_str(), entry.tag.as_str(), entry.value.as_str(), entry.currency.as_str()));
            }
            call_wrapper!(self, py, shared, "account_summary_end", (batch.req_id,));
        }
        Ok(())
    }

    /// Each held slot's quote, as ticks against what its callers were last
    /// told, to the requests watching it now — under the occupancy the slot is
    /// held under now.
    fn deliver_quotes(
        &self, py: Python<'_>, shared: &Arc<SharedState>, quotes: Vec<crate::client_core::PolledQuote>,
    ) -> PyResult<()> {
        let instruments = self.core.snapshot_instruments();
        let mut snapshot_done: Vec<(i64, Option<u64>)> = Vec::new();
        for polled in quotes {
            let iid = polled.iid;
            let Some((_, req_id, watchers)) = instruments.iter().find(|(at, ..)| *at == iid).cloned() else {
                continue;
            };
            // A quote of the contract that left the slot is not this one's,
            // and what the caller was last told stays where it was.
            if polled.generation != self.core.generation_held(iid) {
                self.close_finished_snapshots(py, shared, req_id, &watchers, &mut snapshot_done)?;
                continue;
            }
            let result = self.core.ticks_from(shared, polled, req_id);

            // Ahead of everything this read delivers, and to everyone it
            // delivers to: the type a caller is served under is stated before
            // the data it applies to.
            let delivering = result.delivered
                || !result.generic_ticks.is_empty()
                || !result.string_ticks.is_empty()
                || !result.snapshot_ticks.is_empty()
                || result.timestamp.is_some();
            for id in std::iter::once(req_id).chain(watchers.iter().copied()) {
                if let Some(mdt) = self.core.check_mdt_needed(id, delivering) {
                    call_wrapper!(self, py, shared, "market_data_type", (id, mdt));
                }
            }

            // One object per side, because the venue states both sides in one
            // mask and a caller is handed an attribute beside each price.
            let side_attrib = |tick_type: i32| {
                let a = crate::types::quote_attributes(
                    tick_type, result.eligible_mask, result.quote_state_mask,
                );
                TickAttrib {
                    can_auto_execute: a.can_auto_execute,
                    past_limit: a.past_limit,
                    pre_open: a.pre_open,
                }
            };
            let bid_attrib = Py::new(py, side_attrib(1))?.into_any();
            let ask_attrib = Py::new(py, side_attrib(2))?.into_any();
            let plain_attrib = Py::new(py, TickAttrib::default())?.into_any();
            // Which kinds the venue has stated, for anything waiting on a
            // snapshot of this contract.
            for tick in &result.ticks {
                for id in std::iter::once(tick.req_id).chain(watchers.iter().copied()) {
                    self.core.note_snapshot_tick(id, tick.tick_type);
                }
            }
            for tick in &result.ticks {
                for id in std::iter::once(tick.req_id).chain(watchers.iter().copied()) {
                    if tick.is_price {
                        let attrib_obj = match tick.tick_type {
                            1 | 66 => &bid_attrib,
                            2 | 67 => &ask_attrib,
                            _ => &plain_attrib,
                        };
                        call_wrapper!(self, py, shared, "tick_price", (id, tick.tick_type, tick.value, attrib_obj));
                    } else {
                        call_wrapper!(self, py, shared, "tick_size", (id, tick.tick_type, tick.value));
                    }
                }
            }
            for tick in &result.generic_ticks {
                for id in std::iter::once(tick.req_id).chain(watchers.iter().copied()) {
                    call_wrapper!(self, py, shared, "tick_generic", (id, tick.tick_type, tick.value));
                }
            }
            for st in &result.string_ticks {
                for id in std::iter::once(st.req_id).chain(watchers.iter().copied()) {
                    call_wrapper!(self, py, shared, "tick_string", (id, st.tick_type, st.value.as_str()));
                }
            }
            if let Some(ts) = &result.timestamp {
                let ts_secs = ts.timestamp_ns / 1_000_000_000;
                let tick_type = if result.delayed { 88 } else { TICK_LAST_TIMESTAMP };
                // To everyone watching this contract, as the prices and the
                // other strings above are. A delayed feed's time is one of
                // the kinds its snapshot waits for.
                for id in std::iter::once(ts.req_id).chain(watchers.iter().copied()) {
                    self.core.note_snapshot_tick(id, tick_type);
                    call_wrapper!(self, py, shared, "tick_string", (id, tick_type, ts_secs.to_string().as_str()));
                }
            }
            // The answer to a chargeable snapshot, to the snapshot's own
            // requests alone.
            for tick in &result.snapshot_ticks {
                if tick.is_price {
                    let attrib_obj = match tick.tick_type {
                        1 => &bid_attrib,
                        2 => &ask_attrib,
                        _ => &plain_attrib,
                    };
                    call_wrapper!(self, py, shared, "tick_price", (tick.req_id, tick.tick_type, tick.value, attrib_obj));
                } else {
                    call_wrapper!(self, py, shared, "tick_size", (tick.req_id, tick.tick_type, tick.value));
                }
            }
            for st in &result.snapshot_strings {
                call_wrapper!(self, py, shared, "tick_string", (st.req_id, st.tick_type, st.value.as_str()));
            }
            self.close_finished_snapshots(py, shared, req_id, &watchers, &mut snapshot_done)?;
            if !self.is_current_session(shared) { return Ok(()); }
        }
        // A snapshot on a slot this read did not poll still ends at its bound.
        for (_, req_id, watchers) in &instruments {
            self.close_finished_snapshots(py, shared, *req_id, watchers, &mut snapshot_done)?;
            if !self.is_current_session(shared) { return Ok(()); }
        }
        // Withdrawn by this client, not by the caller: a command, and nothing
        // is reported.
        for (req_id, was_watching) in snapshot_done {
            // Only where the number still holds the subscription the snapshot
            // was for.
            if self.core.registration_of(req_id) != was_watching {
                continue;
            }
            let Ok(tx) = self.tx() else { break };
            if let Err(why) = self.withdraw_mkt_data(py, &tx, req_id) {
                log::debug!("withdrawing finished snapshot {req_id}: {why}");
            }
        }
        Ok(())
    }

    /// `tick_snapshot_end` for the holder and everyone watching whose snapshot
    /// is whole, or whose bound has run out.
    fn close_finished_snapshots(
        &self, py: Python<'_>, shared: &Arc<SharedState>, req_id: i64, watchers: &[i64],
        done: &mut Vec<(i64, Option<u64>)>,
    ) -> PyResult<()> {
        for id in std::iter::once(req_id).chain(watchers.iter().copied()) {
            if self.core.check_snapshot_done(id) {
                // What it was watching when the snapshot finished, so the
                // withdrawal can tell this subscription from whatever the
                // callback leaves under the same number.
                let was_watching = self.core.registration_of(id);
                done.push((id, was_watching));
                call_wrapper!(self, py, shared, "tick_snapshot_end", (id,));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod unstated_tests {
    use super::{or_unstated_greek, or_unstated_price};

    /// The wrapper reads `None` where the reference decoder saw an indicator,
    /// including figures held as unset doubles on this side of the boundary.
    #[test]
    fn an_unstated_figure_arrives_as_none() {
        for unstated in [f64::MAX, f64::NAN] {
            assert_eq!(or_unstated_price(unstated), None);
            assert_eq!(or_unstated_greek(unstated), None);
        }
        assert_eq!(or_unstated_price(-1.0), None);
        assert_eq!(or_unstated_greek(-2.0), None);
        assert_eq!(or_unstated_price(-2.0), Some(-2.0));
        assert_eq!(or_unstated_greek(-1.0), Some(-1.0));
        // Everything else is the venue's own figure and passes through, a
        // negative delta and a zero among them.
        for stated in [0.0, -0.42, 1.0, 775.4] {
            assert_eq!(or_unstated_price(stated), Some(stated));
            assert_eq!(or_unstated_greek(stated), Some(stated));
        }
    }

    /// A solve states a volatility against a price and computes no greek.
    #[test]
    fn a_solved_computation_states_no_greek() {
        let solved = crate::types::OptionComputation::solved(7);
        assert_eq!(solved.answers, Some(7));
        for greek in [solved.delta, solved.gamma, solved.vega, solved.theta, solved.pv_dividend] {
            assert_eq!(or_unstated_greek(greek), None, "a greek nobody computed reads as one");
        }
    }
}

#[cfg(test)]
mod withdrawal_tests {
    use super::*;

    /// A snapshot that ends once the engine has gone is withdrawn quietly.
    ///
    /// The pass reaches the withdrawal with the session already recorded as
    /// over. A send that fails there is not an exception out of `run`, which
    /// ends the way the loop means it to, with `connection_closed`.
    #[test]
    fn a_snapshot_ending_after_the_engine_has_gone_does_not_end_the_pass_with_an_error() {
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let wrapper = py
                .eval(c"type('W', (), {'__getattr__': lambda s, n: (lambda *a: None)})()", None, None)
                .unwrap()
                .unbind();
            client.__init__(wrapper).unwrap();
            let shared = Arc::new(SharedState::new());
            shared.market.set_instrument_count(1);
            // The engine's end of the channel is gone.
            let (tx, rx) = std::sync::mpsc::channel();
            drop(rx);
            *client.shared.lock().unwrap() = Some(shared.clone());
            *client.control_tx.lock().unwrap() = Some(tx);
            client.connected.store(true, Ordering::Release);
            // A snapshot asked for long enough ago to be swept on this pass.
            client.core.req_to_instrument.lock().unwrap().insert(1, 0);
            client.core.instrument_to_req.lock().unwrap().insert(0, 1);
            client.core.snapshot_reqs.lock().unwrap().insert(
                1,
                crate::client_core::SnapshotWait {
                    asked_at: std::time::Instant::now() - std::time::Duration::from_secs(12),
                    ..crate::client_core::SnapshotWait::new(0, false)
                },
            );

            client.dispatch_once(py, &shared).expect("the pass ends, saying nothing of the withdrawal");
        });
    }
}

#[cfg(test)]
mod scanner_tests {
    use super::*;

    /// A scan the venue refused arrives in the shape of a completed one.
    /// Handed over as an empty result, the caller was told the market held
    /// no matches where the venue declined the question — told instead, as
    /// on the other surface.
    #[test]
    fn a_refused_scan_is_reported_not_delivered_as_an_empty_result() {
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let wrapper = py
                .eval(c"__import__('builtins').type('W', (), {'__init__': lambda s: setattr(s, 'calls', []), '__getattr__': lambda s, n: (lambda *a: s.calls.append((n, a)))})()", None, None)
                .unwrap()
                .unbind();
            client.__init__(wrapper.clone_ref(py)).unwrap();
            let shared = Arc::new(SharedState::new());
            shared.reference.push_scanner_data(3, crate::control::scanner::ScannerResult {
                con_ids: Vec::new(),
                entries: Vec::new(),
                scan_time: String::new(),
                error_text: "Scanner subscription not allowed".to_string(),
            });

            *client.shared.lock().unwrap() = Some(shared.clone());
            client.dispatch_once(py, &shared).expect("the pass ends");

            let calls = wrapper.getattr(py, "calls").unwrap();
            let list = calls.cast_bound::<pyo3::types::PyList>(py).unwrap();
            let names: Vec<String> = list.iter()
                .map(|c| c.get_item(0).unwrap().extract::<String>().unwrap())
                .collect();
            assert!(names.contains(&"error".to_string()),
                "the refusal is reported: {names:?}");
            assert!(!names.contains(&"scanner_data".to_string()),
                "and nothing is delivered as a row: {names:?}");
            let error_call = list.iter()
                .find(|c| c.get_item(0).unwrap().extract::<String>().unwrap() == "error")
                .unwrap();
            let args = error_call.get_item(1).unwrap();
            assert_eq!(args.get_item(0).unwrap().extract::<i64>().unwrap(), 3,
                "against the requesting id");
            assert_eq!(args.get_item(3).unwrap().extract::<String>().unwrap(),
                "Scanner subscription not allowed", "in the venue's own words");
        });
    }
}

#[cfg(test)]
mod eviction_tests {
    use super::*;

    /// A completion arms the cleanup of the record kept for an order, and the
    /// venue can restate the execution before the pass that runs it: a trade
    /// correction puts the order back in the book, and the row taken then is
    /// the live one — what reads the order next finds nothing and seeds an
    /// empty contract and order in its place. Only a finished order's row is
    /// reclaimed.
    #[test]
    fn a_reopened_order_keeps_its_record_when_the_armed_cleanup_runs() {
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let wrapper = py
                .eval(c"type('W', (), {'__getattr__': lambda s, n: (lambda *a: None)})()", None, None)
                .unwrap()
                .unbind();
            client.__init__(wrapper).unwrap();
            let shared = Arc::new(SharedState::new());
            *client.shared.lock().unwrap() = Some(shared.clone());
            let row = |order_id: i64, status: &str| crate::bridge::RichOrderInfo {
                contract: crate::types::model::Contract {
                    con_id: 756733, symbol: "SPY".into(), ..Default::default()
                },
                order: crate::types::model::Order { order_id, ..Default::default() },
                order_state: crate::types::model::OrderState {
                    status: status.into(), ..Default::default()
                },
                last_exec: Default::default(),
            };
            // One the venue has put back to working, and one that has finished.
            shared.orders.push_order_info(7, row(7, "Submitted"));
            shared.orders.push_order_info(8, row(8, "Filled"));
            client.deferred_evictions.lock().unwrap().extend([7, 8]);

            client.dispatch_once(py, &shared).expect("the pass ends");

            assert!(shared.orders.get_order_info(7).is_some(),
                "the working row outlives the cleanup its completion armed");
            assert!(shared.orders.get_order_info(8).is_none(),
                "and a finished one is still reclaimed");
            assert!(client.deferred_evictions.lock().unwrap().is_empty(),
                "both were swept");
        });
    }
}
