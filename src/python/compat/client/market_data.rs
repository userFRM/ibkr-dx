//! Market data request/cancel methods.

use pyo3::prelude::*;
use crate::error_codes::Refusal;

use crate::types::*;
use super::{wire_req_id, EClient};
use super::super::contract::{Contract, SpreadScan};

#[pymethods]
impl EClient {
    /// Set news provider codes for per-contract news ticks (e.g. "BRFG*BRFUPDN").
    #[pyo3(signature = (providers))]
    fn set_news_providers(&self, providers: &str) {
        self.core.set_news_providers(providers);
    }

    /// Request market data for a contract.
    ///
    /// With `snapshot`, `tickSnapshotEnd` follows once the snapshot is whole,
    /// as a gateway ends one: when the venue has stated the bid, the ask, the
    /// last, the open and the close; on a contract of a type a gateway marks
    /// as an option (`OPT`, `FOP`, `IOPT`, `WAR`, `EC`) also the option model,
    /// 13 (83 delayed); on a delayed feed also the last trade's time, 88; or
    /// eleven seconds after the request. Ticks 10, 11 and 12 (80, 81 and 82
    /// delayed), the bid's, the ask's and the last's option computations, are
    /// not stated by the venue: a gateway works them out with an option model
    /// of its own, and so does this client, for an option on a share once its
    /// inputs are in hand. A gateway's snapshot of an option also waits for
    /// them; this client's does not.
    ///
    /// `mkt_data_options` is checked as a gateway checks it: `manual`, `0` or `1`, is
    /// taken and changes nothing a gateway sends; any other key is refused
    /// under 10337, another value under 10338, and an entry not written
    /// `key=value` under 320. Where the venue has lifted the key checks, a
    /// `manual` that does not read as the number nought or one is refused
    /// under 321.
    #[pyo3(signature = (req_id, contract, generic_tick_list="", snapshot=false, regulatory_snapshot=false, mkt_data_options=None))]
    pub(crate) fn req_mkt_data(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        generic_tick_list: &str,
        snapshot: bool,
        regulatory_snapshot: bool,
        mkt_data_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        if self.tx_or_report(req_id)?.is_none() { return Ok(()); }
        if let Some(why) = self.options_refused(py, &crate::client_core::MKT_DATA_OPTIONS, mkt_data_options)? {
            return self.report_refusal(py, req_id, why);
        }
        self.ask_for_mkt_data(py, req_id, contract, generic_tick_list, snapshot, regulatory_snapshot,
            self.core.subscription_mode(), None, None, true)
    }

    /// Like `req_mkt_data`, but names the market-data mode on the request
    /// itself (0=realtime, 1=delayed, 2=frozen, 3=delayed-frozen) rather than
    /// taking the one the session is set to. The frozen one keeps thinly-traded
    /// names quoting after hours, when the realtime feed is silent.
    ///
    /// A contract holds one subscription at a time, so this states the mode for
    /// that subscription rather than adding a second alongside it: a later
    /// request for a contract already subscribed follows the one that is up and
    /// is handed its quotes. To compare two modes on one contract, withdraw
    /// between them.
    ///
    /// `regulatory_snapshot` asks for the venue's own chargeable one-shot
    /// snapshot: a request type of its own rather than a mode on an ordinary
    /// quote. It needs the entitlement, and an
    /// account without it is refused by the venue, which names the request
    /// type back through `error`. It ends the way an ordinary snapshot does,
    /// so `tickSnapshotEnd` fires either way.
    #[pyo3(signature = (req_id, contract, generic_tick_list="", snapshot=false, regulatory_snapshot=false, mode_9887=0, mkt_data_options=None))]
    fn req_mkt_data_ex(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        generic_tick_list: &str,
        snapshot: bool,
        regulatory_snapshot: bool,
        mode_9887: i32,
        mkt_data_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        if self.tx_or_report(req_id)?.is_none() { return Ok(()); }
        if let Some(why) = self.options_refused(py, &crate::client_core::MKT_DATA_OPTIONS, mkt_data_options)? {
            return self.report_refusal(py, req_id, why);
        }
        self.ask_for_mkt_data(
            py, req_id, contract, generic_tick_list, snapshot, regulatory_snapshot, mode_9887,
            None, None, false,
        )
    }

    /// Cancel market data.
    pub fn cancel_mkt_data(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        // Whatever this number was registered as is over from here, whether
        // or not the engine's record of it has been read yet.
        self.core.withdrawing(req_id);
        // The engine took the request before this, so it decides whether
        // there is one to withdraw — refusing a number that watches nothing,
        // as a caller branching on it is owed — and what goes to the venue.
        self.withdraw_mkt_data(py, &tx, req_id)
    }

    /// Request tick-by-tick data.
    ///
    /// `number_of_ticks` and `ignore_size` are sent as stated: a count of past
    /// ticks goes out as the length of the run the stream opens with, and the
    /// size filter as the query's filter term. Neither goes out at its default
    /// — no prelude, sizes included — which is what the venue does on its own.
    /// Whether the venue honours the size filter is the venue's, and what it
    /// sends is passed on as it stands, as a gateway passes it: one session
    /// saw size-only changes still arrive on a stream that asked to leave
    /// them out.
    #[pyo3(signature = (req_id, contract, tick_type, number_of_ticks=0, ignore_size=false))]
    fn req_tick_by_tick_data(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        tick_type: &str,
        number_of_ticks: i32,
        ignore_size: bool,
    ) -> PyResult<()> {
        // The number this request will answer under, checked before anything
        // reaches the venue. Unchecked, it was narrowed further down instead,
        // so a caller numbering its requests from the order counter — which the
        // venue lets run past what a request id can hold — had this stream's
        // refusals reported against somebody else's request.
        wire_req_id(req_id)?;
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal(py, req_id, why);
        }

        let tbt_type = match TbtType::named(tick_type) {
            Ok(named) => named,
            // A tick type this client does not carry is a request it will not
            // send, which is what validation means.
            Err(why) => return self.report_refusal(py, req_id, Refusal::validation(why)),
        };

        // A stream is asked for by the venue's id for the contract, and states
        // what the contract is and where it trades: the engine names one the
        // caller described, or gave by id alone, before it asks.
        let shared = self.shared_state()?;
        if let Err(why) = self.core.register_tbt(
            &shared, &tx, req_id, contract.into(), contract.lookup_filters(), tbt_type,
            number_of_ticks.max(0) as u32, ignore_size,
        ) {
            return self.report_refusal(py, req_id, why);
        }
        Ok(())
    }

    /// Cancel tick-by-tick data.
    fn cancel_tick_by_tick_data(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(-1)? else { return Ok(()) };
        // The engine took the stream before this, so it decides whether there
        // is one to withdraw, and refuses a number that carries none.
        if let Err(why) = self.send_control(&tx, ControlCommand::UnsubscribeTbt { req_id }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request an auth-connection round-trip time sample: sends a
    /// lightweight liveness probe with no side effects on subscriptions,
    /// contract caches, or pacing budgets. Poll `last_rtt_ms()` after a
    /// moment for the result.
    fn req_ping(&self, py: Python<'_>) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(-1)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::Ping) {
            return self.report_refusal(py, -1, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Last measured auth-connection round-trip time in milliseconds, or
    /// None if never measured. A gauge, not a benchmark
    /// `req_ping`. Also sampled automatically by the engine's own liveness
    /// probes.
    fn last_rtt_ms(&self) -> PyResult<Option<f64>> {
        let shared = match self.shared.lock().unwrap().clone() {
            Some(s) => s,
            None => return Ok(None),
        };
        Ok(shared.last_ccp_rtt().map(|d| d.as_secs_f64() * 1_000.0))
    }

    /// Name the feeds every subscription after this one may be served:
    /// 1 live, 2 frozen, 3 delayed, 4 delayed-frozen.
    ///
    /// A type turns feeds on, as a gateway takes it: 2 turns frozen data on;
    /// 3 and 4 turn delayed data on, 4 with delayed-frozen and 3 without; only
    /// 1 turns frozen data off, and it turns all three off. A subscription
    /// starts live whatever the type, falls back to delayed data on a refusal
    /// where delayed data is on, and is served the frozen or delayed-frozen
    /// quote while the market is closed, where the logon enables frozen data;
    /// the `market_data_type` callback reports the type served. A type this
    /// client does not know is logged and leaves the feeds as they were.
    /// `req_mkt_data_ex` names a feed per request, which allows two feeds on
    /// one contract at once.
    fn req_market_data_type(&self, market_data_type: i32) -> PyResult<()> {
        // Answered under 504 with no session, as every request is, and the
        // type is then not kept. It used to be: set before `connect`, it
        // applied to the session that followed. The reference client's sends
        // and stores nothing, so a program written against it sets the type
        // after connecting, having never had another way; what a caller loses
        // here is only a setting the reference never let it make.
        let Some(_tx) = self.tx_or_report(-1)? else { return Ok(()) };
        self.core.set_market_data_type(market_data_type);
        Ok(())
    }

    /// Request market depth (L2 order book).
    ///
    /// `mkt_depth_options` is checked as a gateway checks it: `manual`, `0` or `1`, is
    /// taken and changes nothing a gateway sends; any other key is refused
    /// under 10337, another value under 10338, and an entry not written
    /// `key=value` under 320. Where the venue has lifted the key checks, a
    /// `manual` that does not read as the number nought or one is refused
    /// under 321.
    #[pyo3(signature = (req_id, contract, num_rows=5, is_smart_depth=false, mkt_depth_options=None))]
    fn req_mkt_depth(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        num_rows: i32,
        is_smart_depth: bool,
        mkt_depth_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        // As the caller stated it. The reference client sends a book request's
        // secType and exchange straight off the contract, so a contract naming
        // only an id was subscribed here to a US stock on SMART: a book for an
        // instrument nobody asked about, under their own request id.
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Some(why) = self.options_refused(py, &crate::client_core::MKT_DEPTH_OPTIONS, mkt_depth_options)? {
            return self.report_refusal(py, req_id, why);
        }
        // The number is checked before the book slot is taken: a number the
        // wire cannot carry holds nothing, and taking the slot first left it
        // held against a request that was then refused.
        let wire = wire_req_id(req_id)?;
        // What a gateway refuses before it looks the contract up.
        if let Err(why) = crate::client_core::ClientCore::validate_depth_request(
            &contract.exchange, &contract.sec_type, num_rows, &contract.last_trade_date_or_contract_month,
        ) {
            return self.report_refusal(py, req_id, why);
        }
        // A book rides the quote feed, so a feed given up on serves none. The
        // other surface refuses this and this one did not: a book asked for
        // here took a slot, reached a sender with no connection to write it to,
        // and said nothing — and a book that never arrives is what a market
        // with nothing to say looks like.
        // Read out from under the lock before anything is refused. The refusal
        // below reaches the caller's own error handler, and a handler that
        // disconnects takes this same lock on its way out — held across the
        // call, the two are one thread waiting on a lock it is already holding,
        // with the interpreter stopped behind it.
        let shared = self.shared_state()?;
        if let Some(why) = shared.market.market_data_over() {
            return self.report_refusal(
                py, req_id,
                Refusal::not_connected(format!(
                    "market data is unavailable for the rest of this session: {why}",
                )),
            );
        }
        if let Err(why) = self.core.hold_the_book(req_id, &shared) {
            return self.report_refusal(py, req_id, why);
        }
        if let Err(why) = self.send_control(&tx, ControlCommand::SubscribeDepth {
                contract: ContractRef { con_id: contract.con_id, symbol: contract.symbol.clone(), exchange: contract.exchange.clone(), sec_type: contract.sec_type.clone(), currency: contract.currency.clone(), ..Default::default() },
                req_id: wire,
                num_rows,
                is_smart_depth,
                filters: contract.lookup_filters(),
            }) {
            // The slot goes back with it. Kept, the number stayed held
            // against a request the venue never heard, and the caller's
            // retry under it was refused as a duplicate of that one until
            // the session was rebuilt.
            let _ = self.core.release_the_book(req_id, &shared);
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel market depth.
    ///
    /// `is_smart_depth` has no effect: a book is withdrawn by the request that
    /// asked for it, and this client remembers which kind that was. Stated as
    /// the book was asked for, it withdraws the same book a gateway would.
    #[pyo3(signature = (req_id, is_smart_depth=false))]
    fn cancel_mkt_depth(&self, py: Python<'_>, req_id: i64, is_smart_depth: bool) -> PyResult<()> {
        let _ = is_smart_depth;
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        // A caller withdrawing a book this client does not hold branches on
        // being told so, under the number the catalogue gives depth rather
        // than the one a quote subscription is withdrawn under.
        let wire = wire_req_id(req_id)?;
        if let Err(why) = self.core.release_the_book(req_id, &*self.shared_state()?) {
            return self.report_refusal(py, req_id, why);
        }
        if let Err(why) = self.send_control(&tx, ControlCommand::UnsubscribeDepth { req_id: wire }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request real-time 5-second bars.
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
    ///
    /// `real_time_bars_options` is checked as a gateway checks it: `manual`, `0` or `1`, is
    /// taken and changes nothing a gateway sends; any other key is refused
    /// under 10337, another value under 10338, and an entry not written
    /// `key=value` under 320. Where the venue has lifted the key checks, a
    /// `manual` that does not read as the number nought or one is refused
    /// under 321.
    #[pyo3(signature = (req_id, contract, bar_size=5, what_to_show="TRADES", use_rth=0, real_time_bars_options=None))]
    fn req_real_time_bars(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        bar_size: i32,
        what_to_show: &str,
        use_rth: i32,
        real_time_bars_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal(py, req_id, why);
        }
        let _ = bar_size;
        if let Some(why) = self.options_refused(py, &crate::client_core::REAL_TIME_BARS_OPTIONS, real_time_bars_options)? {
            return self.report_refusal(py, req_id, why);
        }
        if let Err(why) = crate::control::historical::BarDataType::from_api_str(what_to_show) {
            return self.report_refusal(py, req_id, why.into());
        }
        let wire = wire_req_id(req_id)?;
        if let Err(why) = self.send_control(&tx, ControlCommand::SubscribeRealTimeBar {
                contract: contract.into(),
                req_id: wire,
                what_to_show: what_to_show.to_string(),
                use_rth: use_rth != 0,
                filters: contract.lookup_filters(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel real-time bars.
    fn cancel_real_time_bars(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelRealTimeBar { req_id: wire_req_id(req_id)? }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    // ── Quote Access ──

    /// Zero-copy SeqLock quote read by req_id.
    /// Returns a dict with bid, ask, last, bid_size, ask_size, last_size, volume,
    /// high, low, open, close, or None if the req_id is not mapped.
    fn quote(&self, req_id: i64) -> PyResult<Option<Py<PyAny>>> {
        let shared = self.shared_state()?;
        let map = self.core.req_to_instrument.lock().unwrap();
        let iid = match map.get(&req_id) {
            Some(&iid) => iid,
            None => return Ok(None),
        };
        drop(map);
        let q = shared.market.quote(iid);
        Python::attach(|py| {
            let ps = super::super::super::types::PRICE_SCALE_F;
            let qs = crate::types::QTY_SCALE as f64;
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("bid", q.bid as f64 / ps)?;
            dict.set_item("ask", q.ask as f64 / ps)?;
            dict.set_item("last", q.last as f64 / ps)?;
            dict.set_item("bid_size", q.bid_size as f64 / qs)?;
            dict.set_item("ask_size", q.ask_size as f64 / qs)?;
            dict.set_item("last_size", q.last_size as f64 / qs)?;
            dict.set_item("volume", q.volume as f64 / qs)?;
            dict.set_item("high", q.high as f64 / ps)?;
            dict.set_item("low", q.low as f64 / ps)?;
            dict.set_item("open", q.open as f64 / ps)?;
            dict.set_item("close", q.close as f64 / ps)?;
            // What the venue says about dealing in this contract at all, which
            // is what every price above means or does not mean. Left out, a
            // caller reading a quote through this surface read the last price
            // before a halt as a market it could deal on.
            dict.set_item("halted", q.halted)?;
            Ok(Some(dict.into_any().unbind()))
        })
    }

    /// Zero-copy SeqLock quote read by InstrumentId.
    /// Returns a dict with bid, ask, last, bid_size, ask_size, last_size,
    /// volume, high, low, open, close, and whether the venue has halted the
    /// contract and why — or None if not connected, or for an id past every
    /// slot the instrument table holds. Whether it is restricting
    /// short sales in it is `shortSaleRestrictedByInstrument`, which is stated
    /// on the same record and kept off this one.
    fn quote_by_instrument(&self, instrument: u32) -> PyResult<Option<Py<PyAny>>> {
        let shared = match self.shared.lock().unwrap().clone() {
            Some(s) => s,
            None => return Ok(None),
        };
        // Out-of-range id: None, not a cross-language panic.
        let Some(q) = shared.market.try_quote(instrument) else {
            return Ok(None);
        };
        Python::attach(|py| {
            let ps = super::super::super::types::PRICE_SCALE_F;
            let qs = crate::types::QTY_SCALE as f64;
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("bid", q.bid as f64 / ps)?;
            dict.set_item("ask", q.ask as f64 / ps)?;
            dict.set_item("last", q.last as f64 / ps)?;
            dict.set_item("bid_size", q.bid_size as f64 / qs)?;
            dict.set_item("ask_size", q.ask_size as f64 / qs)?;
            dict.set_item("last_size", q.last_size as f64 / qs)?;
            dict.set_item("volume", q.volume as f64 / qs)?;
            dict.set_item("high", q.high as f64 / ps)?;
            dict.set_item("low", q.low as f64 / ps)?;
            dict.set_item("open", q.open as f64 / ps)?;
            dict.set_item("close", q.close as f64 / ps)?;
            // What the venue says about dealing in this contract at all, which
            // is what every price above means or does not mean. Left out, a
            // caller reading a quote through this surface read the last price
            // before a halt as a market it could deal on.
            dict.set_item("halted", q.halted)?;
            Ok(Some(dict.into_any().unbind()))
        })
    }

    /// What the venue's own model last made of an option, whole.
    ///
    /// `tickOptionComputation` carries its greeks and its price, beside the
    /// volatility and the underlying's price a gateway takes from the venue's
    /// other series. The venue states eighteen figures on this record, and
    /// every one is in this dict, its own volatility and underlying price among
    /// them. A figure the venue did not state is this API's own unset double;
    /// zero is a real greek.
    ///
    /// `None` where the request names no subscription, or the venue has not
    /// stated a model for it yet.
    #[pyo3(signature = (req_id))]
    fn option_model(&self, req_id: i64) -> PyResult<Option<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(None) };
        let Some(instrument) =
            self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(None);
        };
        self.option_model_dict(&shared, instrument)
    }

    /// Whether the venue is restricting short sales in the contract a request
    /// is watching.
    ///
    /// The circuit breaker a venue puts on a contract that has fallen far
    /// enough in a day, which stops a short from resting below the bid. Stated
    /// on the same record as the halt, and with no field anywhere in the
    /// documented API. Not the same question as whether the contract can be
    /// borrowed, which ticks 46 and 89 already answer beside it.
    #[pyo3(signature = (req_id))]
    fn short_sale_restricted(&self, req_id: i64) -> PyResult<bool> {
        let Ok(shared) = self.shared_state() else { return Ok(false) };
        let held = self.core.req_to_instrument.lock().unwrap();
        Ok(held.get(&req_id).is_some_and(|&iid| shared.market.short_sale_restricted(iid)))
    }

    /// The same, by InstrumentId, for callers who track them themselves.
    #[pyo3(signature = (instrument))]
    fn short_sale_restricted_by_instrument(&self, instrument: u32) -> PyResult<bool> {
        let Ok(shared) = self.shared_state() else { return Ok(false) };
        Ok(shared.market.short_sale_restricted(instrument))
    }

    /// What the venue says about the contract itself, beside its prices: how
    /// many shares are on issue, and what it opened at a year ago.
    ///
    /// Both arrive on the tick carrying the price extremes and neither has a
    /// tick of its own in the documented API — a share count is a fundamentals
    /// request there, and a year-ago open has no call at all.
    #[pyo3(signature = (req_id))]
    fn contract_figures(&self, req_id: i64) -> PyResult<Option<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(None) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(None);
        };
        Self::contract_figures_dict(&shared, instrument)
    }

    /// The same, by InstrumentId, for callers who track them themselves.
    #[pyo3(signature = (instrument))]
    fn contract_figures_by_instrument(&self, instrument: u32) -> PyResult<Option<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(None) };
        Self::contract_figures_dict(&shared, instrument)
    }

    /// What one of the venue's own series last stated for a subscription, in
    /// the order the series states it.
    ///
    /// The venue runs dozens of series the documented API has no call for: a
    /// bond's analytics, an option's model volatility, the volume a contract
    /// usually opens on, what margin a future takes. Ask for one by its own
    /// number in the generic tick list, and read what it stated here. A figure
    /// the venue holds nothing for comes as the largest number its field
    /// carries.
    #[pyo3(signature = (req_id, series))]
    fn stated_figures(&self, req_id: i64, series: u32) -> PyResult<Vec<f64>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        Ok(shared.market.stated_figures(instrument, series))
    }

    /// What one of the venue's numbered-table series last stated for a
    /// subscription: its figures under the venue's own numbering.
    ///
    /// Four series state their figures as two tables, whole numbers and
    /// fractional ones, each entry naming what it is before stating it.
    /// `fractional` picks the table; the venue numbers the two separately, so
    /// the same number in each is not the same figure.
    #[pyo3(signature = (req_id, series, fractional=false))]
    fn numbered_figures(
        &self, req_id: i64, series: u32, fractional: bool,
    ) -> PyResult<Vec<(i32, f64)>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        Ok(shared.market.numbered_figures(instrument, series, fractional))
    }

    /// The run of paired figures one series last stated for a subscription.
    ///
    /// Two series state their figures as a count and then that many pairs: the
    /// volatility the venue's own model puts on each point of a curve, and the
    /// weight it puts on each price a contract might reach.
    #[pyo3(signature = (req_id, series))]
    fn paired_figures(&self, req_id: i64, series: u32) -> PyResult<Vec<(f64, f64)>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        Ok(shared.market.paired_figures(instrument, series))
    }

    /// Ask the venue to scan an underlying for strategies worth putting on.
    ///
    /// The scan goes out beside a subscription for the series the venue states
    /// its answer on, because that is how it is asked for: the series carries
    /// the answer and the scan tells the venue what to look for. Read the
    /// answer with `scanned_strategies` under the same request, and withdraw
    /// it with `cancel_mkt_data`.
    ///
    /// The documented API has no call for this at all. What the scan states
    /// about each strategy is the venue's own, in the venue's own words, and
    /// nothing here translates them.
    #[pyo3(signature = (req_id, contract, scan))]
    fn req_spread_scan(
        &self, py: Python<'_>, req_id: i64, contract: &Contract,
        scan: &SpreadScan,
    ) -> PyResult<()> {
        let Some(_tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        let mut scan = crate::types::SpreadScan::from(scan);
        let con_id = if scan.under_con_id > 0 { scan.under_con_id } else { contract.con_id };
        if con_id <= 0 {
            return self.report_refusal(py, req_id, Refusal::validation(
                "a spread scan names the contract to scan by the venue's id for it",
            ));
        }
        scan.under_con_id = con_id;
        // The scan rides its own request, so two scans of one contract each
        // state their own; the engine sends one at a time.
        let mode = self.core.subscription_mode();
        self.ask_for_mkt_data(py, req_id, contract, "481", false, false, mode, Some(scan.stated()), None, true)
    }

    /// The strategies a spread scan stated for a request, as the venue stated
    /// them.
    ///
    /// Each is a dict of its legs, which shape of strategy it is and how
    /// pressing, the thirteen figures the venue states about it, where it comes
    /// out even, and one figure behind those. Empty until a scan has been asked
    /// for and answered. The documented API has no call for this.
    #[pyo3(signature = (req_id))]
    fn scanned_strategies(&self, req_id: i64) -> PyResult<Vec<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        let found = shared.market.scanned_strategies(instrument);
        Python::attach(|py| {
            let mut out = Vec::with_capacity(found.len());
            for s in found {
                let dict = pyo3::types::PyDict::new(py);
                dict.set_item("legs", s.legs.clone())?;
                dict.set_item("kind", s.kind)?;
                dict.set_item("aggression", s.aggression)?;
                dict.set_item("figures", s.figures.clone())?;
                dict.set_item("breakEvens", s.break_evens.clone())?;
                dict.set_item("lastFigure", s.last_figure)?;
                out.push(dict.into_any().unbind());
            }
            Ok(out)
        })
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
    #[pyo3(signature = (req_id, series))]
    fn stated_rows(&self, req_id: i64, series: u32) -> PyResult<Vec<(f64, f64, f64)>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        Ok(shared.market.stated_rows(instrument, series))
    }

    /// What one of the option model's chain series last stated for a
    /// subscription on an underlying.
    ///
    /// A list with one dict per class of the underlying's options: the
    /// underlying's price, the dividends expected, and per expiry the yield,
    /// the interest rate, the forward and the at-the-money volatilities the
    /// model works from. Ask for series 687 for the standing set, or 691 for
    /// the set as the chain closed, in the generic tick list of a subscription
    /// on the underlying.
    ///
    /// The documented API has no call for either: a gateway reads them for its
    /// own option model and hands none of it on. Empty where the request names
    /// no subscription, or the series has stated nothing for it.
    #[pyo3(signature = (req_id, series))]
    fn chain_model_parameters(&self, req_id: i64, series: u32) -> PyResult<Vec<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        let found = shared.market.chain_model_parameters(instrument, series);
        Python::attach(|py| {
            let mut out = Vec::with_capacity(found.len());
            for set in found {
                let dict = pyo3::types::PyDict::new(py);
                dict.set_item("productId", set.product_id)?;
                dict.set_item("classes", set.classes)?;
                dict.set_item("underlyingPrice", set.underlying_price)?;
                dict.set_item("dividends", set.dividends)?;
                dict.set_item("indexStyleDividends", set.index_style_dividends)?;
                let mut terms = Vec::with_capacity(set.terms.len());
                for term in set.terms {
                    let at = pyo3::types::PyDict::new(py);
                    at.set_item("lastTradeDate", term.last_trade_date)?;
                    at.set_item("modelYield", term.model_yield)?;
                    at.set_item("interestRate", term.interest_rate)?;
                    at.set_item("forward", term.forward)?;
                    at.set_item("callAtmVol", term.call_atm_vol)?;
                    at.set_item("putAtmVol", term.put_atm_vol)?;
                    at.set_item("attributes", term.attributes)?;
                    terms.push(at);
                }
                dict.set_item("terms", terms)?;
                dict.set_item("timestampMillis", set.timestamp_millis)?;
                dict.set_item("attributes", set.attributes)?;
                out.push(dict.into_any().unbind());
            }
            Ok(out)
        })
    }

    /// Which series have stated rows for a subscription, in order.
    #[pyo3(signature = (req_id))]
    fn stated_rows_series(&self, req_id: i64) -> PyResult<Vec<u32>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        Ok(shared.market.stated_rows_series(instrument))
    }

    /// Which series have stated paired figures for a subscription, in order.
    #[pyo3(signature = (req_id))]
    fn paired_figures_series(&self, req_id: i64) -> PyResult<Vec<u32>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        Ok(shared.market.paired_figures_series(instrument))
    }

    /// Which series have stated numbered figures for a subscription, in order.
    #[pyo3(signature = (req_id))]
    fn numbered_figures_series(&self, req_id: i64) -> PyResult<Vec<u32>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        Ok(shared.market.numbered_figures_series(instrument))
    }

    /// Which series have stated figures for a subscription, in order.
    #[pyo3(signature = (req_id))]
    fn stated_figures_series(&self, req_id: i64) -> PyResult<Vec<u32>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(Vec::new());
        };
        Ok(shared.market.stated_figures_series(instrument))
    }

    /// What the venue's model made of an option as it closed.
    ///
    /// The same model as `option_model` and in the same shape — every greek it
    /// states, the ones the documented API has no field for included — but
    /// worked out as the contract closed rather than as it stands. The
    /// documented API has no call for it at all. Ask for it by the venue's own
    /// number for the series in the generic tick list.
    #[pyo3(signature = (req_id))]
    fn closing_option_model(&self, req_id: i64) -> PyResult<Option<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(None) };
        let Some(instrument) = self.core.req_to_instrument.lock().unwrap().get(&req_id).copied()
        else {
            return Ok(None);
        };
        self.closing_option_model_dict(&shared, instrument)
    }

    /// The same, by InstrumentId, for callers who track them themselves.
    #[pyo3(signature = (instrument))]
    fn closing_option_model_by_instrument(&self, instrument: u32) -> PyResult<Option<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(None) };
        self.closing_option_model_dict(&shared, instrument)
    }

    /// The same, by InstrumentId, for callers who track them themselves.
    #[pyo3(signature = (instrument))]
    fn option_model_by_instrument(&self, instrument: u32) -> PyResult<Option<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(None) };
        self.option_model_dict(&shared, instrument)
    }
}

impl EClient {
    /// One statement of what the venue says about a contract, so the two
    /// readers above cannot publish different halves of it.
    fn contract_figures_dict(
        shared: &std::sync::Arc<crate::bridge::SharedState>,
        instrument: crate::types::InstrumentId,
    ) -> PyResult<Option<Py<PyAny>>> {
        let Some(f) = shared.market.contract_figures(instrument) else { return Ok(None) };
        Python::attach(|py| {
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("sharesOutstanding", f.shares_outstanding)?;
            dict.set_item("openAYearAgo", f.open_a_year_ago)?;
            Ok(Some(dict.into_any().unbind()))
        })
    }

    /// One statement of the record, so the two readers above cannot publish
    /// different halves of it.
    fn option_model_dict(
        &self,
        shared: &std::sync::Arc<crate::bridge::SharedState>,
        instrument: crate::types::InstrumentId,
    ) -> PyResult<Option<Py<PyAny>>> {
        let Some(m) = shared.market.option_model(instrument) else { return Ok(None) };
        Self::model_dict(m)
    }

    /// The same, for the model the venue worked out as the contract closed.
    fn closing_option_model_dict(
        &self,
        shared: &std::sync::Arc<crate::bridge::SharedState>,
        instrument: crate::types::InstrumentId,
    ) -> PyResult<Option<Py<PyAny>>> {
        let Some(m) = shared.market.closing_option_model(instrument) else { return Ok(None) };
        Self::model_dict(m)
    }

    /// One statement of a model's fields, so no two readers can publish
    /// different halves of the same record.
    fn model_dict(m: crate::types::OptionComputation) -> PyResult<Option<Py<PyAny>>> {
        Python::attach(|py| {
            let dict = pyo3::types::PyDict::new(py);
            for (name, value) in [
                ("impliedVol", m.implied_vol),
                ("delta", m.delta),
                ("optPrice", m.opt_price),
                ("pvDividend", m.pv_dividend),
                ("gamma", m.gamma),
                ("vega", m.vega),
                ("theta", m.theta),
                ("undPrice", m.und_price),
                ("rho", m.rho),
                ("fugit", m.fugit),
                ("exerciseBoundary", m.exercise_boundary),
                ("forwardCoeff", m.forward_coeff),
                ("modelYield", m.model_yield),
                ("bridgeYield", m.bridge_yield),
                ("timeValue", m.time_value),
                ("calDays", m.cal_days),
                ("rate", m.rate),
            ] {
                dict.set_item(name, value)?;
            }
            dict.set_item("priceBasedVol", m.price_based_vol)?;
            Ok(Some(dict.into_any().unbind()))
        })
    }
}

impl EClient {
    /// Withdraw a request's subscription, saying nothing about it.
    ///
    /// The body of `cancel_mkt_data`, for the withdrawals this client makes on
    /// its own: a snapshot that has ended, the watch opened behind an option
    /// calculation. Nobody asked for those, so nothing is reported against the
    /// request — made through the public cancel, a handler that disconnected
    /// on `tick_snapshot_end` was told 504 about a snapshot that had just
    /// completed.
    pub(crate) fn withdraw_mkt_data(
        &self,
        py: Python<'_>,
        tx: &std::sync::mpsc::Sender<ControlCommand>,
        req_id: i64,
    ) -> PyResult<()> {
        if let Err(why) = self.send_control(tx, ControlCommand::CancelMktData { req_id }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Hand a market-data request to the engine, which names the contract
    /// where the caller described it or gave only its id, registers it, and
    /// serves the request: nothing waits here. What it is served on, or why it
    /// is not, stands in the session's order.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ask_for_mkt_data(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        generic_tick_list: &str,
        snapshot: bool,
        regulatory_snapshot: bool,
        mode_9887: i32,
        spread_scan: Option<String>,
        calculation: Option<Box<crate::types::Calculation>>,
        delayed_allowed: bool,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        // As on the other surface: a number every other request refuses is
        // refused here too, and a negative one is read as a gateway reads it.
        if req_id >= 0 {
            wire_req_id(req_id)?;
        }
        let shared = self.shared_state()?;
        // What tells two listings of one symbol apart, taken whole. A caller
        // who names an option's class, its local name or where it is listed
        // states it here the way every other request that names a contract by
        // description does; dropped, the lookup behind the subscription asks a
        // wider question than the caller put and is answered with several
        // contracts, which names none.
        let filters = contract.lookup_filters();
        if let Err(why) = self.core.register_mkt_data(
            &shared, &tx, req_id,
            contract.con_id, &contract.symbol, &contract.exchange, &contract.sec_type,
            &contract.currency, &filters,
            snapshot, regulatory_snapshot, generic_tick_list, mode_9887,
            spread_scan, calculation, delayed_allowed,
        ) {
            return self.report_refusal(py, req_id, why);
        }
        self.core.cache_contract(contract.con_id, crate::types::model::Contract {
            con_id: contract.con_id,
            symbol: contract.symbol.clone(),
            sec_type: contract.sec_type.clone(),
            exchange: contract.exchange.clone(),
            currency: contract.currency.clone(),
            last_trade_date_or_contract_month: contract.last_trade_date_or_contract_month.clone(),
            strike: contract.strike,
            right: contract.right.clone(),
            multiplier: contract.multiplier.clone(),
            ..Default::default()
        });
        Ok(())
    }
}
