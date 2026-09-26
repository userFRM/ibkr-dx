//! Market data request/cancel methods and quote accessors.

use crate::types::*;
use crate::error_codes::Refusal;

use super::{wire_req_id, Contract, EClient};

impl EClient {
    // ── Market Data ──

    /// Ask the venue to scan an underlying for strategies worth putting on.
    ///
    /// The scan goes out beside a subscription for the series the venue states
    /// its answer on, because that is how it is asked for: the series carries
    /// the answer and the scan tells the venue what to look for. Read the
    /// answer with [`Self::scanned_strategies`] under the same request.
    ///
    /// The documented API has no call for this at all. What the scan states
    /// about each strategy is the venue's own, in the venue's own words, and
    /// nothing here translates them.
    pub fn req_spread_scan(
        &self, req_id: i64, contract: &Contract, scan: &crate::types::SpreadScan,
    ) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            let con_id = if scan.under_con_id > 0 { scan.under_con_id } else { contract.con_id };
            if con_id <= 0 {
                return Err(Refusal::stated(
                    321, "a spread scan names the contract to scan by the venue's id for it",
                ));
            }
            let mut scan = scan.clone();
            scan.under_con_id = con_id;
            // The scan rides its own request, so two scans of one contract each
            // state their own; the engine sends one at a time.
            let mode = self.core.subscription_mode();
            self.ask_for_mkt_data(req_id, contract, "481", false, false, mode, Some(scan.stated()), None, true)
        })() {
            self.refuse_request(req_id, &why);
        }
    }


    /// The strategies a spread scan stated for a request, as the venue stated
    /// them.
    ///
    /// Empty until a scan has been asked for and answered. A scan the venue
    /// refuses answers with nothing rather than with strategies.
    pub fn scanned_strategies(&self, req_id: i64) -> Vec<crate::types::ScannedStrategy> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.scanned_strategies(instrument)
    }

    /// Subscribe to market data. Matches `reqMktData` in C++.
    /// When `snapshot` is true, the quote is delivered as it arrives, and
    /// `tick_snapshot_end` follows once the snapshot is whole, as a gateway
    /// ends one: when the venue has stated the bid, the ask, the last, the open
    /// and the close; on a contract of a type a gateway marks as an option
    /// (`OPT`, `FOP`, `IOPT`, `WAR`, `EC`) also the option model, 13 (83
    /// delayed); on a delayed feed also the last trade's time, 88; or eleven
    /// seconds after the request, whichever comes first. The subscription is
    /// then withdrawn. That is a subscription this client ends, not a request
    /// of its own: the venue's own one-shot snapshot is the chargeable one,
    /// asked for with `regulatory_snapshot` on
    /// [`req_mkt_data_ex`](EClient::req_mkt_data_ex).
    ///
    /// Ticks 10, 11 and 12 (80, 81 and 82 delayed), the bid's, the ask's and
    /// the last's option computations, are not stated by the venue: a gateway
    /// works them out with an option model of its own, and so does this
    /// client, for an option on a share once its inputs are in hand. A
    /// gateway's snapshot of an option also waits for them; this client's
    /// does not.
    ///
    /// `generic_tick_list` goes out with the subscription, and what comes back
    /// reaches the caller on the callback the series belongs to. These are
    /// read:
    ///
    /// * `100`, `101`, `105` — option volume, open interest and the average
    ///   volume, calls before puts.
    /// * `104`, `106`, `411` — volatility: the historical figure, the implied
    ///   one, and the one the venue restrikes through the session.
    /// * `162`, `165` — an index's premium over its future; the extremes of
    ///   the last quarter, half-year and year with the ordinary day's volume.
    /// * `220`, `221`, `232`, `619` — the mark the venue keeps, which is not a
    ///   trade, under each of the numbers it is asked for by, and the slow one
    ///   beside it.
    /// * `225` — the auction: what is crossing, which way, at what price, and
    ///   the imbalance the venue must publish.
    /// * `233`, `375` — everything that traded, and what traded on a trade
    ///   report, each stated as a trade rather than as the totals it is read
    ///   from.
    /// * `236` — whether it can be borrowed, and how much of it.
    /// * `258` (or `47`) — the company ratios, as the venue writes them.
    /// * `292` — news for the contract, from the providers the session names;
    ///   `292:BRFG+DJNL` names the providers to ask instead. A contract's
    ///   headlines are asked for once, by the first request that wants them.
    /// * `293`, `294`, `295` — how fast it is trading.
    /// * `318` — what last traded in the regular session.
    /// * `456` (or `59`) — what it pays out.
    /// * `460` — the factor a redemption changes.
    /// * `499` — what it costs to borrow.
    /// * `577`, `614`, `623` — a fund's value per share: last, the day's
    ///   extremes, and the frozen one.
    /// * `586` — what a share is expected to open at, and what it did.
    /// * `588` — a future's open interest.
    /// * `595` — what has traded over the last three, five and ten minutes.
    /// * `787` — the odd lot: the two prices nobody has to deal in round lots
    ///   at, their sizes, and where each is quoted.
    ///
    /// A code outside that list still goes to the venue, and a reading of it
    /// arrives and is recorded rather than delivered: the shape it is written
    /// in is the series' own, and nothing here can read one it has not been
    /// taught.
    ///
    /// `tick_generic` also fires for the halt the venue states on its own tick:
    /// tick 49, 0 while a contract is trading and 1 once it has stopped.
    ///
    /// Every type starts live. Types 3 and 4 switch to delayed data after a
    /// bid/ask refusal says delayed data is available, which reports 10167
    /// without ending the request. Types 2 and 4 are served the frozen or the
    /// delayed-frozen quote while the market is closed, where the logon
    /// enables frozen data. `market_data_type` names the feed served.
    /// `req_mkt_data_ex` selects its feed directly.
    pub fn req_mkt_data(
        &self, req_id: i64, contract: &Contract,
        generic_tick_list: &str, snapshot: bool, regulatory_snapshot: bool,
    ) {
        if let Err(why) = self.try_req_mkt_data(req_id, contract, generic_tick_list, snapshot, regulatory_snapshot) {
            self.refuse_request(req_id, &why);
        }
    }

    /// [`req_mkt_data`](Self::req_mkt_data), with its refusal handed back to the
    /// caller rather than pushed into the session's order.
    pub(crate) fn try_req_mkt_data(
        &self, req_id: i64, contract: &Contract,
        generic_tick_list: &str, snapshot: bool, regulatory_snapshot: bool,
    ) -> Result<(), Refusal> {
        // The mode the caller asked for on `req_market_data_type`, which names
        // the type once for every subscription that follows. `req_mkt_data_ex`
        // states it per request instead.
        let mode = self.core.subscription_mode();
        if self.session_over() { return Err(Refusal::not_connected("Not connected")); }
        self.ask_for_mkt_data(req_id, contract, generic_tick_list, snapshot, regulatory_snapshot, mode, None, None, true)
    }

    /// Like [`req_mkt_data`](EClient::req_mkt_data), but names the market-data
    /// mode on the request itself, through FIX field 9887, rather than taking
    /// the one the session is set to:
    ///
    /// | `mode_9887` | mode             | wire shape |
    /// |-------------|------------------|---|
    /// | `0`         | REALTIME         | `264=442` (BID_ASK) + `264=443` (LAST), no 9887 |
    /// | `1`         | DELAYED          | `264=442` + `264=443`, each with `9887=1` |
    /// | `2`         | FROZEN           | `264=442` + `264=443`, each with `9887=2` |
    /// | `3`         | DELAYED_FROZEN   | `264=442` + `264=443`, each with `9887=3` |
    ///
    /// The frozen mode keeps thinly-traded names quoting after-hours, when the
    /// realtime feed is silent.
    ///
    /// A contract holds one subscription at a time, so this states
    /// the mode for that subscription rather than adding a parallel one — to
    /// compare modes on one contract, cancel between them. To set the mode for
    /// every subscription instead of naming it per request, call
    /// `req_market_data_type`.
    ///
    /// `regulatory_snapshot` asks for the venue's own chargeable one-shot
    /// snapshot: a request type of its own rather than a mode on an ordinary
    /// quote, asked for under the snapshot action and with no feed named
    /// beside it. It needs the entitlement — an account without it is
    /// refused by the venue, which names the request type back. Whether it
    /// also costs something is between the account and the broker, and is not
    /// on this wire. It ends the way an ordinary snapshot does, so a caller hears
    /// `tick_snapshot_end` either way. Its default is false.
    #[allow(clippy::too_many_arguments)]
    pub fn req_mkt_data_ex(
        &self, req_id: i64, contract: &Contract,
        generic_tick_list: &str, snapshot: bool, regulatory_snapshot: bool,
        mode_9887: i32, mkt_data_options: &[crate::types::model::TagValue],
    ) {
        if let Err(why) = self.try_req_mkt_data_ex(req_id, contract, generic_tick_list, snapshot, regulatory_snapshot, mode_9887, mkt_data_options) {
            self.refuse_request(req_id, &why);
        }
    }

    /// [`req_mkt_data_ex`](Self::req_mkt_data_ex), with its refusal handed back to the
    /// caller rather than pushed into the session's order.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_req_mkt_data_ex(
        &self, req_id: i64, contract: &Contract,
        generic_tick_list: &str, snapshot: bool, regulatory_snapshot: bool,
        mode_9887: i32, mkt_data_options: &[crate::types::model::TagValue],
    ) -> Result<(), Refusal> {
        if self.session_over() {
            return Err(Refusal::not_connected("Not connected"));
        }
        if !mkt_data_options.is_empty() {
            crate::client_core::ClientCore::check_option_list(
                &crate::client_core::MKT_DATA_OPTIONS,
                &crate::client_core::ClientCore::written_options(mkt_data_options),
                &self.shared.reference.enabled_features(),
            )?;
        }
        self.ask_for_mkt_data(req_id, contract, generic_tick_list, snapshot, regulatory_snapshot, mode_9887, None, None, false)
    }

    /// Hand a market-data request to the engine, which names the contract
    /// where the caller described it or gave only its id, registers it, and
    /// serves the request: nothing waits here. What it is served on, or why it
    /// is not, stands in the session's order.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ask_for_mkt_data(
        &self, req_id: i64, contract: &Contract,
        generic_tick_list: &str, snapshot: bool, regulatory_snapshot: bool,
        mode_9887: i32, spread_scan: Option<String>,
        calculation: Option<Box<crate::types::Calculation>>,
        delayed_allowed: bool,
    ) -> Result<(), Refusal> {
        // A number every other request refuses is refused here too: one in the
        // ranges this client and its engine number their own work in is
        // answered to nobody. A negative one is read as a gateway reads it.
        if req_id >= 0 {
            wire_req_id(req_id)?;
        }
        self.core.register_mkt_data(
            &self.shared, &self.control_tx, req_id,
            contract.con_id, &contract.symbol, &contract.exchange, &contract.sec_type,
            &contract.currency, &contract.lookup_filters(),
            snapshot, regulatory_snapshot, generic_tick_list, mode_9887,
            spread_scan, calculation, delayed_allowed,
        )
    }

    /// Cancel market data. Matches `cancelMktData` in C++.
    pub fn cancel_mkt_data(&self, req_id: i64) {
        if let Err(why) = self.try_cancel_mkt_data(req_id) {
            self.refuse_request(req_id, &why);
        }
    }

    /// [`cancel_mkt_data`](Self::cancel_mkt_data), with its refusal handed back to the
    /// caller rather than pushed into the session's order.
    pub(crate) fn try_cancel_mkt_data(&self, req_id: i64) -> Result<(), Refusal> {
        // A stream opened by `watch` holds its id as this client's own for as
        // long as it runs. Withdrawing it is where that ends. An id the caller
        // chose was never held, and releasing one that was not held does
        // nothing.
        self.shared.reference.forget_ours(crate::bridge::RecordKind::Answer, req_id);
        // Whatever this number was registered as is over from here, whether
        // or not the engine's record of it has been read yet.
        self.core.withdrawing(req_id);
        // The engine took the request before this, so it decides whether
        // there is one to withdraw — refusing a number that watches nothing,
        // as a caller branching on it is owed — and what goes to the venue:
        // the quotes stay up for another caller while the headlines and the
        // series only this one asked for stop.
        self.send(ControlCommand::CancelMktData { req_id })
    }


    /// Subscribe to every trade or every quote change on a contract.
    ///
    /// The feed rides the historical farm, registered there under the name
    /// `TickByTick` beside the five-second bars. No separate service is
    /// involved. A missing entitlement arrives as the venue's refusal
    /// rather than as silence.
    ///
    /// `number_of_ticks` and `ignore_size` are sent as stated: a count of past
    /// ticks goes out as the length of the run the stream opens with, and the
    /// size filter as the query's filter term. Neither goes out at its default
    /// — no prelude, sizes included — which is what the venue does on its own.
    /// Whether the venue honours the size filter is the venue's, and what it
    /// sends is passed on as it stands, as a gateway passes it: one session
    /// saw size-only changes still arrive on a stream that asked to leave
    /// them out.
    pub fn req_tick_by_tick_data(
        &self, req_id: i64, contract: &Contract, tick_type: &str,
        number_of_ticks: i32, ignore_size: bool,
    ) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month)?;
            // The only request surface that did not check the number it was given.
            // Unchecked, it was narrowed further down instead, so a caller
            // numbering its requests from the order counter — which the venue lets
            // run past what a request id can hold — had this stream's refusals
            // reported against somebody else's request.
            let _ = wire_req_id(req_id)?;
            let kind = TbtType::named(tick_type)?;
            // A stream is asked for by the venue's id for the contract, and states
            // what the contract is and where it trades: the engine names one the
            // caller described, or gave by id alone, before it asks.
            self.core
                .register_tbt(
                    &self.shared,
                    &self.control_tx,
                    req_id,
                    contract.into(),
                    contract.lookup_filters(),
                    kind,
                    number_of_ticks.max(0) as u32,
                    ignore_size,
                )?;
            Ok(())
        })() {
            self.refuse_request(req_id, &why);
        }
    }



    /// Cancel tick-by-tick data. Matches `cancelTickByTickData` in C++.
    pub fn cancel_tick_by_tick_data(&self, req_id: i64) {
        // The engine took the stream before this, so it decides whether there
        // is one to withdraw, and refuses a number that carries none, as a
        // caller branching on it is owed.
        if let Err(why) = self.send(ControlCommand::UnsubscribeTbt { req_id }) {
            self.refuse_request(req_id, &why);
        }
    }


    // ── Market Depth ──

    /// Subscribe to market depth (L2 order book). Matches `reqMktDepth` in C++.
    ///
    /// Refused as a gateway refuses it, before anything is sent: a contract
    /// naming no exchange, a combination, and a book of no rows. A contract
    /// that names no security type is sent as it stands, and the engine checks
    /// a named one against the venue's routing table. Substituting a stock here
    /// asks for a future's book as a stock's, which the venue refuses as a book
    /// it does not serve.
    pub fn req_mkt_depth(
        &self, req_id: i64, contract: &Contract,
        num_rows: i32, is_smart_depth: bool,
    ) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            // The number is checked before the book slot is taken: a number the
            // wire cannot carry holds nothing, and taking the slot first left it
            // held against a request that was then refused.
            let wire = wire_req_id(req_id)?;
            // No session is said before anything about the request, as the
            // reference client says it and as the other surface does.
            if self.session_over() {
                return Err(Refusal::not_connected("Not connected"));
            }
            crate::client_core::ClientCore::validate_depth_request(&contract.exchange, &contract.sec_type, num_rows, &contract.last_trade_date_or_contract_month)?;
            // A book rides the quote feed, so a feed the engine has given up on
            // serves none. Accepted, the request took a book slot and reached a
            // sender with no connection to write it to, which is silent — and a
            // book that never arrives is what a market with nothing to say looks
            // like, so nothing distinguished the two.
            if let Some(why) = self.shared.market.market_data_over() {
                return Err(Refusal::not_connected(format!(
                    "market data is unavailable for the rest of this session: {why}",
                )));
            }
            self.core.hold_the_book(req_id, &self.shared)?;
            self.send(ControlCommand::SubscribeDepth {
                contract: ContractRef {
                    con_id: contract.con_id,
                    symbol: contract.symbol.clone(),
                    exchange: contract.exchange.clone(),
                    sec_type: contract.sec_type.clone(),
                    currency: contract.currency.clone(),
                    ..Default::default()
                },
                req_id: wire,
                filters: contract.lookup_filters(),
                num_rows,
                is_smart_depth,
            })
        })() {
            self.refuse_request(req_id, &why);
        }
    }


    /// Cancel market depth. Matches `cancelMktDepth` in C++.
    pub fn cancel_mkt_depth(&self, req_id: i64) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            let wire = wire_req_id(req_id)?;
            // A caller withdrawing a book this client does not hold branches on
            // being told so, under the number the catalogue gives depth rather
            // than the one a quote subscription is withdrawn under.
            self.core.release_the_book(req_id, &self.shared)?;
            self.send(ControlCommand::UnsubscribeDepth { req_id: wire })
        })() {
            self.refuse_request(req_id, &why);
        }
    }


    // ── Real-Time Bars ──

    /// Subscribe to real-time 5-second bars. Matches `reqRealTimeBars` in C++.
    ///
    /// `bar_size` has no effect, as on a gateway: a real-time bar is five
    /// seconds, and the venue's request carries no bar size. A gateway reads
    /// the number and does not use it.
    ///
    /// Requests for the same bars of one contract — this call's, or the stream
    /// a historical request kept up to date rides — read one stream, as on a
    /// gateway: the venue serves it under one number, each request is handed
    /// every bar under its own id, and a cancel withdraws its own request
    /// alone. The stream is withdrawn when the last of them leaves.
    pub fn req_real_time_bars(
        &self, req_id: i64, contract: &Contract,
        _bar_size: i32, what_to_show: &str, use_rth: bool,
    ) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month)?;
            // Refused here rather than turned into trades on the way out: a
            // misspelled "BID" answered with trade bars looks like data.
            crate::control::historical::BarDataType::from_api_str(what_to_show)?;
            self.send(ControlCommand::SubscribeRealTimeBar {
                contract: contract.into(),
                req_id: wire_req_id(req_id)?,
                filters: contract.lookup_filters(),
                what_to_show: what_to_show.into(),
                use_rth,
            })
        })() {
            self.refuse_request(req_id, &why);
        }
    }


    /// Cancel real-time bars. Matches `cancelRealTimeBars` in C++.
    pub fn cancel_real_time_bars(&self, req_id: i64) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            self.send(ControlCommand::CancelRealTimeBar { req_id: wire_req_id(req_id)? })
        })() {
            self.refuse_request(req_id, &why);
        }
    }


    /// Request an auth-connection round-trip time sample: sends a
    /// lightweight liveness probe with no side effects on subscriptions,
    /// contract caches, or pacing budgets. The result lands asynchronously —
    /// poll `last_rtt()` after a moment. No-op while a probe is already in
    /// flight or the connection is down.
    pub fn req_ping(&self) {
        if let Err(why) = self.send(ControlCommand::Ping) {
            self.refuse_session(&why);
        }
    }


    /// Last measured auth-connection round-trip time, if any.
    /// A gauge, not a benchmark: the sample is the interval from a probe to
    /// the first inbound traffic that followed it, which on an active feed
    /// can undercount by racing data already in flight. Also sampled
    /// automatically whenever liveness sends its own probe.
    pub fn last_rtt(&self) -> Option<std::time::Duration> {
        self.shared.last_ccp_rtt()
    }

    /// Which feeds the subscriptions after this one may be served: 1 live, 2
    /// frozen, 3 delayed, 4 delayed and frozen.
    ///
    /// A type turns feeds on, as a gateway takes it: 2 turns frozen data on;
    /// 3 and 4 turn delayed data on, 4 with delayed-frozen and 3 without; only
    /// 1 turns frozen data off, and it turns all three off. A subscription
    /// starts live whatever the type, falls back to delayed data on a refusal
    /// where delayed data is on, and is served the frozen or delayed-frozen
    /// quote while the market is closed, where the logon enables frozen data;
    /// the `market_data_type` callback reports the type served. To name a feed
    /// for one request instead, [`req_mkt_data_ex`](EClient::req_mkt_data_ex)
    /// takes it. A number naming no type leaves the feeds as they were, and
    /// says so.
    pub fn req_market_data_type(&self, market_data_type: i32) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        self.core.set_market_data_type(market_data_type);
    }

    /// Set news provider codes for per-contract news ticks.
    pub fn set_news_providers(&self, providers: &str) {
        self.core.set_news_providers(providers);
    }

    // ── Escape Hatch ──

    /// Zero-copy SeqLock quote read. Maps reqId → InstrumentId → SeqLock.
    /// Returns `None` if the reqId is not mapped to a subscription.
    #[inline]
    pub fn quote(&self, req_id: i64) -> Option<Quote> {
        let map = self.core.req_to_instrument.lock().unwrap();
        map.get(&req_id).map(|&iid| self.shared.market.quote(iid))
    }

    /// Direct SeqLock read by InstrumentId (for callers who track IDs themselves).
    /// Returns `None` for an id past every slot the instrument table holds.
    #[inline]
    pub fn quote_by_instrument(&self, instrument: InstrumentId) -> Option<Quote> {
        self.shared.market.try_quote(instrument)
    }

    /// What the venue's own model last made of an option, whole.
    ///
    /// [`Wrapper::tick_option_computation`] carries its greeks and its price,
    /// beside the volatility and the underlying's price a gateway takes from
    /// the venue's other series. The venue states eighteen figures on this
    /// record, and the ones the callback has no room for — its own volatility
    /// and underlying price, the rate greek, the expected time to exercise and
    /// the price that triggers it, the forward coefficient, two yields, the
    /// time value, the days the model counted, the rate it discounted at, and
    /// which kind of volatility it priced on — are on the record this returns.
    ///
    /// A figure the venue did not state is `f64::MAX`, as everywhere else on
    /// this record; zero is a real greek.
    ///
    /// `None` where the request names no subscription, or the venue has not
    /// stated a model for it yet.
    ///
    /// [`Wrapper::tick_option_computation`]: crate::api::Wrapper::tick_option_computation
    pub fn option_model(&self, req_id: i64) -> Option<crate::types::OptionComputation> {
        let instrument = *self.core.req_to_instrument.lock().unwrap().get(&req_id)?;
        self.shared.market.option_model(instrument)
    }

    /// Whether the venue is restricting short sales in the contract a request
    /// is watching.
    ///
    /// The circuit breaker a venue puts on a contract that has fallen far
    /// enough in a day, which stops a short from resting below the bid. The
    /// venue states it on the same record as the halt, and it has no field
    /// anywhere in the documented API. Not the same question as whether the
    /// contract can be borrowed, which [`Wrapper::tick_generic`] already
    /// answers beside it: a contract can be freely borrowable and still
    /// restricted.
    ///
    /// `false` where the request names no subscription, as it is for a
    /// contract the venue has not restricted.
    ///
    /// [`Wrapper::tick_generic`]: crate::api::Wrapper::tick_generic
    pub fn short_sale_restricted(&self, req_id: i64) -> bool {
        let held = self.core.req_to_instrument.lock().unwrap();
        held.get(&req_id)
            .is_some_and(|&iid| self.shared.market.short_sale_restricted(iid))
    }

    /// The same, by InstrumentId, for callers who track them themselves.
    pub fn short_sale_restricted_by_instrument(&self, instrument: InstrumentId) -> bool {
        self.shared.market.short_sale_restricted(instrument)
    }

    /// What the venue says about the contract itself, beside its prices.
    ///
    /// How many shares the company has on issue — the multiplier that turns a
    /// price into a market capitalisation — and what the contract opened at a
    /// year ago, which gives a trailing return without asking for a year of
    /// history. Both arrive on the tick that carries the price extremes, on
    /// every subscription that asks for them, and both were read past.
    ///
    /// The documented API reaches neither from a quote: a share count is a
    /// fundamentals request of its own there, and a year-ago open has no call
    /// at all. So they are read here rather than sent as ticks — a tick number
    /// of this client's own choosing, where a caller reads the venue's, is not
    /// something this client invents.
    ///
    /// `None` where the request names no subscription, or the venue has stated
    /// neither figure for it yet; `f64::MAX` for a figure it has not stated.
    pub fn contract_figures(&self, req_id: i64) -> Option<crate::types::ContractFigures> {
        let instrument = *self.core.req_to_instrument.lock().unwrap().get(&req_id)?;
        self.shared.market.contract_figures(instrument)
    }

    /// The same, by InstrumentId, for callers who track them themselves.
    pub fn contract_figures_by_instrument(
        &self, instrument: InstrumentId,
    ) -> Option<crate::types::ContractFigures> {
        self.shared.market.contract_figures(instrument)
    }

    /// What one of the venue's own series last stated for a subscription, in
    /// the order the series states it.
    ///
    /// The venue runs dozens of series the documented API has no call for: a
    /// bond's analytics, an option's model volatility, the volume a contract
    /// usually opens on, what margin a future takes. Ask for one by its own
    /// number in the generic tick list, and read what it stated here.
    ///
    /// The figures come as the venue stated them — its order, its widths — and
    /// a figure it holds nothing for comes as the largest number its field
    /// carries. Empty where the request names no subscription, or that series
    /// has stated nothing for it.
    ///
    /// Read here rather than sent as ticks for the same reason the contract
    /// figures are: a tick number of this client's own choosing, where a caller
    /// reads the venue's, is not something this client invents.
    pub fn stated_figures(&self, req_id: i64, series: u32) -> Vec<f64> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.stated_figures(instrument, series)
    }

    /// What one of the venue's numbered-table series last stated for a
    /// subscription: its figures under the venue's own numbering.
    ///
    /// Four series state their figures as two tables, whole numbers and
    /// fractional ones, each entry naming what it is before stating it. Two of
    /// those numbers have a documented call to arrive on and the rest have
    /// none; the rest are here, under the number the venue gave them.
    ///
    /// `fractional` picks the table. The venue numbers the two separately, so
    /// the same number in each is not the same figure.
    pub fn numbered_figures(
        &self, req_id: i64, series: u32, fractional: bool,
    ) -> Vec<(i32, f64)> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.numbered_figures(instrument, series, fractional)
    }

    /// The run of paired figures one series last stated for a subscription.
    ///
    /// Two series state their figures as a count and then that many pairs: the
    /// volatility the venue's own model puts on each point of a curve, and the
    /// weight it puts on each price a contract might reach. Neither has a
    /// documented call to arrive on.
    pub fn paired_figures(&self, req_id: i64, series: u32) -> Vec<(f64, f64)> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.paired_figures(instrument, series)
    }

    /// The rows of three figures one series last stated for a subscription.
    ///
    /// What the three are is the series' own:
    ///
    /// | Series | The three figures |
    /// | --- | --- |
    /// | 547 | A quantity, what it is offered at, and a second price where the form states one |
    /// | 491 | Which strategy the leg belongs to, the contract it names, and its size |
    /// | 320, 376, 530, 532 | The venue's number for a field of a packed quote, its figure, and how far that figure's decimal point moves |
    ///
    /// On the packed quotes the venue numbers its fields itself: nought and one
    /// are the two sides of the quote and four and five the size behind each,
    /// read off a live session. A side the venue is not standing behind reads
    /// as minus one hundred. Those figures are counted in the contract's own
    /// increments, as every packed figure is — 75815 against an increment of a
    /// hundredth is 758.15 — and no scale is put on them here, because which
    /// fields are prices and which are counts is the venue's to change.
    ///
    /// `f64::MAX` stands where a form states no third figure. The documented
    /// API has no call for any of these.
    pub fn stated_rows(&self, req_id: i64, series: u32) -> Vec<(f64, f64, f64)> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.stated_rows(instrument, series)
    }

    /// What one of the option model's chain series last stated for a
    /// subscription on an underlying.
    ///
    /// Per class of the underlying's options: the underlying's price, the
    /// dividends expected, and per expiry the yield, the interest rate, the
    /// forward and the at-the-money volatilities the model works from. Ask for
    /// series 687 for the standing set, or 691 for the set as the chain
    /// closed, in the generic tick list of a subscription on the underlying.
    ///
    /// The documented API has no call for either: a gateway reads them for its
    /// own option model and hands none of it on. Empty where the request names
    /// no subscription, or the series has stated nothing for it.
    pub fn chain_model_parameters(
        &self, req_id: i64, series: u32,
    ) -> Vec<crate::types::ChainModelParameters> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.chain_model_parameters(instrument, series)
    }

    /// Which series have stated rows for a subscription, in order.
    pub fn stated_rows_series(&self, req_id: i64) -> Vec<u32> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.stated_rows_series(instrument)
    }

    /// Which series have stated paired figures for a subscription, in order.
    pub fn paired_figures_series(&self, req_id: i64) -> Vec<u32> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.paired_figures_series(instrument)
    }

    /// Which series have stated numbered figures for a subscription, in order.
    pub fn numbered_figures_series(&self, req_id: i64) -> Vec<u32> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.numbered_figures_series(instrument)
    }

    /// Which series have stated figures for a subscription, in order.
    pub fn stated_figures_series(&self, req_id: i64) -> Vec<u32> {
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Vec::new();
        };
        self.shared.market.stated_figures_series(instrument)
    }

    /// What the venue states about a contract's company or its terms on one
    /// series, as the pairs it wrote.
    ///
    /// Seventeen series carry this text, each asked for by the venue's own
    /// number for it in the generic tick list: the analyst ratings, what
    /// institutions and insiders hold, the shares on issue and the float, fund
    /// terms, the screening scores, margin, the technical readings, the
    /// company's accounts and what it has coming.
    /// The keys are the venue's own and are handed on unchanged.
    ///
    /// Held against the venue's id for the contract rather than the request,
    /// because it is a fact about the contract and outlives the subscription
    /// that fetched it — so it is read by `con_id`, not by request number, and
    /// it is still there after the watch ends — for four thousand and
    /// ninety-six contracts, the one heard of longest ago making way.
    ///
    /// Empty where that series has stated nothing for the contract.
    pub fn company_data(&self, con_id: u32, series: u32) -> Vec<(String, String)> {
        self.shared.reference.company_data(con_id, series)
    }

    /// Which of those series have been stated for a contract, in order.
    pub fn company_data_series(&self, con_id: u32) -> Vec<u32> {
        self.shared.reference.company_data_series(con_id)
    }

    /// What the venue's model made of an option as it closed.
    ///
    /// The same model as [`Self::option_model`] and in the same shape — every
    /// greek it states, the ones the documented API has no field for included —
    /// but worked out as the contract closed rather than as it stands. The
    /// documented API has no call for it at all. Ask for it by the venue's own
    /// number for the series in the generic tick list.
    pub fn closing_option_model(&self, req_id: i64) -> Option<crate::types::OptionComputation> {
        let instrument = *self.core.req_to_instrument.lock().unwrap().get(&req_id)?;
        self.shared.market.closing_option_model(instrument)
    }

    /// The same, by InstrumentId, for callers who track them themselves.
    pub fn closing_option_model_by_instrument(
        &self, instrument: InstrumentId,
    ) -> Option<crate::types::OptionComputation> {
        self.shared.market.closing_option_model(instrument)
    }

    /// The same, by InstrumentId, for callers who track them themselves.
    pub fn option_model_by_instrument(
        &self, instrument: InstrumentId,
    ) -> Option<crate::types::OptionComputation> {
        self.shared.market.option_model(instrument)
    }
}
