//! Market-data requests as their callers make them.
//!
//! A call checks what needs no venue and hands the request over. What a
//! gateway does with a request between taking it and serving it happens here,
//! in the loop's own laps: the contract is registered on the slot it already
//! holds or on a new one, the request is served off the subscription the
//! contract already has or one is asked for, and a record in the session's
//! order says which slot the request is served on, ahead of everything that
//! arrives under it. The loop keeps every request it has taken, so a cancel —
//! which it takes after the request — finds what it withdraws, and the last
//! request on a contract is the one that takes the subscription down.

use std::collections::HashSet;

use crate::bridge::{MarketDataTaken, Record};
use crate::error_codes::{DUPLICATE_TICKER_ID, NO_SUCH_SUBSCRIPTION, Refusal};
use crate::types::model::ErrorOrigin;
use crate::types::{ContractRef, ControlCommand, InstrumentId};

use super::HotLoop;

/// A market-data request the loop has taken and not withdrawn.
#[derive(Debug)]
pub(crate) struct MdRequest {
    /// The slot it is served on.
    pub(crate) slot: InstrumentId,
    /// The contract it named, zero until the venue numbers it.
    pub(crate) con_id: i64,
    /// The extra series it named.
    pub(crate) series: Vec<u32>,
    /// The providers it asked the contract's headlines of, where it did.
    pub(crate) news: Option<String>,
    /// Whether it is a spread scan.
    pub(crate) scan: bool,
    /// Whether it was opened for a calculation, whose cancel withdraws it.
    pub(crate) for_calculation: bool,
}

impl HotLoop {
    /// The next number in the order of what this loop decides about a
    /// subscription: the occupancy a request begins is named by it, and a
    /// withdrawal decided later carries a later one.
    pub(crate) fn next_md_number(&mut self) -> u64 {
        self.md_numbers += 1;
        self.md_numbers
    }

    /// Refuse a market-data request under its number. One the engine opened
    /// for itself is refused to nobody.
    fn refuse_md(&self, req_id: i64, why: Refusal) {
        log::warn!("market data {req_id} refused: {}", why.message);
        if super::intake::engine_owned(req_id) {
            return;
        }
        self.shared.push_refused(
            ErrorOrigin::Request { id: req_id, ends: true },
            i64::from(why.code),
            why.message,
        );
    }

    /// Take a market-data request.
    pub(crate) fn take_subscription(&mut self, cmd: ControlCommand) {
        let ControlCommand::Subscribe {
            req_id,
            contract,
            filters,
            mode_9887,
            delayed_mode,
            frozen,
            delayed_frozen,
            regulatory_snapshot,
            snapshot,
            generic_ticks,
            news,
            spread_scan,
            calculation,
        } = cmd
        else {
            return;
        };
        if let Some(why) = self.farm_halted {
            return self.refuse_md(
                req_id,
                Refusal::stated(
                    Refusal::NO_DEFINITION,
                    format!(
                        "market data is unavailable for the rest of this session: {}",
                        why.as_str()
                    ),
                ),
            );
        }
        if self.held_scans.iter().chain(self.ccp.resolved_named.iter())
            .chain(self.ccp.pending_named.iter().map(|(_, cmd, _)| cmd)).any(|held| {
            matches!(held, ControlCommand::Subscribe { req_id: waiting, .. } if *waiting == req_id)
        }) {
            return self.refuse_md(req_id, Refusal::stated(
                DUPLICATE_TICKER_ID,
                format!("request {req_id} is already waiting for market data: withdraw it before asking again"),
            ));
        }
        // One request per number. Taken twice, the two contracts' quotes were
        // delivered under one number with nothing to tell them apart, and the
        // withdrawal reached only the second.
        if let Some(held) = self.md_requests.get(&req_id) {
            // Except a calculation asked under a number already watching its
            // contract: that watch is what makes the venue state the model, so
            // the question is kept on it.
            if let Some(asked) = calculation
                && contract.con_id != 0
                && held.con_id == contract.con_id
            {
                let slot = held.slot;
                self.keep_calculation(req_id, slot, *asked);
                return;
            }
            return self.refuse_md(
                req_id,
                Refusal::stated(
                    DUPLICATE_TICKER_ID,
                    format!(
                        "request {req_id} is already watching a contract: withdraw it before \
                     asking for another under the same number",
                    ),
                ),
            );
        }
        // One scan of a contract at a time. Its series is asked once per slot
        // and its answer kept per slot, so a second scan sent while the first
        // stands went out with the first one's text and read its answer. Held
        // until the scan before it is withdrawn.
        if spread_scan.is_some()
            && contract.con_id > 0
            && self.md_requests.values().any(|r| r.scan && r.con_id == contract.con_id)
        {
            self.held_scans.push(ControlCommand::Subscribe {
                req_id,
                contract,
                filters,
                mode_9887,
                delayed_mode,
                frozen,
                delayed_frozen,
                regulatory_snapshot,
                snapshot,
                generic_ticks,
                news,
                spread_scan,
                calculation,
            });
            return;
        }
        let issued = self.next_md_number();
        let ContractRef {
            con_id,
            symbol,
            exchange,
            sec_type,
            currency,
            last_trade_date,
            strike,
            right,
            multiplier,
        } = contract;
        // The strategies series is asked for the way every other is and the
        // scan is what tells the venue what to look for, so it is handed to
        // the farm before the subscription goes — the scan this request
        // stated, not whichever the contract last had.
        if con_id > 0
            && let Some(scan) = &spread_scan
        {
            self.farm.note_spread_scan(con_id, scan.clone());
        }
        // What tells two conId-less contracts on one underlying apart.
        // Built by the same function an order uses, or the two describe one
        // contract differently: the slot a subscription took would not be
        // found again by an order, which would take a second one — with no
        // quote on it, and stating the wrong currency because the slot it did
        // take never recorded one.
        let option_key = crate::types::model::contract_identity(
            &last_trade_date,
            strike,
            &right,
            &multiplier,
            &currency,
        );
        // And what narrows the lookup that will name it. None of these says
        // what the contract is, so none of them belongs in the identity — and
        // each of them decides which listing the venue answers with, so two
        // descriptions the venue would answer differently are not one
        // contract. Left out, the second description followed the first one's
        // subscription and was served the other listing's prices under its own
        // number.
        let narrowing = format!(
            "{}|{}|{}|{}|{}|{}",
            filters.primary_exchange,
            filters.local_symbol,
            filters.trading_class,
            filters.sec_id_type,
            filters.sec_id,
            filters.issuer_id,
        );
        let narrowing = if narrowing.chars().all(|c| c == '|') { String::new() } else { narrowing };
        let id = self.register_contract(
            con_id,
            symbol.clone(),
            &sec_type,
            &exchange,
            &option_key,
            &narrowing,
        );
        // Held against the slot before the subscription goes out, so the frame
        // that carries them is built from them and the rebuild after a
        // reconnect asks for them again.
        //
        // Written against the slot only where this request is the one the
        // subscription goes out for. A request landing on a slot another is
        // already being served on joins it: its list is added to what the slot
        // asks for as the series are asked for, below, because written here it
        // would leave nothing new to ask for — and written over theirs it would
        // leave the venue serving series the rebuild no longer asks for.
        // And never from a chargeable snapshot: the message that carries one
        // states no extra series at all, so a series named beside it went out
        // on nothing — and stayed on the list the rebuild after a reconnect
        // reads, which asked the venue for it as part of a stream that never
        // named it.
        if !generic_ticks.is_empty() && !regulatory_snapshot && !self.farm.holds_a_stream(id) {
            let held = self.farm.asked_generic_ticks.entry(id).or_default();
            for tick in &generic_ticks {
                if !held.contains(tick) {
                    held.push(*tick);
                }
            }
        }
        // Already subscribed, so nothing goes to the venue again: one contract
        // holds one subscription on the wire, and the request is served off
        // the one that is up, so two parts of one program may watch one
        // contract.
        //
        // Except the chargeable snapshot, which is a request of its own and
        // not a share of somebody's stream. Handed the stream instead it was
        // never sent, never billed and never refused for want of the
        // entitlement, and the caller heard the snapshot end off ticks it did
        // not ask for — a paid answer that reached nobody.
        //
        // And what is joined is a stream. A snapshot holds the slot but is
        // withdrawn as soon as it completes, so a request pointed at one was
        // never sent and the withdrawal took the record out from under it.
        let joins = self.farm.holds_a_stream(id) && !regulatory_snapshot;
        if joins {
            // This request is asking for the contract too, so the subscription
            // it is served off is one it asked for.
            self.farm.note_subscription_asked_on(id, issued);
            self.farm.note_series_asked_on(id, &generic_ticks, issued);
        } else {
            self.farm.note_subscription_asked_on(id, issued);
            // A request that begins the stream owns the occupancy, and one
            // beside what is already there did not begin it and must not
            // rename it.
            if self.farm.holds_a_stream(id) {
                self.farm.note_subscription_began_under(id, issued);
            } else {
                self.farm.note_it_changed_hands(id, issued);
            }
            self.publish_occupancy(id);
            self.farm.note_series_asked_on(id, &generic_ticks, issued);
            // The venue states it on the logon. A stream held for a
            // reconnect still needs its line.
            let allowance = self.ccp_conn.as_ref().map_or(40, |c| c.market_data_allowance);
            let subscribed = self
                .context
                .market
                .active_instruments()
                .filter(|(at, _)| *at != id && self.farm.holds_a_stream(*at))
                .count();
            if !regulatory_snapshot && subscribed >= allowance {
                log::warn!(
                    "subscription refused: {subscribed} of {allowance} quote lines are in use, \
                     which is what the venue allows this session",
                );
                self.refuse_md(
                    req_id,
                    Refusal::stated(101, "Max number of tickers has been reached"),
                );
                self.try_reclaim_instrument(id);
                return;
            }
        }
        if !joins && !regulatory_snapshot {
            self.farm.note_data_type(id, mode_9887, delayed_mode, &self.shared);
        }
        // Taken: the record that says so stands ahead of everything the
        // subscription brings under this slot.
        self.md_requests.insert(
            req_id,
            MdRequest {
                slot: id,
                con_id,
                series: generic_ticks.clone(),
                news: news.clone(),
                scan: spread_scan.is_some(),
                for_calculation: calculation.is_some(),
            },
        );
        let (stated_type, _) = self.described_as(con_id, &sec_type, &exchange);
        // One the engine opened for itself is served to nobody, so nothing
        // is said of it.
        if !super::intake::engine_owned(req_id) {
            self.shared.push_call_record(Record::MarketDataTaken(Box::new(MarketDataTaken {
                req_id,
                slot: id,
                generation: self.farm.what_took_it(id),
                con_id,
                series: generic_ticks.clone(),
                snapshot,
                asked_at: std::time::Instant::now(),
                one_shot: regulatory_snapshot,
                data_type: self.shared.market.subscription_data_type(id, crate::client_core::data_type_for_mode(mode_9887))
                    .load(std::sync::atomic::Ordering::Relaxed),
                marked: crate::client_core::marked_as_option(&stated_type),
            })));
        }
        if con_id > 0 {
            self.ask_for_headlines(req_id, &stated_type);
        }
        // A calculation the model answers, kept on the slot the request is
        // served on and answered where the venue states the model — and now,
        // where it already has.
        if let Some(asked) = calculation {
            self.keep_calculation(req_id, id, *asked);
        }
        if joins {
            if !super::intake::engine_owned(req_id) {
                if let Some(params) = self.shared.market.tick_req_params_for_follower(id) {
                    self.shared.market.push_tick_req_params_for(req_id, params);
                }
                if let Some(reason) = self.shared.market.failure_for_follower(id) {
                    self.shared.market.push_subscription_failure_for(req_id, reason);
                }
            }
            // The joiner's own series, where the stream it joins was not asked
            // for them. Nothing else sends them: the subscription is already
            // up, so this is the one chance to ask.
            if !generic_ticks.is_empty() {
                self.farm.also_ask_for_series(
                    id,
                    con_id,
                    &generic_ticks,
                    &self.context,
                    &mut self.farm_conn,
                    &mut self.hb,
                );
            }
            return;
        }
        if regulatory_snapshot && self.farm.disconnected {
            // A snapshot is not recorded for replay,
            // so one with no transport to carry it is simply lost — and said
            // only where no stream on the contract would hear it as its own
            // refusal.
            if !self.farm.holds_a_stream(id) {
                self.shared.market.push_subscription_failure(
                    id,
                    "the quote feed was down when this snapshot was asked for, so it was \
                     never sent: ask for it again once the feed is back"
                        .to_string(),
                );
            }
        } else {
            let (sec_type, exchange) = self.described_as(con_id, &sec_type, &exchange);
            if !regulatory_snapshot {
                self.farm.note_frozen_feeds(
                    id, frozen, delayed_frozen, con_id, &sec_type, &exchange, &self.shared,
                );
            }
            self.farm.send_mktdata_subscribe(
                con_id,
                &symbol,
                &exchange,
                &sec_type,
                &last_trade_date,
                strike,
                &right,
                &multiplier,
                id,
                mode_9887,
                regulatory_snapshot,
                &mut self.farm_conn,
                &mut self.hb,
            );
        }
    }

    /// Keep a calculation on the slot its request is served on, and answer it
    /// now where the venue has already stated the model.
    fn keep_calculation(&self, req_id: i64, slot: InstrumentId, asked: crate::types::Calculation) {
        self.shared.market.keep_calculation(
            req_id,
            crate::bridge::KeptCalculation {
                contract: asked.contract,
                slot,
                wants_volatility: asked.wants_volatility,
                option_price: asked.option_price,
                under_price: asked.under_price,
                answered: false,
            },
        );
        crate::client_core::answer_kept_calculations(&self.shared, None, Some(req_id));
    }

    /// Ask for a contract's headlines for a request that wants them, once the
    /// contract is numbered, where nobody on it has already asked.
    ///
    /// Asked for once per contract: the venue is asked by contract and
    /// withdrawn by contract, so asking twice leaves a subscription the one
    /// withdrawal cannot match.
    pub(crate) fn ask_for_headlines(&mut self, req_id: i64, sec_type: &str) {
        let Some(req) = self.md_requests.get(&req_id) else { return };
        let (Some(providers), slot, con_id) = (req.news.clone(), req.slot, req.con_id) else {
            return;
        };
        if con_id <= 0 || self.farm.news_subscriptions.iter().any(|(at, ..)| *at == slot) {
            return;
        }
        let news_req = self.farm.next_md_req_id;
        self.farm.next_md_req_id += 1;
        self.farm.send_news_subscribe(
            con_id,
            slot,
            sec_type,
            &providers,
            news_req,
            &mut self.farm_conn,
            &mut self.hb,
        );
    }

    /// Withdraw a market-data request, by the caller's number for it.
    pub(crate) fn withdraw_mkt_data(&mut self, req_id: i64) {
        let naming = self.ccp.pending_named.len();
        self.ccp.pending_named.retain(|(_, held, _)| {
            !matches!(held, ControlCommand::Subscribe { req_id: waiting, .. } if *waiting == req_id)
        });
        if self.ccp.pending_named.len() != naming {
            return;
        }
        if let Some(at) = self.ccp.resolved_named.iter().position(|held| {
            matches!(held, ControlCommand::Subscribe { req_id: waiting, .. } if *waiting == req_id)
        }) {
            if let ControlCommand::Subscribe { contract, .. } = self.ccp.resolved_named.remove(at) {
                self.release_next_scan(contract.con_id);
            }
            return;
        }
        // A scan held behind another on its contract never reached the venue,
        // and is forgotten.
        if let Some(at) = self.held_scans.iter().position(|held| {
            matches!(held, ControlCommand::Subscribe { req_id: held, .. } if *held == req_id)
        }) {
            self.held_scans.remove(at);
            return;
        }
        // A caller withdrawing a subscription this session does not hold
        // branches on being told so. Said nothing, the withdrawal reads
        // exactly like one that worked.
        let Some(req) = self.md_requests.remove(&req_id) else {
            return self.refuse_md(
                req_id,
                Refusal::stated(
                    NO_SUCH_SUBSCRIPTION,
                    format!("no contract is being watched under request {req_id}"),
                ),
            );
        };
        if !super::intake::engine_owned(req_id) {
            self.shared.push_call_record(Record::MarketDataWithdrawn(req_id));
        }
        // The calculation it was opened for goes with it.
        self.shared.market.forget_calculation(req_id);
        let slot = req.slot;
        let issued = self.next_md_number();
        let still_watched = self.md_requests.values().any(|r| r.slot == slot);
        if !still_watched {
            // The last request on the contract: the subscription goes whole,
            // and its series with it.
            self.farm.send_mktdata_unsubscribe(
                slot,
                0,
                0,
                &req.series,
                issued,
                false,
                &mut self.farm_conn,
                &mut self.hb,
            );
        } else {
            // The subscription stays up for the others, and what this request
            // brought with it goes: only the series nobody else watching the
            // contract named. Left behind, the venue served them for as long
            // as that subscription outlived this request, and every rebuild
            // after a reconnect asked for them again.
            let wanted: HashSet<u32> = self
                .md_requests
                .values()
                .filter(|r| r.slot == slot)
                .flat_map(|r| r.series.iter().copied())
                .collect();
            let unwanted: Vec<u32> =
                req.series.iter().copied().filter(|t| !wanted.contains(t)).collect();
            if !unwanted.is_empty() {
                self.farm.stop_asking_for_series(
                    slot,
                    0,
                    0,
                    &unwanted,
                    issued,
                    &mut self.farm_conn,
                    &mut self.hb,
                );
            }
        }
        // The headlines stop with the last request that asked for them, while
        // the quotes may stay up for another.
        if req.news.is_some()
            && !self.md_requests.values().any(|r| r.slot == slot && r.news.is_some())
        {
            self.farm.send_news_unsubscribe(slot, &mut self.farm_conn, &mut self.hb);
        }
        // A scan's answer is kept per slot: withdrawn, it is not the next
        // scan's to read. And the next scan of this contract, held behind this
        // one, is taken now.
        if req.scan {
            self.shared.market.forget_scanned_strategies(slot);
            self.farm.forget_spread_scan(req.con_id);
            self.release_next_scan(req.con_id);
        }
        // The tags are dead with the requests that earned them, and the slot
        // only drops them when it goes: news is the one reader that outlives
        // the quotes, so a live news subscription keeps them.
        if !self.farm.holds_market_data(slot)
            && !self.farm.news_subscriptions.iter().any(|(id, ..)| *id == slot)
        {
            self.context.market.clear_server_tags_for(slot);
        }
        self.try_reclaim_instrument(slot);
    }

    /// Release only the next scan of this contract. The others still wait
    /// behind it, and its withdrawal can release the one after it.
    fn release_next_scan(&mut self, con_id: i64) {
        if let Some(at) = self.held_scans.iter().position(|held| {
            matches!(held, ControlCommand::Subscribe { contract, .. } if contract.con_id == con_id)
        }) {
            self.ccp.resolved_named.push(self.held_scans.remove(at));
        }
    }

    /// Withdraw a calculation: forget the question, and the watch it opened
    /// for it where it opened one. Said nothing: the caller withdrew a
    /// question, not a subscription, and one answered in the call it was
    /// asked in left nothing to withdraw.
    pub(crate) fn withdraw_calculation(&mut self, req_id: i64) {
        self.shared.market.forget_calculation(req_id);
        let holds_it = |cmd: &ControlCommand| {
            matches!(cmd,
            ControlCommand::Subscribe { req_id: held, calculation: Some(_), .. } if *held == req_id)
        };
        self.ccp.pending_named.retain(|(_, cmd, _)| !holds_it(cmd));
        self.ccp.resolved_named.retain(|cmd| !holds_it(cmd));
        if self.md_requests.get(&req_id).is_some_and(|req| req.for_calculation) {
            self.withdraw_mkt_data(req_id);
        }
    }
}
