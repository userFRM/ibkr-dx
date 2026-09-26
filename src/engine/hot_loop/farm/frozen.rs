//! A market's status, watched where a gateway watches it, and the frozen feeds
//! a closed market moves a subscription to.
//!
//! Where the logon enables frozen data, a gateway watches the status of the
//! market for a subscription taken with frozen data on, and for one that has
//! fallen back to delayed data with delayed-frozen data on. While the status
//! says the market is closed the contract is in the frozen state: the frozen
//! quote is asked for beside the one being served, and the delayed-frozen one
//! beside a delayed one, and the program is served those instead. Once the
//! status says the market is open again both are withdrawn and the program is
//! served what it was before.

use super::*;
use crate::engine::market_state::TradeClock;
use crate::types::{Quote, SeriesTick};

/// The venue's number for a market's status: whether it is closed.
pub(super) const MARKET_DATA_STATUS_REQUEST_TYPE: u32 = 398;

/// The frozen feed, as 9887 names it.
const FROZEN_FEED: i32 = 2;

/// The delayed-frozen feed.
const DELAYED_FROZEN_FEED: i32 = 3;

/// The kinds of contract a frozen quote is served for.
const FROZEN_QUOTED: [&str; 20] = [
    "STK", "CFD", "OPT", "FOP", "WAR", "IOPT", "FUT", "FWD", "BAG", "ICS", "PDC", "CASH", "IND",
    "BOND", "BILL", "FIXED", "SLB", "CMDTY", "CRYPTO", "EC",
];

/// A subscription whose market status is watched, and what the status has
/// made of it.
#[derive(Default)]
pub(super) struct StatusWatch {
    /// Taken with frozen data on, and the live quote not refused: that quote
    /// is served frozen.
    frozen: bool,
    /// Taken with delayed-frozen data on: once fallen back to delayed data, it
    /// is served delayed-frozen.
    delayed_frozen: bool,
    con_id: i64,
    sec_type: String,
    /// Where the subscription is asked for.
    exchange: String,
    /// Where the status is asked for.
    status_venue: String,
    /// What the frozen quote's last-trade entry states on tag 9839, as the
    /// subscription's does.
    last_precision: &'static str,
    /// The number the status is asked under, once it is.
    status: Option<u32>,
    /// What the status last said: the market is closed.
    closed: bool,
    /// The contract is in the frozen state.
    in_frozen_state: bool,
    /// The frozen quotes asked for, by feed.
    pairs: Vec<FrozenPair>,
    /// The feeds the venue refused.
    refused: Vec<i32>,
    /// The program is served the frozen quote.
    shows_frozen: bool,
    /// The program is served the delayed-frozen quote.
    shows_delayed_frozen: bool,
    /// The first delayed-frozen quote is to be served, and the delayed one
    /// with it no longer.
    armed: bool,
    /// The quote being served before, kept up while the program is served
    /// another.
    held: Option<Held>,
}

/// A frozen quote asked for: its feed, its two requests, and the numbers the
/// venue answers them under.
struct FrozenPair {
    feed: i32,
    requests: [u32; 2],
    refused: Vec<u32>,
    tags: Vec<u32>,
}

/// A quote kept up while it is not the one served.
struct Held {
    quote: Quote,
    clock: TradeClock,
    series: Vec<SeriesTick>,
}

/// What a quote entry is to a subscription whose market status is watched.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// Served to the program.
    Serve,
    /// Kept up, for when it is served again.
    Hold,
    /// Not served.
    Drop,
}

/// Where a gateway asks a contract's status: where the contract is asked for
/// on the smart route, and otherwise on the contract's preferred market or on
/// the smart route where it is listed there.
fn status_venue(shared: &SharedState, con_id: i64, exchange: &str) -> String {
    let exchange = if exchange.is_empty() { "SMART" } else { exchange };
    if matches!(exchange, "SMART" | "OVERNIGHT" | "IBEOS") {
        return exchange.to_string();
    }
    let Some(definition) = shared.reference.contract_definition(con_id as u32, exchange) else {
        return exchange.to_string();
    };
    let listed = |market: &str| definition.valid_exchanges.iter().any(|valid| valid == market);
    let other = match shared.reference.preferred_market(definition.agg_group) {
        Some(market) if !matches!(market.as_str(), "" | "SMART" | "BEST") => {
            (market != exchange && listed(&market)).then_some(market)
        }
        _ => (exchange != "BEST" && listed("SMART")).then(|| "SMART".to_string()),
    };
    other.unwrap_or_else(|| exchange.to_string())
}

impl FarmState {
    /// Note the frozen feeds a subscription was taken with, where a gateway
    /// watches the market's status for it: the logon enables frozen data and
    /// the contract is of a kind a frozen quote is served for.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn note_frozen_feeds(
        &mut self,
        instrument: InstrumentId,
        frozen: bool,
        delayed_frozen: bool,
        con_id: i64,
        sec_type: &str,
        exchange: &str,
        shared: &SharedState,
    ) {
        self.status_watches.remove(&instrument);
        if !(frozen || delayed_frozen)
            || !shared.reference.enables("FROZEN")
            || !FROZEN_QUOTED.contains(&sec_type)
        {
            return;
        }
        self.status_watches.insert(
            instrument,
            StatusWatch {
                frozen,
                delayed_frozen,
                con_id,
                sec_type: sec_type.to_string(),
                exchange: exchange.to_string(),
                status_venue: status_venue(shared, con_id, exchange),
                last_precision: super::quote_precision(
                    shared, REALTIME_LAST_REQUEST_TYPE, con_id, exchange, sec_type,
                ),
                ..StatusWatch::default()
            },
        );
    }

    /// Start a subscription's watch afresh as the subscription goes out: the
    /// status is asked for with the quote where frozen data is on.
    pub(super) fn start_status_watch(
        &mut self,
        instrument: InstrumentId,
        farm_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        *watch = StatusWatch {
            frozen: watch.frozen,
            delayed_frozen: watch.delayed_frozen,
            con_id: watch.con_id,
            sec_type: std::mem::take(&mut watch.sec_type),
            exchange: std::mem::take(&mut watch.exchange),
            status_venue: std::mem::take(&mut watch.status_venue),
            last_precision: watch.last_precision,
            ..StatusWatch::default()
        };
        if watch.frozen {
            self.ask_status(instrument, farm_conn, hb);
        }
    }

    fn ask_status(
        &mut self,
        instrument: InstrumentId,
        farm_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        let req_id = self.next_md_req_id;
        self.next_md_req_id += 1;
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        watch.status = Some(req_id);
        self.md_req_to_instrument.push((req_id, instrument));
        self.generic_tick_reqs.push((req_id, MARKET_DATA_STATUS_REQUEST_TYPE));
        let Some(conn) = farm_conn.as_mut() else { return };
        let mut tags = build_trading_status_subscribe_tags(
            req_id,
            watch.con_id,
            &watch.sec_type,
            &watch.status_venue,
            &chrono_free_timestamp(),
        );
        for (tag, value) in tags.iter_mut() {
            if *tag == 264 {
                *value = MARKET_DATA_STATUS_REQUEST_TYPE.to_string();
            }
        }
        if let Some(combo) = self.attached_combo_quotes.get(&instrument) {
            combo.decorate(&mut tags);
        }
        let refs: Vec<(u32, &str)> =
            tags.iter().map(|(tag, value)| (*tag, value.as_str())).collect();
        let _ = conn.send_fixcomp(&refs);
        hb.last_farm_sent = Instant::now();
    }

    /// A market's status as the venue states it: closed where it states one.
    pub(super) fn note_market_status(&mut self, instrument: InstrumentId, payload: &[u8]) {
        if let Some(status) = series_i32(payload, 0) {
            self.statuses_stated.push((instrument, status == 1));
        }
    }

    /// Act on the statuses the venue has stated.
    pub(super) fn apply_market_status(
        &mut self,
        farm_conn: &mut Option<Connection>,
        context: &mut Context,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        for (instrument, closed) in std::mem::take(&mut self.statuses_stated) {
            self.apply_status(instrument, closed, farm_conn, context, shared, hb);
        }
    }

    fn apply_status(
        &mut self,
        instrument: InstrumentId,
        closed: bool,
        farm_conn: &mut Option<Connection>,
        context: &mut Context,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        let fell_back = self.fell_back(instrument);
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        watch.closed = closed;
        if closed && !watch.in_frozen_state {
            // A subscription served delayed data goes frozen only where
            // delayed-frozen data is served for it.
            if fell_back && (shared.reference.enables("NODLFRZ") || watch.sec_type == "BAG") {
                return;
            }
            self.enter_frozen_state(instrument, farm_conn, context, shared, hb);
        } else if !closed && watch.in_frozen_state {
            self.leave_frozen_state(instrument, true, farm_conn, context, shared, hb);
        }
    }

    /// Whether the subscription has fallen back to delayed data.
    fn fell_back(&self, instrument: InstrumentId) -> bool {
        self.delayed_subscriptions.get(&instrument).is_some_and(|state| state.requests.is_some())
    }

    /// Whether the contract is in the frozen state on the live quote, which a
    /// gateway states no permission for. One that has fallen back to delayed
    /// data is in it on the delayed quote alone.
    pub(super) fn in_frozen_state(&self, instrument: InstrumentId) -> bool {
        !self.fell_back(instrument)
            && self.status_watches.get(&instrument).is_some_and(|watch| watch.in_frozen_state)
    }

    /// Whether the program is served the delayed-frozen quote.
    pub(super) fn shows_delayed_frozen(&self, instrument: InstrumentId) -> bool {
        self.status_watches.get(&instrument).is_some_and(|watch| watch.shows_delayed_frozen)
    }

    /// Whether the quote the subscription asked for is the one served: the
    /// live one unless the frozen one is, the delayed one unless the
    /// delayed-frozen one is.
    fn serves_its_own(&self, instrument: InstrumentId) -> bool {
        let fell_back = self.fell_back(instrument);
        self.status_watches.get(&instrument).is_none_or(|watch| {
            if fell_back { !watch.shows_delayed_frozen } else { !watch.shows_frozen }
        })
    }

    fn enter_frozen_state(
        &mut self,
        instrument: InstrumentId,
        farm_conn: &mut Option<Connection>,
        context: &mut Context,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        let fell_back = self.fell_back(instrument);
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        watch.in_frozen_state = true;
        let frozen = watch.frozen;
        self.ask_frozen_pair(instrument, FROZEN_FEED, farm_conn, hb);
        // Only a watch with delayed-frozen data on outlasts the fallback. Its
        // first delayed-frozen record is served, and the delayed quote until
        // then.
        if fell_back {
            self.ask_frozen_pair(instrument, DELAYED_FROZEN_FEED, farm_conn, hb);
            if let Some(watch) = self.status_watches.get_mut(&instrument) {
                watch.armed = true;
            }
        } else if frozen {
            self.show(instrument, true, false, context, shared);
            state_data_type(instrument, 2, shared);
        }
    }

    /// Leave the frozen state: the frozen quotes are withdrawn and, where the
    /// status said the market is open, the program is served what it was
    /// before. A refusal of the quote the subscription asked for leaves it
    /// without a word.
    fn leave_frozen_state(
        &mut self,
        instrument: InstrumentId,
        told: bool,
        farm_conn: &mut Option<Connection>,
        context: &mut Context,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        watch.in_frozen_state = false;
        watch.armed = false;
        let (frozen_shown, delayed_frozen_shown) = (watch.shows_frozen, watch.shows_delayed_frozen);
        let pairs = std::mem::take(&mut watch.pairs);
        for tag in pairs.iter().flat_map(|pair| pair.tags.iter()) {
            context.market.forget_server_tag(*tag);
        }
        self.withdraw_frozen_pairs(instrument, &pairs, farm_conn, hb);
        if !told {
            if let Some(watch) = self.status_watches.get_mut(&instrument) {
                watch.shows_frozen = false;
                watch.shows_delayed_frozen = false;
                watch.held = None;
            }
            return;
        }
        self.show(instrument, false, false, context, shared);
        if frozen_shown {
            state_data_type(instrument, 1, shared);
        }
        if delayed_frozen_shown {
            state_data_type(instrument, 3, shared);
        }
    }

    /// Ask for a frozen quote beside the one the subscription asked for,
    /// unless it has been asked for or refused.
    fn ask_frozen_pair(
        &mut self,
        instrument: InstrumentId,
        feed: i32,
        farm_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        let Some(watch) = self.status_watches.get(&instrument) else { return };
        if watch.refused.contains(&feed) || watch.pairs.iter().any(|pair| pair.feed == feed) {
            return;
        }
        let requests = [self.next_md_req_id, self.next_md_req_id + 1];
        self.next_md_req_id += 2;
        for req_id in requests {
            self.md_req_to_instrument.push((req_id, instrument));
        }
        let tags = build_conid_subscribe_tags(
            false,
            false,
            watch.last_precision,
            requests[0],
            requests[1],
            watch.con_id,
            &watch.exchange,
            &watch.sec_type,
            feed,
            &chrono_free_timestamp(),
            &[],
        );
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        watch.pairs.push(FrozenPair { feed, requests, refused: Vec::new(), tags: Vec::new() });
        let Some(conn) = farm_conn.as_mut() else { return };
        let mut tags = tags;
        if let Some(combo) = self.attached_combo_quotes.get(&instrument) {
            combo.decorate(&mut tags);
        }
        let refs: Vec<(u32, &str)> =
            tags.iter().map(|(tag, value)| (*tag, value.as_str())).collect();
        let _ = conn.send_fixcomp(&refs);
        hb.last_farm_sent = Instant::now();
    }

    /// Withdraw frozen quotes as they were asked for.
    fn withdraw_frozen_pairs(
        &mut self,
        instrument: InstrumentId,
        pairs: &[FrozenPair],
        farm_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        for pair in pairs {
            self.md_req_to_instrument.retain(|(req_id, _)| !pair.requests.contains(req_id));
        }
        let Some(watch) = self.status_watches.get(&instrument) else { return };
        let con_id = (watch.con_id as u32).to_string();
        let (venue, sec_type) = stated_venue_and_type(&watch.sec_type, &watch.exchange);
        let combo = self.attached_combo_quotes.get(&instrument);
        for pair in pairs {
            let Some(conn) = farm_conn.as_mut() else { continue };
            let feed = pair.feed.to_string();
            for ((req_id, request_type), precision) in pair
                .requests
                .iter()
                .zip([REALTIME_BID_ASK_REQUEST_TYPE, REALTIME_LAST_REQUEST_TYPE])
                .zip(["1", watch.last_precision])
            {
                if pair.refused.contains(req_id) {
                    continue;
                }
                let (req_id, request_type) = (req_id.to_string(), request_type.to_string());
                let tags: Vec<(u32, &str)> = vec![
                    (fix::TAG_MSG_TYPE, fix::MSG_MARKET_DATA_REQ),
                    (263, "2"),
                    (146, "1"),
                    (262, &req_id),
                    (6008, &con_id),
                    (207, venue),
                    (167, sec_type),
                    (264, &request_type),
                    (6088, "Socket"),
                    (9830, "1"),
                    (9839, precision),
                    (9887, &feed),
                ];
                attached_quotes::send_decorated(conn, &tags, combo);
            }
            hb.last_farm_sent = Instant::now();
        }
    }

    /// Withdraw a subscription's watch with it: the status and the frozen
    /// quotes, as they were asked for.
    pub(super) fn withdraw_status_watch(
        &mut self,
        instrument: InstrumentId,
        farm_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        let pairs = std::mem::take(&mut watch.pairs);
        let status = watch.status.take();
        self.withdraw_frozen_pairs(instrument, &pairs, farm_conn, hb);
        let Some(watch) = self.status_watches.remove(&instrument) else { return };
        let Some(req_id) = status else { return };
        self.md_req_to_instrument.retain(|(id, _)| *id != req_id);
        self.generic_tick_reqs.retain(|(id, _)| *id != req_id);
        let Some(conn) = farm_conn.as_mut() else { return };
        let (venue, sec_type) = stated_venue_and_type(&watch.sec_type, &watch.status_venue);
        let (req_id, con_id) = (req_id.to_string(), (watch.con_id as u32).to_string());
        let request_type = MARKET_DATA_STATUS_REQUEST_TYPE.to_string();
        let tags: Vec<(u32, &str)> = vec![
            (fix::TAG_MSG_TYPE, fix::MSG_MARKET_DATA_REQ),
            (263, "2"),
            (146, "1"),
            (262, &req_id),
            (6008, &con_id),
            (207, venue),
            (167, sec_type),
            (264, &request_type),
            (6088, "Socket"),
            (9830, "1"),
            (9839, "1"),
        ];
        attached_quotes::send_decorated(conn, &tags, self.attached_combo_quotes.get(&instrument));
        hb.last_farm_sent = Instant::now();
    }

    /// Serve the program the frozen quote, the delayed-frozen one, or neither,
    /// keeping up the one it was served while it is not, and serving it again
    /// as it stands once it is.
    fn show(
        &mut self,
        instrument: InstrumentId,
        frozen: bool,
        delayed_frozen: bool,
        context: &mut Context,
        shared: &SharedState,
    ) {
        let before = self.serves_its_own(instrument);
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        watch.shows_frozen = frozen;
        watch.shows_delayed_frozen = delayed_frozen;
        let after = self.serves_its_own(instrument);
        if !before && after {
            self.state_params_due(instrument, shared);
        }
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        let (quote, clock) = context.market.quote_and_clock_mut(instrument);
        if before && !after {
            watch.held = Some(Held { quote: *quote, clock: *clock, series: Vec::new() });
        } else if !before
            && after
            && let Some(held) = watch.held.take()
        {
            *quote = held.quote;
            *clock = held.clock;
            shared.market.push_quote(instrument, quote);
            for tick in held.series {
                shared.market.push_series_tick(tick);
            }
        }
    }

    /// A refusal of the quote the subscription asked for, as it bears on the
    /// watch. A gateway stops watching for the live quote on any refusal of
    /// it: the frozen quote asked for beside it is withdrawn without a word to
    /// the program, and the status with it unless the fallback to delayed data
    /// watches it, with delayed-frozen data on.
    pub(super) fn status_on_refusal(
        &mut self,
        instrument: InstrumentId,
        falls_back: bool,
        farm_conn: &mut Option<Connection>,
        context: &mut Context,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        watch.frozen = false;
        // The delayed quote is a record of its own, which the venue has
        // refused no frozen quote for.
        if falls_back {
            watch.refused.clear();
        }
        let keeps_status = falls_back && watch.delayed_frozen;
        if watch.in_frozen_state {
            self.leave_frozen_state(instrument, false, farm_conn, context, shared, hb);
        }
        if !keeps_status {
            self.withdraw_status_watch(instrument, farm_conn, hb);
        }
    }

    /// The fallback to delayed data, as it bears on the watch: the status is
    /// asked for where only the fallback watches it, and the delayed-frozen
    /// quote beside the delayed one where the market is closed.
    pub(super) fn status_on_fallback(
        &mut self,
        instrument: InstrumentId,
        farm_conn: &mut Option<Connection>,
        context: &mut Context,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        self.status_on_refusal(instrument, true, farm_conn, context, shared, hb);
        let Some(watch) = self.status_watches.get(&instrument) else { return };
        if watch.status.is_none() {
            self.ask_status(instrument, farm_conn, hb);
            return;
        }
        let closed = watch.closed;
        self.apply_status(instrument, closed, farm_conn, context, shared, hb);
    }

    /// Note the number the venue answers a frozen quote under.
    pub(super) fn note_frozen_tag(
        &mut self,
        instrument: InstrumentId,
        req_id: u32,
        server_tag: u32,
    ) {
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return };
        if let Some(pair) = watch.pairs.iter_mut().find(|pair| pair.requests.contains(&req_id)) {
            pair.tags.retain(|tag| *tag != server_tag);
            pair.tags.push(server_tag);
        }
    }

    /// Whether a refusal is of the status or of a frozen quote, which nothing
    /// is said of to the program: a frozen quote refused is not asked for
    /// again.
    pub(super) fn frozen_refusal(&mut self, instrument: InstrumentId, req_id: u32) -> bool {
        let Some(watch) = self.status_watches.get_mut(&instrument) else { return false };
        if watch.status == Some(req_id) {
            return true;
        }
        let Some(pair) = watch.pairs.iter_mut().find(|pair| pair.requests.contains(&req_id)) else {
            return false;
        };
        pair.refused.push(req_id);
        if !watch.refused.contains(&pair.feed) {
            watch.refused.push(pair.feed);
        }
        true
    }

    /// What a quote entry under `server_tag` is to the program, where the
    /// contract's market status is watched. The first delayed-frozen quote
    /// after the market closed is served, and the delayed one no longer.
    pub(super) fn frozen_route(
        &mut self,
        instrument: InstrumentId,
        server_tag: u32,
        context: &mut Context,
        shared: &SharedState,
    ) -> Route {
        let Some(watch) = self.status_watches.get(&instrument) else { return Route::Serve };
        let feed =
            watch.pairs.iter().find(|pair| pair.tags.contains(&server_tag)).map(|pair| pair.feed);
        match feed {
            None if self.serves_its_own(instrument) => Route::Serve,
            None => Route::Hold,
            Some(FROZEN_FEED) if watch.shows_frozen => Route::Serve,
            Some(DELAYED_FROZEN_FEED) if watch.shows_delayed_frozen => Route::Serve,
            Some(DELAYED_FROZEN_FEED) if watch.armed => {
                if let Some(watch) = self.status_watches.get_mut(&instrument) {
                    watch.armed = false;
                }
                self.show(instrument, false, true, context, shared);
                state_data_type(instrument, 4, shared);
                Route::Serve
            }
            Some(_) => Route::Drop,
        }
    }

    /// The quote kept up while it is not served, where one is.
    pub(super) fn held_quote(
        &mut self,
        instrument: InstrumentId,
    ) -> Option<(&mut Quote, &mut TradeClock)> {
        let held = self.status_watches.get_mut(&instrument)?.held.as_mut()?;
        Some((&mut held.quote, &mut held.clock))
    }

    /// Keep a series reading of the quote kept up, the last of each kind.
    pub(super) fn hold_series(&mut self, instrument: InstrumentId, tick: SeriesTick) {
        let Some(held) =
            self.status_watches.get_mut(&instrument).and_then(|watch| watch.held.as_mut())
        else {
            return;
        };
        held.series.retain(|kept| kept.tick_type != tick.tick_type);
        held.series.push(tick);
    }

    /// Start every watch again after the connection is lost: the venue's
    /// numbers go with it, and a reconnect starts live.
    pub(super) fn reset_status_watches(&mut self, shared: &SharedState) {
        for (instrument, watch) in self.status_watches.iter_mut() {
            if watch.shows_frozen || watch.shows_delayed_frozen {
                state_data_type(*instrument, 1, shared);
            }
            watch.status = None;
            watch.closed = false;
            watch.in_frozen_state = false;
            watch.pairs.clear();
            watch.refused.clear();
            watch.shows_frozen = false;
            watch.shows_delayed_frozen = false;
            watch.armed = false;
            watch.held = None;
        }
    }
}

/// State the type of data a subscription is served, to its watchers and to
/// any that join it.
fn state_data_type(instrument: InstrumentId, data_type: i32, shared: &SharedState) {
    shared
        .market
        .subscription_data_type(instrument, data_type)
        .store(data_type, std::sync::atomic::Ordering::Relaxed);
    shared.market.push_market_data_type(instrument, data_type);
}
