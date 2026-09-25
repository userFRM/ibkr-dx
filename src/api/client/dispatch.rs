//! Event dispatch: one read of the session, in the session's one order.
//!
//! A read takes the conflated state, then a cut of the session's stamp
//! counter, then every record stamped below the cut; it delivers the records
//! in stamp order and the state after them. A gateway writes everything to one
//! socket and a TWS client reads it in that one order: here the order is the
//! order the engine took each record in, whatever queue it waited in.

use std::sync::atomic::Ordering;
use crate::types::qty_to_f64;
use crate::types::model::{
    BarData, CommissionAndFeesReport, ContractDetails, ContractDescription, ErrorOrigin, Execution,
    Order as ApiOrder, OrderState, TickAttribLast, TickAttribBidAsk, PRICE_SCALE_F,
};
use crate::api::wrapper::Wrapper;
use crate::bridge::{Answer, FillRecord, Record, Reply, Take, UpdateRecord};
use crate::client_core::Polled;
use crate::types::order_status::order_status_str;
const QTY_SCALE_F: f64 = crate::types::QTY_SCALE as f64;

use crate::types::*;

use super::{Contract, EClient};

/// Tick type 53: a computation this client was asked for.
///
/// The stream and the answer are two different things, and the venue names
/// them apart: a caller watching a contract reads the model on 13, and a
/// caller who asked what a volatility implies reads their answer on 53. Sent
/// under 13, an answer arrived indistinguishable from the stream.
const ASKED_OPTION_COMPUTATION: i32 = 53;

/// What the reference client reports when a contract cannot be named.
const NO_SECURITY_DEFINITION: i64 = 200;

/// Reported against no request, the way the reference client reports anything
/// it cannot attribute to one.
pub(crate) const NO_REQUEST: i64 = -1;

/// The venue said something went wrong and stated no code for it. This one
/// says only that the venue is the one saying it.
const VENUE_REPORTED: i64 = 321;

/// What the venue sends every session when it has something to say that
/// belongs to no request. The number is the venue's own, not a stand-in: it
/// arrives on the session as a message of its own subtype and reaches every
/// client under this code.
const VENUE_MESSAGE: i64 = 2148;

/// What the reference client reports when data a caller asked for is not being
/// served. A book this client has given up on is not being served: the venue
/// goes on sending it and nothing further is kept, until the caller withdraws
/// it and asks again.
pub(crate) const DEPTH_NOT_SERVED: i64 = 354;

impl EClient {
    // ── Message Processing ──

    /// Deliver everything the session holds, in the order the engine took it
    /// in. Call this in a loop — it is the Rust equivalent of C++
    /// `EReader::processMsgs()`.
    ///
    /// One read takes the quotes and the figures this client compares with
    /// what the caller was last told, then every record the engine had pushed
    /// by then, and delivers the records in the order they were pushed and the
    /// quotes and figures after them. A record pushed during the read is left
    /// for the next one, so nothing arrives ahead of anything pushed before it.
    ///
    /// Reading takes the session's turn, because the queues empty as they are
    /// read. The calls that answer pump this loop themselves and keep what
    /// carries their own request id, so a second reader running beside one
    /// takes the answer first and hands it to a callback — and the question
    /// waits out its whole deadline for a reply that had already arrived. The
    /// turn is released before this returns, so a loop calling it holds
    /// nothing between reads.
    pub fn process_msgs(&self, wrapper: &mut impl Wrapper) {
        // The wake hook may be called again for whatever changes from here.
        self.shared.arm_wake();
        // A read from inside a read is served by the one it is inside. The
        // turn this thread holds is not re-entrant, so waiting for it here
        // ends the program; and what this would drain is the outer read's to
        // deliver, to the wrapper that read was given.
        if super::reading_now() == self.which_session() {
            return;
        }
        let _turn = self.asking.lock().unwrap_or_else(|e| e.into_inner());
        let bulletins = self.core.bulletins_subscribed();
        self.read_the_session(wrapper, Take::Dispatch { bulletins });
    }

    /// One read, for a caller that already holds the turn.
    ///
    /// The turn is not re-entrant, and a question that took it before sending
    /// pumps this loop while it waits. `take` says which records the read
    /// takes: everything not held by a call that answers, a whole read for a
    /// call that answers with a record kept, or a call's own records alone.
    pub(crate) fn read_the_session(&self, wrapper: &mut impl Wrapper, take: Take<'_>) {
        // The session's last record has been delivered, and nothing after it
        // is this session's to deliver.
        if self.ended.load(Ordering::Acquire) {
            return;
        }
        // Said for the length of the read, so a question asked from inside one
        // of the callbacks below is told why it cannot be answered rather than
        // left waiting on this thread's own turn.
        let _reading = super::Reading::begin(self.which_session());
        self.free_what_the_fills_held_back();
        // (a) The conflated state, before the cut: a change that followed a
        // record's push is then delivered no earlier than that record. A call
        // taking only its own records takes none of it.
        let mut polled = (!matches!(take, Take::Own(_))).then(|| self.poll_the_state());
        #[cfg(test)]
        crate::bridge::hooks::run(&crate::bridge::hooks::BEFORE_THE_CUT);
        // (b) and (c): the cut, and every record stamped below it.
        let cut = self.shared.next_seq();
        let records = self.shared.take_records(cut, take);
        // (d) In the order the engine took them in.
        for (_, record) in records {
            if let Record::Closed = record {
                // A barrier. Every writer has stopped by now, so the state
                // taken again is final, and delivered in place of what was
                // taken before the cut — a quote or a figure written between
                // the two is not lost behind the older value.
                let last = self.poll_the_state();
                let state = match polled.take() {
                    Some(earlier) => earlier.then(last),
                    None => last,
                };
                self.deliver_state(state, wrapper);
                self.deliver_the_close(wrapper);
                return;
            }
            self.deliver_record(record, wrapper, &mut polled);
        }
        // (e) The state taken in (a), under the mapping the records left.
        if let Some(state) = polled {
            self.deliver_state(state, wrapper);
        }
    }

    /// Records kept back because a fill for them was still queued, freed once
    /// that fill has been read. Freed here and nowhere else: this is the side
    /// that reads the fills, so a record cannot be freed between a fill being
    /// taken off the queue and the report that is built from it.
    fn free_what_the_fills_held_back(&self) {
        if !self.deferred_evictions.lock().unwrap().is_empty() {
            self.deferred_evictions.lock().unwrap().retain(|oid| {
                if self.shared.orders.has_pending_fill(*oid) {
                    return true;
                }
                self.shared.orders.remove_completed_order_info(*oid);
                false
            });
        }
    }

    /// Take the conflated state, as a read's first step.
    fn poll_the_state(&self) -> Polled {
        let multi = self.core.multi_watchers();
        let positions_watched = self.positions_requested.load(Ordering::Acquire)
            || !self.core.positions_watchers().is_empty();
        self.core.poll_conflated(&self.shared, positions_watched, &multi)
    }

    /// The summaries due under the numbers a call that answers holds, for a
    /// call reading only its own: a summary is kept per request and compared
    /// with what that request was last told, so it is the call's own.
    pub(crate) fn deliver_own_summaries(
        &self, wrapper: &mut impl Wrapper, own: &[crate::bridge::Owner],
    ) {
        for owner in own {
            if let crate::bridge::Owner::Request(req_id) = owner
                && let Some(batch) = self.core.prepare_account_summary_for(&self.shared, *req_id)
            {
                for entry in &batch.entries {
                    wrapper.account_summary(batch.req_id, &entry.account, &entry.tag, &entry.value, &entry.currency);
                }
                wrapper.account_summary_end(batch.req_id);
            }
        }
    }

    /// The session's last record: `connection_closed`, after everything
    /// before it and after the final state, then nothing more.
    ///
    /// What this side keeps about the session's requests is kept until here,
    /// so the final quotes, callbacks and holdings above reach the requests
    /// they belong to, and reset once they have.
    fn deliver_the_close(&self, wrapper: &mut impl Wrapper) {
        self.connected.store(false, Ordering::Release);
        self.ended.store(true, Ordering::Release);
        wrapper.connection_closed();
        self.core.reset();
    }

    // ── Records ──

    fn deliver_record(&self, record: Record, wrapper: &mut impl Wrapper, polled: &mut Option<Polled>) {
        match record {
            Record::Closed => {}
            // The connection going, said under 1100 unless it was asked for:
            // a stop's end is the session's last record, and the reference
            // client answers `disconnect()` with `connection_closed` alone.
            Record::ConnectionLost { by_design } => {
                self.connected.store(false, Ordering::Release);
                if !by_design {
                    wrapper.error_from(ErrorOrigin::Session, 1100, "Connectivity between client and server has been lost", "");
                }
            }
            // 1102 rather than 1101: the reconnect re-establishes the
            // subscriptions itself. Pushed only as the connection comes back
            // after a loss that was pushed, so it always follows its 1100.
            Record::ConnectionRestored => {
                self.connected.store(true, Ordering::Release);
                wrapper.error_from(ErrorOrigin::Session, 1102, "Connectivity between client and server has been restored - data maintained", "");
            }
            // One of the connections the venue keeps data on went away or came
            // back, said under the number the venue reports it under. A
            // market-data loss also forgets what each caller was last told of
            // every quote — the engine zeroes the quotes at the drop, and
            // diffed against what the caller had heard those noughts went out
            // as prices; diffed against nothing, only what the venue restates
            // goes out.
            Record::VenueData((which, up)) => {
                if matches!(which, crate::bridge::VenueDataConnection::MarketData) && !up {
                    self.core.forget_last_quotes();
                }
                let (broken, ok) = which.codes();
                wrapper.error_from(ErrorOrigin::Session, if up { ok } else { broken }, which.says(up), "");
            }
            Record::Retired(what) => {
                if matches!(what, crate::types::Retirement::Question(crate::types::model::Question::AccountUpdates))
                    && let Some(state) = polled {
                    state.account = None;
                    state.portfolio.clear();
                }
                self.retire(what, wrapper);
            },
            Record::OrderBook(entry) => self.core.keep_the_book(&self.shared, entry),
            Record::SlotTaken { slot, generation } => self.core.note_slot_taken(slot, generation),
            Record::SlotReleased { slot, generation } => self.core.note_slot_released(slot, generation),

            // What was said about an order that went anyway: a warning on its
            // number, and nothing else about it changes.
            Record::OrderNotice((order_id, code, msg, op)) => {
                wrapper.error_from(ErrorOrigin::Order { id: self.core.api_order_id(order_id), op }, code as i64, &msg, "");
            }
            Record::Fill(fill) => self.deliver_fill(fill, wrapper),
            Record::OrderUpdate(update) => self.deliver_update(update, wrapper),
            // What the venue says a fill cost, naming the execution it belongs
            // to. Pushed after the fill, so the fill is stored by now.
            Record::Charge(charge) => {
                self.core.record_charge(&charge);
                wrapper.commission_and_fees_report(&charge);
            }
            // An execution the venue restated rather than announced. Filed for
            // `req_executions` and reported to nobody: a caller that asks is
            // answered, and one that did not hears nothing.
            Record::RestatedExecution(restated) => {
                let (contract, execution) = *restated;
                self.core.push_execution(contract, execution, CommissionAndFeesReport::default());
            }
            // A replacement the venue has taken spends the terms kept against
            // a refusal of it, in its place: an acceptance and a stale refusal
            // leave the record on what the venue holds.
            Record::ReplacementTaken(order_id) => self.core.settle_replacement(order_id),
            Record::CancelReject(reject) => {
                let (code, msg) = self.core.retire_rejected(&reject);
                let origin = ErrorOrigin::Order { id: self.core.api_order_id(reject.order_id), op: reject.refuses() };
                wrapper.error_from(origin, code, &msg, "");
            }
            // Why an order stopped working. The status already said Inactive;
            // this says why.
            Record::OrderInactive((order_id, code, msg, op)) => {
                // A refusal is the end of a preview: it states what an order
                // would have cost, and nothing reached the book. Left
                // standing, the record read as a working order and its number
                // as spent.
                if self.core.tracked_order(order_id).is_some_and(|o| o.what_if) {
                    self.core.untrack_order(order_id);
                }
                wrapper.error_from(ErrorOrigin::Order { id: self.core.api_order_id(order_id), op }, code as i64, &msg, "");
            }
            // A preview and nothing else. The venue answers what an order
            // would cost on the order itself: it states no status for it,
            // nothing filled, no permanent number and no client. The reference
            // client's own decode of an open order calls one callback and this
            // is it.
            Record::WhatIf(wi) => {
                let state = OrderState::from(&wi);
                // Taken before the callback rather than after it. A preview is
                // finished the moment it is answered, and left recorded while
                // its own callback runs an order placed from inside that
                // callback under the same number read as a change to the
                // preview and was refused for being one.
                let tracked = self.core.open_orders.lock().unwrap().remove(&wi.order_id);
                let (contract, order) = tracked
                    .map(|t| (t.contract, t.order))
                    .unwrap_or_else(|| (Contract::default(), ApiOrder::default()));
                wrapper.open_order(self.core.api_order_id(wi.order_id), &contract, &order, &state);
            }

            // What a joined subscription was acknowledged with, to the request
            // that joined, as the reference client delivers it on
            // `tick_req_params` ahead of the first tick.
            Record::TickReqParamsFor((req_id, _)) => {
                if let Some(p) = self.core.watching(req_id)
                    .and_then(|instrument| self.shared.market.tick_req_params_for_follower(instrument))
                    && self.core.should_send_tick_req_params(req_id)
                {
                    wrapper.tick_req_params(req_id, p.min_tick, &p.bbo_exchange, p.snapshot_permissions);
                }
            }
            Record::SubscriptionFailureFor((req_id, reason)) => {
                    wrapper.error_from(ErrorOrigin::Request { id: req_id, ends: true }, NO_SECURITY_DEFINITION, &reason, "");
            }
            Record::TickReqParams((instrument, generation, _)) => {
                if generation == self.core.generation_held(instrument) {
                    for req_id in self.core.watchers_of(instrument) {
                        if let Some(p) = self.shared.market.tick_req_params_for_follower(instrument)
                            && self.core.should_send_tick_req_params(req_id)
                        {
                            wrapper.tick_req_params(req_id, p.min_tick, &p.bbo_exchange, p.snapshot_permissions);
                        }
                    }
                }
            }
            Record::TbtTrade(trade) => {
                // As the caller numbered it, from the record itself: a
                // contract can carry several streams and the contract alone
                // does not say which one this came from.
                let req_id = trade.req_id;
                // Tick type names the stream the request asked for: 1 = Last,
                // 2 = AllLast, as the record carries it from its stream.
                let kind = match trade.kind {
                    TbtType::AllLast => 2,
                    _ => 1,
                };
                // What the venue said about this print, not what a default says.
                let attrib_last = TickAttribLast {
                    past_limit: trade.past_limit,
                    unreported: trade.unreported,
                };
                wrapper.tick_by_tick_all_last(
                    req_id, kind, trade.timestamp as i64,
                    trade.price as f64 / PRICE_SCALE_F,
                    trade.size as f64 / QTY_SCALE_F,
                    &attrib_last, &trade.exchange, &trade.conditions,
                );
            }
            Record::TbtQuote(quote) => {
                let attrib_ba = TickAttribBidAsk {
                    bid_past_low: quote.bid_past_low,
                    ask_past_high: quote.ask_past_high,
                };
                wrapper.tick_by_tick_bid_ask(
                    quote.req_id, quote.timestamp as i64,
                    quote.bid as f64 / PRICE_SCALE_F, quote.ask as f64 / PRICE_SCALE_F,
                    quote.bid_size as f64 / QTY_SCALE_F,
                    quote.ask_size as f64 / QTY_SCALE_F,
                    &attrib_ba,
                );
            }
            // The point between the two, each time it moved.
            Record::TbtMid(mid) => {
                wrapper.tick_by_tick_mid_point(
                    mid.req_id, mid.timestamp as i64, mid.price as f64 / PRICE_SCALE_F,
                );
            }
            // A book this client could not keep whole, on the request that
            // asked for it. Nothing further is kept for it, so a caller not
            // told reads a subscription that is up and a book that has
            // stopped moving.
            Record::DepthDrop((req_id, reason)) => {
                let origin = ErrorOrigin::Request { id: i64::from(req_id), ends: true };
                wrapper.error_from(origin, DEPTH_NOT_SERVED, &reason, "");
            }
            Record::DepthUpdate(du) => {
                if du.market_maker.is_empty() {
                    wrapper.update_mkt_depth(du.req_id as i64, du.position, du.operation, du.side, du.price, du.size);
                } else {
                    wrapper.update_mkt_depth_l2(du.req_id as i64, du.position, &du.market_maker, du.operation, du.side, du.price, du.size, du.is_smart_depth);
                }
            }
            // News goes to every subscriber of the contract, as its quotes do
            // — of the contract that held the slot when it arrived.
            Record::TickNews((generation, news)) => {
                if generation == self.core.generation_held(news.instrument) {
                    for id in self.core.watchers_of(news.instrument) {
                        wrapper.tick_news(
                            id, news.timestamp as i64,
                            &news.provider_code, &news.article_id, &news.headline, "",
                        );
                    }
                }
            }
            // A computation answering a request, to that request alone.
            Record::OptionComputation(comp) => {
                if let Some(asked) = comp.answers {
                    wrapper.tick_option_computation(
                        asked, ASKED_OPTION_COMPUTATION, 0,
                        comp.implied_vol, comp.delta, comp.opt_price, comp.pv_dividend,
                        comp.gamma, comp.vega, comp.theta, comp.und_price,
                    );
                }
            }
            // An option computation to every request watching the option
            // that is owed it, with the figures it is owed.
            Record::OptionTick((generation, tick)) => {
                let (tick_type, to) = self.core.option_tick_owed(generation, &tick);
                for (req_id, figures) in to {
                    let [implied_vol, delta, opt_price, pv_dividend, gamma, vega, theta, und_price] =
                        figures;
                    wrapper.tick_option_computation(
                        req_id, tick_type, i32::from(tick.price_based),
                        implied_vol, delta, opt_price, pv_dividend, gamma, vega, theta, und_price,
                    );
                }
            }
            // What the venue said went wrong. It attributes these to no
            // request, so neither does this.
            Record::VenueError(text) => wrapper.error_from(ErrorOrigin::Session, VENUE_MESSAGE, &text, ""),
            // A lookup that named a contract another slot already holds. One
            // subscription per contract exists on the wire, so the callers
            // given the second slot read the first — otherwise their quotes
            // arrive on a slot nothing is watching.
            // A market-data request the engine has taken: from here its
            // number is served on the slot the record names.
            Record::MarketDataTaken(taken) => self.core.note_mkt_data_taken(&self.shared, &taken),
            // And withdrawn: nothing more is delivered under its number.
            Record::MarketDataWithdrawn(req_id) => self.core.unregister_mkt_data(req_id),
            // Everyone watching the contract, not only whoever asked first. A
            // refusal is a fact about the contract, and a caller sharing
            // somebody else's subscription holds no request of its own for the
            // venue to refuse. Where nobody holds it, there is nobody to tell.
            Record::MarketDataType((instrument, generation, data_type)) => {
                if generation == self.core.generation_held(instrument) {
                    self.core.note_mkt_data_type(instrument, data_type);
                }
            }
            Record::SubscriptionNotice((instrument, generation, notice)) => {
                if generation == self.core.generation_held(instrument) {
                    for req_id in self.core.watchers_of(instrument) {
                        let origin = ErrorOrigin::Request { id: req_id, ends: false };
                        wrapper.error_from(origin, i64::from(notice.code), &notice.message, "");
                    }
                }
            }
            Record::SubscriptionFailure((instrument, generation, reason)) => {
                if generation == self.core.generation_held(instrument) {
                    for req_id in self.core.watchers_of(instrument) {
                        let origin = ErrorOrigin::Request { id: req_id, ends: true };
                        wrapper.error_from(origin, NO_SECURITY_DEFINITION, &reason, "");
                    }
                }
            }
            // A request riding beside the quote that the venue refused. Told
            // to everyone watching the contract, under the code that means the
            // venue said something went wrong and stated none.
            Record::CompanionRefusal((instrument, generation, _kind, reason)) => {
                if generation == self.core.generation_held(instrument) {
                    for req_id in self.core.watchers_of(instrument) {
                        // The quote it rides beside goes on.
                        let origin = ErrorOrigin::Request { id: req_id, ends: false };
                        wrapper.error_from(origin, VENUE_REPORTED, &reason, "");
                    }
                }
            }
            // A news subscription the venue refused: the engine released its
            // side, so the client forgets whoever asked, leaving a later ask
            // free to send anew.
            // Taken only where they were asked for; left queued otherwise, for
            // a subscription asking for the day's.
            Record::NewsBulletin(b) => {
                wrapper.update_news_bulletin(b.msg_id as i64, b.msg_type, &b.message, &b.exchange);
            }
            // Real-time bars, and the continued half of a keep-up-to-date
            // request. The two arrive on one feed and are told apart by
            // whether the request has already answered with its history.
            Record::RealTimeBar((req_id, bar, session)) => {
                if self.core.hist_initial_complete.lock().unwrap().contains(&req_id) {
                    // A forming bar is stamped at its open, in seconds since
                    // the epoch; dated as the history before it was, in the
                    // caller's format on the zone the series was stated on.
                    let bd = BarData {
                        date: self.core.bar_time_for_epoch(
                            req_id as i64, i64::from(bar.timestamp), session,
                        ),
                        open: bar.open,
                        high: bar.high,
                        low: bar.low,
                        close: bar.close,
                        volume: bar.volume as i64,
                        wap: bar.wap,
                        bar_count: bar.count,
                        timezone: String::new(),
                        // A forming bar has not ended, and the stream states
                        // no end for one.
                        end: String::new(),
                    };
                    wrapper.historical_data_update(req_id as i64, &bd);
                } else {
                    wrapper.real_time_bar(
                        req_id as i64, bar.timestamp as i64,
                        bar.open, bar.high, bar.low, bar.close,
                        bar.volume, bar.wap, bar.count,
                    );
                }
            }

            // What the venue refused under a request, in its place: a book's
            // reset ahead of the levels that follow it, a query error ahead
            // of the empty end that follows it.
            Record::HistoricalError((origin, code, msg)) => {
                wrapper.error_from(origin, i64::from(code), &msg, "");
            }
            // Historical data → historical_data + historical_data_end, and
            // after that end, historical_data_update. A keep-up-to-date
            // request answers once with the history and then keeps speaking;
            // the reference client separates the two.
            Record::HistoricalData((req_id, response)) => {
                let is_update = self.core.hist_initial_complete.lock().unwrap().contains(&req_id);
                self.core.note_historical_zone(req_id as i64, &response.timezone);
                for bar in &response.bars {
                    let bd = BarData {
                        date: self.core.historical_bar_time_for(req_id as i64, bar, &response.timezone),
                        open: bar.open,
                        high: bar.high,
                        low: bar.low,
                        close: bar.close,
                        volume: bar.volume,
                        wap: bar.wap,
                        bar_count: bar.count,
                        timezone: response.timezone.clone(),
                        end: bar.end.clone(),
                    };
                    if is_update {
                        wrapper.historical_data_update(req_id as i64, &bd);
                    } else {
                        wrapper.historical_data(req_id as i64, &bd);
                    }
                }
                if response.is_complete && !is_update {
                    self.core.hist_initial_complete.lock().unwrap().insert(req_id);
                    // The range the request covered, which is what a caller
                    // pages backwards with.
                    let (from, to) =
                        self.core.historical_range_for(req_id as i64, &response.timezone);
                    wrapper.historical_data_end(req_id as i64, &from, &to);
                }
            }
            // An order this session did not place, paired with the number a
            // caller reaches it under here.
            Record::OrderBound((perm_id, client_id, order_id)) => {
                wrapper.order_bound(perm_id, client_id, order_id);
            }
            Record::HeadTimestamp((req_id, response)) => {
                // Returned in the form `format_date` asked for. The wire
                // carries one form; `bar_time_for` converts it.
                let stated = self.core.bar_time_for(req_id as i64, &response.head_timestamp, "");
                wrapper.head_timestamp(req_id as i64, &stated);
            }
            Record::ContractDetails((req_id, def)) => {
                let details = ContractDetails::from_definition(&def);
                // Fixed income answers on its own callback. A bond, a bill and
                // the type the venue spells `FIXED` share it; every other type
                // is answered on the ordinary one.
                if def.sec_type.is_fixed_income() {
                    wrapper.bond_contract_details(req_id as i64, &details);
                } else {
                    wrapper.contract_details(req_id as i64, &details);
                }
            }
            Record::ContractDetailsEnd(req_id) => wrapper.contract_details_end(req_id as i64),
            Record::DepthExchanges(depth_exchanges) => wrapper.mkt_depth_exchanges(&depth_exchanges),
            // The calendar's answers, as the venue wrote them.
            Record::CalendarMeta((req_id, json)) => wrapper.wsh_meta_data(req_id as i64, &json),
            Record::CalendarEvents((req_id, json)) => wrapper.wsh_event_data(req_id as i64, &json),
            Record::MatchingSymbols((req_id, matches)) => {
                let descriptions: Vec<ContractDescription> =
                    matches.iter().map(ContractDescription::from).collect();
                wrapper.symbol_samples(req_id as i64, &descriptions);
            }
            Record::OptionParams((req_id, underlying_con_id, scopes)) => {
                for scope in &scopes {
                    wrapper.security_definition_option_parameter(
                        req_id as i64, &scope.exchange, underlying_con_id, &scope.trading_class,
                        &scope.multiplier, &scope.expirations, &scope.strikes,
                    );
                }
                wrapper.security_definition_option_parameter_end(req_id as i64);
            }
            Record::ScannerParams(xml) => wrapper.scanner_parameters(&xml),
            // The advisor's own configuration: a partition the caller asked
            // for, the end of one they replaced, and the venue's account of a
            // replacement it would not take.
            Record::AdvisorConfig((fa_data_type, xml)) => wrapper.receive_fa(fa_data_type, &xml),
            Record::AdvisorReplaced((req_id, text)) => wrapper.replace_fa_end(req_id, &text),
            Record::AdvisorRefused((origin, code, text)) => {
                wrapper.error_from(origin, i64::from(code), &text, "");
            }
            // Scanner data. The rows' contracts were resolved by the engine
            // before the batch was released; the fallback covers a partial
            // flushed at its deadline.
            Record::ScannerData((req_id, result)) => {
                // A refused scan arrives in the shape of a completed one and
                // carries the reason, reported against the requesting id so a
                // refusal is not delivered as an empty result.
                if !result.error_text.is_empty() {
                    let origin = ErrorOrigin::Request { id: i64::from(req_id), ends: true };
                    wrapper.error_from(origin, VENUE_REPORTED, &result.error_text, "");
                }
                for (rank, entry) in result.entries.iter().enumerate() {
                    let mut contract = Contract { con_id: entry.con_id as i64, ..Default::default() };
                    if let Some(ac) = self.core.get_contract(entry.con_id as i64, &self.shared) {
                        contract.symbol = ac.symbol;
                        contract.sec_type = ac.sec_type;
                        contract.exchange = ac.exchange;
                        contract.currency = ac.currency;
                        contract.local_symbol = ac.local_symbol;
                        contract.primary_exchange = ac.primary_exchange;
                        contract.trading_class = ac.trading_class;
                    }
                    let details = ContractDetails { contract, ..Default::default() };
                    wrapper.scanner_data(req_id as i64, rank as i32, &details, "", "", "", "");
                }
                wrapper.scanner_data_end(req_id as i64);
            }
            Record::HistoricalNews((req_id, headlines, has_more)) => {
                for h in &headlines {
                    wrapper.historical_news(req_id as i64, &h.time, &h.provider_code, &h.article_id, &h.headline);
                }
                wrapper.historical_news_end(req_id as i64, has_more);
            }
            Record::NewsArticle((req_id, article_type, text)) => {
                wrapper.news_article(req_id as i64, article_type, &text);
            }
            Record::FundamentalData((req_id, data)) => wrapper.fundamental_data(req_id as i64, &data),
            Record::HistogramData((req_id, entries)) => {
                let items: Vec<(f64, i64)> = entries.iter().map(|e| (e.price, e.count)).collect();
                wrapper.histogram_data(req_id as i64, &items);
            }
            // Historical ticks route to the variant-specific callback, as in
            // ibapi.
            Record::HistoricalTicks((req_id, data, _, done)) => match &data {
                HistoricalTickData::Midpoint(_) => wrapper.historical_ticks(req_id as i64, &data, done),
                HistoricalTickData::Last(_) => wrapper.historical_ticks_last(req_id as i64, &data, done),
                HistoricalTickData::BidAsk(_) => wrapper.historical_ticks_bid_ask(req_id as i64, &data, done),
            },
            Record::HistoricalSchedule((req_id, schedule)) => {
                let sessions: Vec<(String, String, String)> = schedule.sessions.iter()
                    .map(|s| (s.ref_date.clone(), s.open_time.clone(), s.close_time.clone()))
                    .collect();
                wrapper.historical_schedule(
                    req_id as i64, &schedule.start_date_time, &schedule.end_date_time,
                    &schedule.timezone, &sessions,
                );
            }

            // A refusal made at a call, in its place: after everything pushed
            // before the call, as a gateway's rejection arrives after
            // everything it wrote before it.
            Record::Refused((origin, code, msg)) => wrapper.error_from(origin, code, &msg, ""),
            Record::Answer(answer) => self.deliver_answer(answer, wrapper, polled),
            Record::Reply(reply) => deliver_reply(reply, wrapper),
        }
    }

    /// A fill and the status stated on the same report: `order_status`, then
    /// `exec_details`, each with what the report stated.
    fn deliver_fill(&self, record: FillRecord, wrapper: &mut impl Wrapper) {
        let FillRecord { fill, report, status } = record;
        let price_f = fill.price as f64 / PRICE_SCALE_F;
        // Status as the report states it, derived from `remaining` only when
        // the report carries none.
        let status_str = status
            .as_ref()
            .map(|u| order_status_str(u.status))
            .unwrap_or(if fill.remaining == 0 { "Filled" } else { "Submitted" });
        self.core.learn_order_identity(&self.shared, fill.order_id);
        let (perm_id, parent_id) = self.core.perm_and_parent_stated(
            fill.order_id, report.as_deref(), status.as_ref(),
        );
        let client = self.core.client_stated(fill.order_id, report.as_deref());
        // After the record is read: a status stating the order filled drops
        // it, and read afterwards the fill that completed an order went out
        // as client zero's, without the parent this client recorded.
        if let Some(u) = &status {
            self.core.update_order_status(
                &self.shared, u.order_id, u.status, u.filled_qty, u.remaining_qty,
                u.instrument,
            );
        }
        // `filled` and `avgFillPrice` describe the order so far;
        // `lastFillPrice` describes this print.
        let avg_price_f = fill.avg_price as f64 / PRICE_SCALE_F;

        let side_str = match fill.side {
            Side::Buy => "BOT",
            Side::Sell => "SLD",
            Side::ShortSell => "SLD",
        };
        // The report this fill was booked off, not whatever the order's
        // record says now: one read can carry two prints of one order, and
        // the record holds only the later.
        let (c, exec) = if let Some(info) = report {
            let mut ex = info.last_exec.clone();
            ex.side = side_str.into();
            ex.shares = qty_to_f64(fill.qty);
            ex.price = price_f;
            ex.order_id = self.core.api_order_id(fill.order_id);
            // The client that placed it, where the report names none. The
            // record knows which client the order went out under and the
            // venue's report may carry no client at all; taken as stated, it
            // filed this session's own fill under client zero while the status
            // above was announced under the placing client.
            if ex.client_id == 0 {
                ex.client_id = i64::from(client);
            }
            let contract = if info.contract.con_id != 0 {
                self.core.get_contract(info.contract.con_id, &self.shared).unwrap_or_else(|| info.contract.clone())
            } else {
                info.contract.clone()
            };
            (contract, ex)
        } else {
            (Contract::default(), Execution {
                side: side_str.into(),
                shares: qty_to_f64(fill.qty),
                price: price_f,
                order_id: self.core.api_order_id(fill.order_id),
                // The venue stated nothing about this one, so the client that
                // placed the order is what names it. Left at nought, a request
                // filtered by client matched none of them.
                client_id: i64::from(client),
                ..Default::default()
            })
        };
        // Stored before either callback about the print. A caller that asks
        // for its executions from inside the status callback or this one --
        // which is ordinary -- was answered without the fill it was being told
        // about. What it cost arrives on a record of its own; stored unstated
        // so a replay of this execution says the charge is unknown rather
        // than that it was nothing.
        self.core.push_execution(c.clone(), exec.clone(), CommissionAndFeesReport::default());
        wrapper.order_status(
            self.core.api_order_id(fill.order_id), status_str, qty_to_f64(fill.cum_qty), qty_to_f64(fill.remaining),
            avg_price_f, perm_id, parent_id, price_f, i64::from(client), "", 0.0,
        );
        // Unsolicited executions carry request id -1. A market-data
        // subscription id does not identify a `reqExecutions` request.
        wrapper.exec_details(NO_REQUEST, &c, &exec);
        self.core.update_order_fill(
            fill.order_id, status_str, qty_to_f64(fill.cum_qty), qty_to_f64(fill.remaining),
        );
    }

    /// A status change with no fill on the same report: `open_order` with the
    /// order's state as the report stated it, then `order_status`.
    fn deliver_update(&self, record: UpdateRecord, wrapper: &mut impl Wrapper) {
        let UpdateRecord { update, state, client_id } = record;
        self.core.learn_order_identity(&self.shared, update.order_id);
        let status = order_status_str(update.status);
        // The engine reads no parent from the report, but this client placed
        // the order and was told. Prefer what it recorded; an order it did not
        // place keeps the engine's answer of none.
        let parent_id = self.core.tracked_parent_id(update.order_id)
            .unwrap_or(update.parent_id);
        // What the order has paid, as the report that changed its status
        // stated it.
        let avg = update.avg_price as f64 / crate::types::PRICE_SCALE as f64;
        // The order as this client sent it, beside the status it is now in.
        // The reference client answers an order's every change with both, from
        // the order it holds. Copied out before the callback rather than read
        // across it: a wrapper that reaches the order cache from the callback
        // would wait on a lock its own caller holds.
        let tracked = self.core.open_orders.lock().unwrap().get(&update.order_id).cloned();
        let client = tracked
            .as_ref()
            .map(|t| t.order.client_id)
            .filter(|c| *c != 0)
            .unwrap_or(client_id);
        // Not for a preview: the answer to a preview is the margin the venue
        // states on its own reply, and a pair sent from the status alone
        // answered it with no margin figures.
        if let Some(tracked) = tracked.filter(|t| !t.order.what_if) {
            // What the venue said about the order, under the status this
            // client names it by — as the report that changed the status
            // stated it, not as the order's record stands at the read.
            let state = OrderState {
                status: status.to_string(),
                ..state.map(|stated| stated.order_state.clone()).unwrap_or_default()
            };
            wrapper.open_order(
                self.core.api_order_id(update.order_id), &tracked.contract, &tracked.order, &state,
            );
        }
        wrapper.order_status(
            self.core.api_order_id(update.order_id), status, update.filled_qty,
            update.remaining_qty, avg, update.perm_id, parent_id, 0.0,
            i64::from(client), "", 0.0,
        );
        self.core.update_order_status(
            &self.shared, update.order_id, update.status, update.filled_qty,
            update.remaining_qty, update.instrument,
        );
    }

    // ── Answers composed where they stand ──

    /// An answer this side composes, as its state stands at the marker's
    /// place in the order.
    fn deliver_answer(&self, answer: Answer, wrapper: &mut impl Wrapper, polled: &mut Option<Polled>) {
        match answer {
            Answer::Positions => {
                // What moved before this answer is in it: the holdings are
                // stated as they are now. Taken, and handed to the per-request
                // watchers alone, so `position` does not say them twice.
                let mut already_stated = self.shared.portfolio.drain_position_changes();
                if let Some(state) = polled.as_mut() {
                    already_stated.append(&mut state.positions);
                }
                // Watching from here, where the answer stands.
                self.positions_requested.store(true, Ordering::Release);
                for pi in &self.shared.portfolio.position_infos() {
                    let c = self.position_contract(pi);
                    let avg_cost = pi.avg_cost as f64 / PRICE_SCALE_F;
                    wrapper.position(&self.account_id, &c, pi.position, avg_cost);
                }
                wrapper.position_end();
                let watching = self.multi_position_watchers();
                for pi in &already_stated {
                    let c = self.position_contract(pi);
                    let avg_cost = pi.avg_cost as f64 / PRICE_SCALE_F;
                    for req_id in &watching {
                        if self.core.positions_account(&self.shared, *req_id) != self.account_id { continue; }
                        wrapper.position_multi(*req_id, &self.account_id, &self.core.positions_model(*req_id), &c, pi.position, avg_cost);
                    }
                }
            }
            Answer::PositionsMulti { req_id, account, model_code } => {
                let account = self.shared.account_name(&account);
                let portfolio = self.shared.portfolio_for(&account);
                let mut moves = std::collections::BTreeMap::new();
                if let Some(state) = polled.as_mut() {
                    if account == self.account_id {
                        moves.extend(state.positions.drain(..).map(|p| (p.con_id, p)));
                    } else {
                        state.named_positions.retain(|(a, p)| {
                            if a != &account { return true; }
                            moves.insert(p.con_id, p.clone());
                            false
                        });
                    }
                }
                moves.extend(portfolio.drain_position_changes().into_iter().map(|p| (p.con_id, p)));
                for pi in moves.values() {
                    let contract = self.position_contract(pi);
                    let cost = pi.avg_cost as f64 / PRICE_SCALE_F;
                    if account == self.account_id && self.positions_requested.load(Ordering::Acquire) {
                        wrapper.position(&account, &contract, pi.position, cost);
                    }
                    for old in self.multi_position_watchers() {
                        if old != req_id && self.core.positions_account(&self.shared, old) == account {
                            wrapper.position_multi(old, &account, &self.core.positions_model(old), &contract, pi.position, cost);
                        }
                    }
                }
                self.core.select_positions_account(req_id, &account, &model_code);
                // Labelled with the account they are on, not with the one
                // that was asked about.
                for pi in portfolio.position_infos().into_iter().filter(|pi| pi.position != 0.0) {
                    let contract = self.position_contract(&pi);
                    wrapper.position_multi(
                        req_id, &account, &model_code, &contract, pi.position,
                        pi.avg_cost as f64 / PRICE_SCALE_F,
                    );
                }
                wrapper.position_multi_end(req_id);
            }
            Answer::AccountUpdatesMulti { req_id, account, model_code, ledger_and_nlv } => {
                let account = self.shared.account_name(&account);
                self.core.select_multi_account(req_id, &account, &model_code);
                // Held open from here. A figure that moves after the batch
                // below is reported again under the same number, and what the
                // request asked for is stated before that.
                self.core.ledger_only_for(req_id, ledger_and_nlv);
                // Against this request's own record, which a fresh ask starts
                // empty — so the batch is the account whole, and every batch
                // after it is what has moved since this request last heard.
                self.core.forget_account_figures_for(req_id);
                if let Some(state) = polled.as_mut() {
                    state.multi.retain(|(id, _)| *id != req_id);
                }
                for field in self.core.account_figures_that_moved(&self.shared, req_id) {
                    wrapper.account_update_multi(
                        req_id, &account, &model_code,
                        &field.key, &field.value, &field.currency,
                    );
                }
                wrapper.account_update_multi_end(req_id);
            }
            Answer::AccountUpdates { account } => {
                self.core.select_account_updates(&account);
                // Subscribed from here, where the answer stands: the account
                // as it is stated now, and its end where it is stated whole.
                self.core.subscribe_account_updates(true);
                if let Some(state) = polled.as_mut() {
                    state.account = None;
                    state.portfolio.clear();
                }
                if let Some(batch) = self.core.prepare_account_updates(&self.shared) {
                    let portfolio = self.core.prepare_portfolio_updates(&self.shared);
                    self.deliver_account_batch(batch, portfolio, wrapper);
                }
            }
            Answer::OpenOrders => {
                for (order_id, tracked) in self.core.collect_open_orders(&self.shared) {
                    let state = OrderState { status: tracked.status, ..Default::default() };
                    wrapper.open_order(self.core.api_order_id(order_id), &tracked.contract, &tracked.order, &state);
                }
                wrapper.open_order_end();
            }
            Answer::CompletedOrders { api_only } => {
                self.archive_completed_orders();
                // Copied before anything is called back: a callback may ask
                // for these again, and the lock is not re-entrant.
                let completed = self.completed.lock().unwrap().clone();
                for (contract, order, state, _) in &completed {
                    // Kept whole in the archive and filtered on the way out,
                    // so the same session can ask for all of them and for the
                    // numbered ones and be answered correctly either way.
                    if api_only && !self.shared.orders.was_entered_through_an_api(
                        order.order_id.max(0) as u64, order.perm_id.max(0) as u64,
                    ) {
                        continue;
                    }
                    wrapper.completed_order(contract, order, state);
                }
                wrapper.completed_orders_end();
            }
            Answer::NextValidId => wrapper.next_valid_id(self.stated_next_id() as i64),
            Answer::Executions { req_id, filter } => {
                let answer = self.core.executions_for_request(
                    &self.shared, &self.accounts, &filter, jiff::Timestamp::now(),
                );
                let (rows, unheld) = match answer {
                    Ok(answer) => answer,
                    Err(why) => {
                        let origin = ErrorOrigin::Request { id: req_id, ends: true };
                        return wrapper.error_from(origin, i64::from(why.code), &why.message, "");
                    }
                };
                if let Some(why) = crate::client_core::ClientCore::unheld_days_notice(&unheld) {
                    log::warn!("{why}");
                    // A notice the executions and their end follow.
                    let origin = ErrorOrigin::Request { id: req_id, ends: false };
                    wrapper.error_from(origin, crate::error_codes::Refusal::VALIDATION as i64, &why, "");
                }
                for se in rows {
                    wrapper.exec_details(req_id, &se.contract, &se.execution);
                    // Only where the venue has said what it cost. An execution
                    // is stored with its charge deliberately unstated, and an
                    // empty name can only mean nobody has said.
                    if !se.commission_and_fees.exec_id.is_empty() {
                        wrapper.commission_and_fees_report(&se.commission_and_fees);
                    }
                }
                wrapper.exec_details_end(req_id);
            }
        }
    }

    /// A withdrawal the engine confirmed, where it stands: what the
    /// withdrawn exchange watched stops here, after everything it answered.
    fn retire(&self, what: crate::types::Retirement, wrapper: &mut impl Wrapper) {
        use crate::types::Retirement;
        match what {
            Retirement::Question(q) => {
                match q {
                    crate::types::model::Question::Positions => {
                        self.positions_requested.store(false, Ordering::Release);
                    }
                    crate::types::model::Question::AccountUpdates => {
                        self.core.subscribe_account_updates(false);
                    }
                    _ => {}
                }
                wrapper.question_retired(q);
            }
            Retirement::PositionsMulti(req_id) => {
                self.core.forget_positions_account(req_id);
            }
            Retirement::AccountUpdatesMulti(req_id) => {
                self.core.forget_account_figures_for(req_id);
                self.core.forget_multi_account(req_id);
                self.core.ledger_only_for(req_id, false);
            }
        }
    }

    /// The per-request position watchers, in order.
    fn multi_position_watchers(&self) -> Vec<i64> {
        self.core.positions_watchers()
    }

    /// One batch of the account's own figures, its holdings beside them, and
    /// the end where the account has just been stated whole.
    fn deliver_account_batch(
        &self,
        batch: crate::client_core::AccountUpdateBatch,
        portfolio: Vec<crate::client_core::PortfolioUpdateEntry>,
        wrapper: &mut impl Wrapper,
    ) {
        let account = self.core.updates_account(&self.shared);
        for field in &batch.fields {
            wrapper.update_account_value(&field.key, &field.value, &field.currency, &account);
        }
        // What the account holds, beside what it is worth. The reference
        // client reports both on this subscription.
        for entry in portfolio {
            let contract = self.core
                .get_contract(entry.con_id, &self.shared)
                .unwrap_or_else(|| crate::types::model::Contract {
                    con_id: entry.con_id,
                    ..Default::default()
                });
            wrapper.update_portfolio(
                &contract, entry.position, entry.market_price, entry.market_value,
                entry.avg_cost, entry.unrealized_pnl, entry.realized_pnl, &account,
            );
        }
        if batch.finished {
            wrapper.update_account_time("");
            wrapper.account_download_end(&account);
        }
    }

    // ── State, after the records ──

    /// The state a read took before its cut, under the mapping its records
    /// left: holdings, quotes, the maps of venues answered, and the figures.
    fn deliver_state(&self, polled: Polled, wrapper: &mut impl Wrapper) {
        let Polled {
            quotes, positions, named_positions, pnl, pnl_single, account, portfolio, multi, summaries,
            smart_components,
        } = polled;

        // A holding that moved since the caller last heard, to whoever still
        // watches. Drained once and given to everyone watching.
        if !positions.is_empty() {
            let on_position = self.positions_requested.load(Ordering::Acquire);
            let per_request = self.multi_position_watchers();
            for pi in &positions {
                let contract = self.position_contract(pi);
                let avg_cost = pi.avg_cost as f64 / PRICE_SCALE_F;
                if on_position {
                    wrapper.position(&self.account_id, &contract, pi.position, avg_cost);
                }
                // The account this session opened under, whatever the request
                // named, as the answer to the request itself states.
                for req_id in &per_request {
                    if self.core.positions_account(&self.shared, *req_id) != self.account_id { continue; }
                    wrapper.position_multi(*req_id, &self.account_id, &self.core.positions_model(*req_id), &contract, pi.position, avg_cost);
                }
            }
        }

        for (account, pi) in named_positions {
            let contract = self.position_contract(&pi);
            for req_id in self.multi_position_watchers() {
                if self.core.positions_account(&self.shared, req_id) == account {
                    wrapper.position_multi(req_id, &account, &self.core.positions_model(req_id), &contract, pi.position, pi.avg_cost as f64 / PRICE_SCALE_F);
                }
            }
        }

        self.deliver_quotes(quotes, wrapper);

        // Maps of venues asked for before they arrived: answered once they
        // have, or refused once the wait a gateway allows has run out.
        for (req_id, answer) in smart_components {
            match answer {
                Ok(components) => wrapper.smart_components(req_id, &components),
                Err(why) => {
                    let origin = ErrorOrigin::Request { id: req_id, ends: true };
                    wrapper.error_from(origin, i64::from(why.code), &why.message, "");
                }
            }
        }

        // PnL → pnl callback (change-detected via ClientCore)
        for update in pnl {
            wrapper.pnl(update.req_id, update.daily_pnl, update.unrealized_pnl, update.realized_pnl);
        }
        for update in pnl_single {
            wrapper.pnl_single(update.req_id, update.pos, update.daily_pnl, update.unrealized_pnl, update.realized_pnl, update.value);
        }

        // The account's own figures, where it is subscribed.
        if let Some(batch) = account {
            self.deliver_account_batch(batch, portfolio, wrapper);
        }

        // The multi-account subscription, which is a live feed of its own:
        // every figure that has moved since it last heard, under each request
        // still watching.
        let watching = self.core.multi_watchers();
        for (req_id, fields) in multi {
            if !watching.contains(&req_id) {
                continue;
            }
            for field in fields {
                wrapper.account_update_multi(
                    req_id, &self.core.multi_account(&self.shared, req_id), &self.core.multi_model(req_id),
                    &field.key, &field.value, &field.currency,
                );
            }
        }

        // Account summary → account_summary + account_summary_end.
        for batch in summaries {
            for entry in &batch.entries {
                wrapper.account_summary(batch.req_id, &entry.account, &entry.tag, &entry.value, &entry.currency);
            }
            wrapper.account_summary_end(batch.req_id);
        }
    }

    /// Each held slot's quote, as ticks against what its callers were last
    /// told, to the requests watching it now.
    fn deliver_quotes(&self, quotes: Vec<crate::client_core::PolledQuote>, wrapper: &mut impl Wrapper) {
        let instruments = self.core.snapshot_instruments();
        let mut snapshot_done: Vec<(i64, Option<u64>)> = Vec::new();
        for polled in quotes {
            let iid = polled.iid;
            let Some((_, req_id, watchers)) = instruments.iter().find(|(at, ..)| *at == iid).cloned() else {
                continue;
            };
            // Only under the occupancy the slot is held under now. A quote of
            // the contract that left the slot is not this one's, and what the
            // caller was last told stays where it was, so the next read states
            // the new contract's quote whole.
            if polled.generation != self.core.generation_held(iid) {
                self.close_finished_snapshots(req_id, &watchers, wrapper, &mut snapshot_done);
                continue;
            }
            let result = self.core.ticks_from(&self.shared, polled, req_id);
            // Ahead of everything this read delivers, and to everyone it
            // delivers to. The type a caller is served under is stated before
            // the data it applies to, which is the order the reference client
            // keeps.
            let delivering = result.delivered
                || !result.generic_ticks.is_empty()
                || !result.string_ticks.is_empty()
                || !result.snapshot_ticks.is_empty()
                || result.timestamp.is_some();
            for id in std::iter::once(req_id).chain(watchers.iter().copied()) {
                if let Some(mdt) = self.core.check_mdt_needed(id, delivering) {
                    wrapper.market_data_type(id, mdt);
                }
            }
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
                        let attrib = crate::types::quote_attributes(
                            tick.tick_type, result.eligible_mask, result.quote_state_mask,
                        );
                        wrapper.tick_price(id, tick.tick_type, tick.value, &attrib);
                    } else {
                        wrapper.tick_size(id, tick.tick_type, tick.value);
                    }
                }
            }
            for tick in &result.generic_ticks {
                for id in std::iter::once(tick.req_id).chain(watchers.iter().copied()) {
                    wrapper.tick_generic(id, tick.tick_type, tick.value);
                }
            }
            for st in &result.string_ticks {
                for id in std::iter::once(st.req_id).chain(watchers.iter().copied()) {
                    wrapper.tick_string(id, st.tick_type, &st.value);
                }
            }
            if let Some(ts) = &result.timestamp {
                let ts_secs = ts.timestamp_ns / 1_000_000_000;
                let tick_type = if result.delayed { 88 } else { 45 };
                // Tick type 45 goes to every subscriber of the contract, as
                // the prices and strings above do. A delayed feed's time, 88,
                // is one of the kinds its snapshot waits for.
                for id in std::iter::once(ts.req_id).chain(watchers.iter().copied()) {
                    self.core.note_snapshot_tick(id, tick_type);
                    wrapper.tick_string(id, tick_type, &ts_secs.to_string());
                }
            }
            // The answer to a chargeable snapshot, to the snapshot's own
            // requests alone.
            for tick in &result.snapshot_ticks {
                if tick.is_price {
                    let attrib = crate::types::quote_attributes(
                        tick.tick_type, result.eligible_mask, result.quote_state_mask,
                    );
                    wrapper.tick_price(tick.req_id, tick.tick_type, tick.value, &attrib);
                } else {
                    wrapper.tick_size(tick.req_id, tick.tick_type, tick.value);
                }
            }
            for st in &result.snapshot_strings {
                wrapper.tick_string(st.req_id, st.tick_type, &st.value);
            }
            self.close_finished_snapshots(req_id, &watchers, wrapper, &mut snapshot_done);
        }
        // A snapshot on a slot this read did not poll still ends at its
        // bound: the eleven seconds run from the request, not from a tick.
        for (_, req_id, watchers) in &instruments {
            self.close_finished_snapshots(*req_id, watchers, wrapper, &mut snapshot_done);
        }
        // Withdrawn by this client: a command, not a delivery. Only where the
        // number still holds the subscription the snapshot was for — not
        // merely the same contract, which a callback that re-asked would also
        // show.
        for (req_id, was_watching) in snapshot_done {
            if self.core.registration_of(req_id) == was_watching {
                let _ = self.try_cancel_mkt_data(req_id);
            }
        }
    }

    /// `tick_snapshot_end` for the holder and everyone watching whose
    /// snapshot is whole, or whose bound has run out. A caller that asked for
    /// a snapshot of a contract somebody was already watching is recorded as
    /// a follower, and naming only the holder left its snapshot never ended.
    fn close_finished_snapshots(
        &self, req_id: i64, watchers: &[i64], wrapper: &mut impl Wrapper,
        done: &mut Vec<(i64, Option<u64>)>,
    ) {
        for id in std::iter::once(req_id).chain(watchers.iter().copied()) {
            if self.core.check_snapshot_done(id) {
                // What it was watching when the snapshot finished, so the
                // withdrawal can tell this subscription from whatever the
                // callback leaves under the same number.
                let was_watching = self.core.registration_of(id);
                wrapper.tick_snapshot_end(id);
                done.push((id, was_watching));
            }
        }
    }
}

/// An answer given at the call, delivered in its place.
fn deliver_reply(reply: Reply, wrapper: &mut impl Wrapper) {
    match reply {
        Reply::NewsProviders(providers) => wrapper.news_providers(&providers),
        Reply::CurrentTime(t) => wrapper.current_time(t),
        Reply::CurrentTimeInMillis(t) => wrapper.current_time_in_millis(t),
        Reply::SoftDollarTiers(req_id, tiers) => wrapper.soft_dollar_tiers(req_id, &tiers),
        Reply::FamilyCodes(codes) => wrapper.family_codes(&codes),
        Reply::UserInfo(req_id, id) => wrapper.user_info(req_id, &id),
        Reply::ManagedAccounts(list) => wrapper.managed_accounts(&list),
        Reply::MarketRule(rule, increments) => wrapper.market_rule(i64::from(rule), &increments),
        Reply::SmartComponents(req_id, components) => wrapper.smart_components(req_id, &components),
        Reply::DisplayGroupList(req_id, groups) => wrapper.display_group_list(req_id, &groups),
        Reply::DisplayGroupUpdated(req_id, info) => wrapper.display_group_updated(req_id, &info),
    }
}

#[cfg(test)]
mod fixed_income_tests {
    use crate::api::wrapper::Wrapper;
    use crate::control::contracts::{ContractDefinition, SecurityType};
    use crate::types::model::ContractDetails;

    /// A lookup for fixed income is answered on its own callback.
    ///
    /// A bond, a bill and the type the venue spells `FIXED` share it, and the
    /// venue answers them with a different set of fields from every other
    /// type's. Answered on the ordinary callback, a program written against
    /// the reference client waited through its own answer.
    #[test]
    fn fixed_income_answers_on_the_callback_written_for_it() {
        #[derive(Default)]
        struct Heard { plain: Vec<String>, fixed: Vec<String> }
        impl Wrapper for Heard {
            fn contract_details(&mut self, _: i64, d: &ContractDetails) {
                self.plain.push(d.contract.symbol.clone());
            }
            fn bond_contract_details(&mut self, _: i64, d: &ContractDetails) {
                self.fixed.push(d.contract.symbol.clone());
            }
        }
        let (client, _rx, shared) = crate::api::client::tests::test_client();
        for (symbol, sec_type) in [
            ("T 4 05/15/30", SecurityType::Bond),
            ("B 0 08/01/26", SecurityType::Bill),
            ("F 3 01/01/28", SecurityType::FixedIncome),
            ("SPY", SecurityType::Stock),
        ] {
            shared.reference.push_contract_details(1, ContractDefinition {
                symbol: symbol.to_string(), sec_type, ..Default::default()
            });
        }
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        assert_eq!(heard.fixed, ["T 4 05/15/30", "B 0 08/01/26", "F 3 01/01/28"]);
        assert_eq!(heard.plain, ["SPY"], "and nothing else moves");
    }
}

#[cfg(test)]
mod delivered_size_tests {
    use crate::api::wrapper::Wrapper;
    use crate::types::model::{TickAttribBidAsk, TickAttribLast};
    use crate::types::{PRICE_SCALE, QTY_SCALE, TbtQuote, TbtTrade};

    /// A snapshot of an option ends, as a gateway ends one, only once the
    /// venue's model has been delivered as well as the five kinds; and one on
    /// a delayed feed only once the last trade's time, 88, has been. Both
    /// reach the snapshot's check from where they are delivered: the model as
    /// its record, the time as a string tick.
    #[test]
    fn a_snapshot_waits_for_the_model_on_an_option_and_the_time_on_a_delayed_feed() {
        #[derive(Default)]
        struct Heard { ended: Vec<i64>, times: Vec<(i64, i32)> }
        impl Wrapper for Heard {
            fn tick_snapshot_end(&mut self, req_id: i64) {
                self.ended.push(req_id);
            }
            fn tick_string(&mut self, req_id: i64, tick_type: i32, _: &str) {
                if matches!(tick_type, 45 | 88) {
                    self.times.push((req_id, tick_type));
                }
            }
        }
        let five = crate::types::Quote {
            bid: 100 * PRICE_SCALE, ask: 101 * PRICE_SCALE, last: 100 * PRICE_SCALE,
            open: 99 * PRICE_SCALE, close: 98 * PRICE_SCALE,
            ..Default::default()
        };

        let (client, rx, shared) = crate::api::client::tests::test_client();
        let option = crate::api::client::Contract {
            con_id: 700_001, symbol: "SPY".into(), sec_type: "OPT".into(),
            exchange: "SMART".into(), currency: "USD".into(),
            last_trade_date_or_contract_month: "20261218".into(), strike: 500.0,
            right: "C".into(), multiplier: "100".into(),
            ..Default::default()
        };
        client.try_req_mkt_data(1, &option, "", true, false).expect("taken");
        crate::api::client::tests::settled(&client, &rx);
        let slot = client.core.watching(1).expect("the engine took it");
        shared.market.push_quote(slot, &five);
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        assert!(heard.ended.is_empty(), "an option's snapshot ended without its model");
        shared.market.push_option_tick(crate::bridge::OptionTick {
            instrument: slot, kind: crate::bridge::OptionTickKind::Model, figures: [0.2; 8], price_based: false,
        });
        client.process_msgs(&mut heard);
        assert_eq!(heard.ended, [1], "the model was the last of it");

        let (client, rx, shared) = crate::api::client::tests::test_client();
        client.try_req_mkt_data(2, &crate::api::client::tests::spy(), "", true, false)
            .expect("taken");
        crate::api::client::tests::settled(&client, &rx);
        let slot = client.core.watching(2).expect("the engine took it");
        client.core.mark_feed_delayed_for_test(slot);
        shared.market.push_quote(slot, &five);
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        assert!(heard.ended.is_empty(), "a delayed snapshot ended without the time");
        shared.market.push_quote(slot, &crate::types::Quote { timestamp_ns: 1_700_000_000_000_000_000, ..five });
        client.process_msgs(&mut heard);
        assert_eq!(heard.times, [(2, 88)], "the time on a delayed feed is 88");
        assert_eq!(heard.ended, [2], "and it was the last of it");
    }

    /// News and model publications belong to whoever still watches the
    /// contract. A model tick goes to each watcher as it stands, and to a
    /// watcher only when it differs from the last that watcher was sent; a
    /// bid's, ask's or last's is completed from the last of its kind that
    /// watcher was sent. An explicit calculation answer keeps its own request
    /// id.
    #[test]
    fn news_and_models_are_delivered_only_to_current_watchers() {
        type Model = (i64, i32, i32, [f64; 8]);
        #[derive(Default)]
        struct Heard { news: Vec<i64>, models: Vec<Model> }
        impl Wrapper for Heard {
            fn tick_news(&mut self, id: i64, _: i64, _: &str, _: &str, _: &str, _: &str) {
                self.news.push(id);
            }
            fn tick_option_computation(
                &mut self, id: i64, kind: i32, attrib: i32, iv: f64, delta: f64, price: f64,
                pv_dividend: f64, gamma: f64, vega: f64, theta: f64, und: f64,
            ) {
                self.models.push((id, kind, attrib, [iv, delta, price, pv_dividend, gamma, vega, theta, und]));
            }
        }
        let (client, rx, shared) = crate::api::client::tests::test_client();
        for req_id in [1, 2] {
            client.try_req_mkt_data(req_id, &crate::api::client::tests::spy(), "", false, false)
                .expect("taken");
        }
        crate::api::client::tests::settled(&client, &rx);
        let slot = client.core.watching(1).expect("the engine took it");
        let figures = |iv: f64| [iv, 0.55, 5.0, f64::MAX, 0.02, 0.3, -0.1, 765.0];
        let publish = |iv: f64| {
            shared.market.push_tick_news(crate::types::TickNews {
                instrument: slot, timestamp: 0, provider_code: "BRFG".into(),
                article_id: "BRFG$1".into(), headline: "SPY headline".into(),
            });
            shared.market.push_option_tick(crate::bridge::OptionTick {
                instrument: slot, kind: crate::bridge::OptionTickKind::Model, figures: figures(iv), price_based: true,
            });
        };
        publish(0.2);
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        assert_eq!(heard.news, [1, 2]);
        assert_eq!(heard.models, [(1, 13, 1, figures(0.2)), (2, 13, 1, figures(0.2))]);

        // The same tick again goes only to a request that was not sent it.
        client.try_req_mkt_data(3, &crate::api::client::tests::spy(), "", false, false)
            .expect("taken");
        crate::api::client::tests::settled(&client, &rx);
        publish(0.2);
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        assert_eq!(heard.models, [(3, 13, 1, figures(0.2))], "sent once to each");

        // The bid's, the ask's and the last's go out under numbers of their
        // own, and take each figure they do not state from the last one of
        // their kind a request was sent: sent where that differs from it, and
        // not where it does not.
        use crate::bridge::OptionTickKind::{Ask, Bid, Last};
        let side = |kind, figures| {
            shared.market.push_option_tick(crate::bridge::OptionTick {
                instrument: slot, kind, figures, price_based: false,
            });
        };
        let unstated = f64::MAX;
        let bid = [0.1, 0.5, 4.74, 1.8, 0.04, 0.39, -0.44, 769.45];
        let ask = [0.11, unstated, 4.77, unstated, unstated, unstated, unstated, 769.45];
        side(Bid, bid);
        side(Ask, ask);
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        assert_eq!(
            heard.models,
            [(1, 10, 0, bid), (2, 10, 0, bid), (3, 10, 0, bid), (1, 11, 0, ask), (2, 11, 0, ask), (3, 11, 0, ask)],
        );
        let priced_again = [unstated, unstated, 4.75, unstated, unstated, unstated, unstated, unstated];
        side(Bid, priced_again);
        side(Bid, priced_again);
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        let merged = [0.1, 0.5, 4.75, 1.8, 0.04, 0.39, -0.44, 769.45];
        assert_eq!(
            heard.models,
            [(1, 10, 0, merged), (2, 10, 0, merged), (3, 10, 0, merged)],
            "the new price with the rest as last sent, and once",
        );

        // On a delayed feed the reference client numbers the model apart, and
        // a program that asked for delayed data reads it there.
        client.core.mark_feed_delayed_for_test(slot);
        publish(0.21);
        let mut delayed_heard = Heard::default();
        client.process_msgs(&mut delayed_heard);
        let kinds: Vec<(i64, i32)> = delayed_heard.models.iter().map(|m| (m.0, m.1)).collect();
        assert_eq!(kinds, [(1, 83), (2, 83), (3, 83)], "the delayed model, not the live one");
        side(Last, bid);
        let mut delayed_heard = Heard::default();
        client.process_msgs(&mut delayed_heard);
        let kinds: Vec<(i64, i32)> = delayed_heard.models.iter().map(|m| (m.0, m.1)).collect();
        assert_eq!(kinds, [(1, 82), (2, 82), (3, 82)], "the delayed last's, not the live one");

        // A number given up and asked under again is a request of its own,
        // sent the model it has not been sent.
        crate::api::client::tests::reported(&client, || client.cancel_mkt_data(1)).unwrap();
        client.try_req_mkt_data(1, &crate::api::client::tests::spy(), "", false, false)
            .expect("taken");
        crate::api::client::tests::settled(&client, &rx);
        publish(0.21);
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        let kinds: Vec<(i64, i32)> = heard.models.iter().map(|m| (m.0, m.1)).collect();
        assert_eq!(kinds, [(1, 83)], "the same model to the same number, asked again");

        // Withdrawn where the engine takes the cancels, so what the venue
        // says of the contract after that is nobody's.
        for req_id in [1, 2, 3] {
            crate::api::client::tests::reported(&client, || client.cancel_mkt_data(req_id)).unwrap();
        }
        crate::api::client::tests::settled(&client, &rx);
        publish(0.22);
        shared.market.push_option_computation(crate::types::OptionComputation {
            instrument: slot, answers: Some(7), ..Default::default()
        });
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        assert!(heard.news.is_empty(), "a withdrawn watch was sent news: {:?}", heard.news);
        let kinds: Vec<(i64, i32)> = heard.models.iter().map(|m| (m.0, m.1)).collect();
        assert_eq!(kinds, [(7, 53)], "only the explicitly addressed answer is owed");
    }

    /// What the venue says on its own account reaches the caller under the
    /// venue's own code.
    ///
    /// It belongs to no request, so it is reported against none. The number is
    /// not a stand-in for "something went wrong": the venue sends this message
    /// on its own subtype and every client is told it under this code, so a
    /// caller matching on the number reads nothing where another number is
    /// reported in its place.
    #[test]
    fn a_message_the_venue_sends_on_its_own_account_keeps_the_venues_code() {
        #[derive(Default)]
        struct Heard(Vec<(i64, i64, String)>);
        impl Wrapper for Heard {
            fn error(&mut self, req_id: i64, code: i64, msg: &str, _: &str) {
                self.0.push((req_id, code, msg.to_string()));
            }
        }
        let (client, _rx, shared) = crate::api::client::tests::test_client();
        shared.market.push_venue_error("the venue is going down at 17:00".to_string());
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        assert_eq!(
            heard.0,
            [(-1, 2148, "the venue is going down at 17:00".to_string())],
            "against no request, under the venue's own code",
        );
    }

    #[derive(Default)]
    struct Sizes(Vec<f64>);

    impl Wrapper for Sizes {
        fn tick_by_tick_all_last(
            &mut self, _req_id: i64, _kind: i32, _time: i64, _price: f64, size: f64,
            _attrib: &TickAttribLast, _exchange: &str, _conditions: &str,
        ) {
            self.0.push(size);
        }
        fn tick_by_tick_bid_ask(
            &mut self, _req_id: i64, _time: i64, _bid: f64, _ask: f64,
            bid_size: f64, ask_size: f64, _attrib: &TickAttribBidAsk,
        ) {
            self.0.push(bid_size);
            self.0.push(ask_size);
        }
    }

    /// A size reaches a caller as the number of shares it is.
    ///
    /// Sizes cross the wire scaled by `QTY_SCALE` and are divided on delivery.
    /// Driven through `process_msgs` rather than checked as arithmetic, so this
    /// fails if delivery stops or the scaling is dropped.
    #[test]
    fn a_size_reaches_a_caller_as_itself() {
        let (client, _rx, shared) = crate::api::client::tests::test_client();
        shared.market.push_tbt_trade(TbtTrade {
            instrument: 0, req_id: 1, kind: crate::types::TbtType::Last, price: PRICE_SCALE, size: 100 * QTY_SCALE,
            timestamp: 0, exchange: "NYSE".into(), conditions: String::new(),
            past_limit: false, unreported: false,
        });
        shared.market.push_tbt_quote(TbtQuote {
            instrument: 0, req_id: 2, bid: PRICE_SCALE, ask: PRICE_SCALE,
            // The smallest representable size, beside an ordinary one.
            bid_size: 50 * QTY_SCALE, ask_size: 1,
            timestamp: 0, bid_past_low: false, ask_past_high: false,
        });

        let mut sizes = Sizes::default();
        client.process_msgs(&mut sizes);
        assert_eq!(sizes.0, vec![100.0, 50.0, 1e-8]);
    }

    /// A correction can reopen an order after its completion has armed cleanup
    /// and before anyone asks for completed orders again.
    #[test]
    fn a_deferred_completion_leaves_a_reopened_order_working() {
        use crate::engine::{context::Context, hot_loop::ccp::CcpState};

        let (client, _rx, shared) = crate::api::client::tests::test_client();
        let mut ccp = CcpState::new();
        let mut context = Context::new();
        let instrument = context.register_instrument(756733);
        context.set_symbol(instrument, "SPY".into());
        context.insert_order(crate::types::Order::new(
            7, instrument, crate::types::Side::Buy,
            100 * QTY_SCALE, 400 * PRICE_SCALE, b'2', b'0', 0,
        ));
        let report = |status, kind, exec_id, filled| {
            [(11, "7"), (39, status), (150, kind), (17, exec_id),
             (54, "1"), (6008, "756733"), (38, "100"), (14, filled),
             (32, filled), (31, "412.25")]
                .into_iter().map(|(tag, value)| (tag, value.to_string())).collect()
        };
        let mut wrapper = Sizes::default();
        ccp.handle_exec_report(
            &report("2", "F", "fill-1", "100"), b"", &mut context, &shared, &None, "",
        );
        client.process_msgs(&mut wrapper);
        crate::api::client::tests::completed_orders_asked_and_answered(&client, false); client.process_msgs(&mut wrapper);
        assert!(client.deferred_evictions.lock().unwrap().contains(&7));

        ccp.handle_exec_report(
            &report("1", "G", "correction-1", "50"), b"", &mut context, &shared, &None, "",
        );
        assert!(!shared.orders.has_pending_fill(7));
        assert!(shared.orders.venue_is_working(7));
        client.process_msgs(&mut wrapper);
        let reopened = shared.orders.get_order_info(7).expect("the working row survives cleanup");
        assert_eq!(reopened.contract.con_id, 756733);
        assert_eq!(reopened.order.order_id, 7);
        assert!(shared.orders.venue_is_working(7));
        assert!(!client.deferred_evictions.lock().unwrap().contains(&7));

        // The venue saying the order is unknown still removes a working row.
        shared.orders.remove_order_info(7);
        assert!(shared.orders.get_order_info(7).is_none());
        shared.orders.push_order_correction(7, "", reopened);

        ccp.handle_exec_report(
            &report("2", "F", "fill-2", "100"), b"", &mut context, &shared, &None, "",
        );
        client.process_msgs(&mut wrapper);
        crate::api::client::tests::completed_orders_asked_and_answered(&client, false); client.process_msgs(&mut wrapper);
        assert!(client.deferred_evictions.lock().unwrap().contains(&7));
        client.process_msgs(&mut wrapper);
        assert!(shared.orders.get_order_info(7).is_none(), "a terminal row is still reclaimed");
    }

    /// What each order callback was told, in the order it was told.
    #[derive(Default)]
    struct Pairs(Vec<(&'static str, i64, String, String)>);

    impl Wrapper for Pairs {
        fn open_order(
            &mut self, order_id: i64, contract: &crate::types::model::Contract,
            order: &crate::types::model::Order, state: &crate::types::model::OrderState,
        ) {
            self.0.push(("open_order", order_id, contract.symbol.clone(), state.status.clone()));
            assert_eq!(order.order_id, order_id, "the order it holds, not a blank one");
        }
        fn order_status(
            &mut self, order_id: i64, status: &str, _filled: f64, _remaining: f64,
            _avg_fill_price: f64, _perm_id: i64, _parent_id: i64,
            _last_fill_price: f64, _client_id: i64, _why_held: &str, _mkt_cap_price: f64,
        ) {
            self.0.push(("order_status", order_id, String::new(), status.into()));
        }
    }

    /// A status change with no fill on the report states the order beside the
    /// status, as the other surface does and as this one's contract says.
    #[test]
    fn a_status_change_states_the_order_beside_it() {
        let (client, _rx, shared) = crate::api::client::tests::test_client();
        client.core.track_order(
            7,
            crate::types::model::Contract {
                con_id: 756733, symbol: "SPY".into(), sec_type: "STK".into(),
                exchange: "SMART".into(), ..Default::default()
            },
            crate::types::model::Order {
                order_id: 7, action: "BUY".into(), total_quantity: 100.0,
                order_type: "LMT".into(), lmt_price: 400.0, ..Default::default()
            },
            0,
        );
        shared.orders.push_order_update(crate::types::OrderUpdate {
            order_id: 7, instrument: 0, status: crate::types::OrderStatus::Submitted,
            filled_qty: 0.0, remaining_qty: 100.0, avg_price: 0,
            perm_id: 0, parent_id: 0, timestamp_ns: 0,
        });

        let mut pairs = Pairs::default();
        client.process_msgs(&mut pairs);
        assert_eq!(
            pairs.0,
            vec![
                ("open_order", 7, "SPY".to_string(), "Submitted".to_string()),
                ("order_status", 7, String::new(), "Submitted".to_string()),
            ],
        );
    }

    /// And a preview is not stated that way, because a preview is answered by
    /// what the venue says it would cost.
    ///
    /// Asking what an order would do places one, so a preview is a tracked
    /// order like any other and its statuses arrive on this path. The question
    /// waits on the first statement carrying its number, so a pair sent from
    /// the status alone answered it — with no margin figures and a cost of
    /// nought rather than the number that means unstated, on the one call whose
    /// whole purpose is to say what an order would cost.
    #[test]
    fn a_preview_is_not_answered_by_the_status_beside_it() {
        let (client, _rx, shared) = crate::api::client::tests::test_client();
        client.core.track_order(
            8,
            crate::types::model::Contract {
                con_id: 756733, symbol: "SPY".into(), sec_type: "STK".into(),
                exchange: "SMART".into(), ..Default::default()
            },
            crate::types::model::Order {
                order_id: 8, action: "BUY".into(), total_quantity: 100.0,
                order_type: "LMT".into(), lmt_price: 400.0,
                what_if: true, ..Default::default()
            },
            0,
        );
        shared.orders.push_order_update(crate::types::OrderUpdate {
            order_id: 8, instrument: 0, status: crate::types::OrderStatus::PreSubmitted,
            filled_qty: 0.0, remaining_qty: 100.0, avg_price: 0,
            perm_id: 0, parent_id: 0, timestamp_ns: 0,
        });

        let mut pairs = Pairs::default();
        client.process_msgs(&mut pairs);
        assert!(
            pairs.0.iter().all(|(what, ..)| *what != "open_order"),
            "the status answered the preview in place of the venue's own reply: {:?}",
            pairs.0,
        );
    }
}
