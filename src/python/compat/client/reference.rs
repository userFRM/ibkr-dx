//! Reference data: contract details, historical data, scanners, news, fundamentals.

use pyo3::prelude::*;
use crate::error_codes::Refusal;

use crate::types::*;
use super::{wire_req_id, EClient};
use super::super::contract::Contract;
use crate::client_core::ClientCore;

#[pymethods]
impl EClient {
    /// Request historical bar data.
    ///
    /// `chart_options` is checked as a gateway checks it: `manual`, `0` or `1`, is
    /// taken and changes nothing a gateway sends; any other key is refused
    /// under 10337, another value under 10338, and an entry not written
    /// `key=value` under 320. Where the venue has lifted the key checks, a
    /// `manual` that does not read as the number nought or one is refused
    /// under 321.
    #[pyo3(signature = (req_id, contract, end_date_time, duration_str, bar_size_setting, what_to_show, use_rth, format_date=1, keep_up_to_date=false, chart_options=None))]
    pub(crate) fn req_historical_data(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        end_date_time: &str,
        duration_str: &str,
        bar_size_setting: &str,
        what_to_show: &str,
        use_rth: i32,
        format_date: i32,
        keep_up_to_date: bool,
        chart_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal(py, req_id, why);
        }
        // Before anything is noted, so a refused request leaves nothing behind.
        if let Some(why) = self.options_refused(py, &crate::client_core::CHART_OPTIONS, chart_options)? {
            return self.report_refusal(py, req_id, why);
        }
        // How it wants its bar times written, and what its range is counted
        // from, are written down where the engine takes the request, as on
        // the other surface.
        if !what_to_show.eq_ignore_ascii_case("SCHEDULE")
            && let Err(why) = ClientCore::validate_historical_args(
                bar_size_setting, what_to_show, keep_up_to_date, end_date_time,
                &contract.sec_type,
            )
        {
            return self.report_refusal(py, req_id, why.into());
        }
        // A contract given by id alone is named by the engine before the
        // request goes: a request states the contract's type and its
        // exchange, and both are the venue's to say.
        if what_to_show.eq_ignore_ascii_case("SCHEDULE") {
            if let Err(why) = self.send_control(&tx, ControlCommand::FetchHistoricalSchedule {
                    contract: contract.into(),
                    req_id: wire_req_id(req_id)?,
                    end_date_time: end_date_time.to_string(),
                    duration: duration_str.to_string(),
                    use_rth: use_rth != 0,
                    filters: contract.lookup_filters(),
                }) {
                return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
            }
        } else {
            if let Err(why) = self.send_control(&tx, ControlCommand::FetchHistorical {
                    contract: contract.into(),
                    req_id: wire_req_id(req_id)?,
                    end_date_time: end_date_time.to_string(),
                    duration: duration_str.to_string(),
                    bar_size: bar_size_setting.to_string(),
                    what_to_show: what_to_show.to_string(),
                    use_rth: use_rth != 0,
                    keep_up_to_date,
                    format_date,
                    include_expired: contract.include_expired,
                    filters: contract.lookup_filters(),
                }) {
                return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
            }
        }
        Ok(())
    }

    /// Cancel historical data.
    fn cancel_historical_data(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        let wire = wire_req_id(req_id)?;
        // A withdrawn request leaves nothing running under this id.
        self.core.forget_historical(wire);
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelHistorical { req_id: wire }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request head timestamp.
    #[pyo3(signature = (req_id, contract, what_to_show, use_rth, format_date=1))]
    pub(crate) fn req_head_time_stamp(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        what_to_show: &str,
        use_rth: i32,
        format_date: i32,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal(py, req_id, why);
        }
        // A contract given by id alone is named by the engine before the
        // request goes: a request states the contract's type and its
        // exchange, and both are the venue's to say.
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchHeadTimestamp {
                contract: contract.into(),
                req_id: wire_req_id(req_id)?,
                what_to_show: what_to_show.to_string(),
                use_rth: use_rth != 0,
                include_expired: contract.include_expired,
                format_date,
                filters: contract.lookup_filters(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel head timestamp request.
    fn cancel_head_time_stamp(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelHeadTimestamp { req_id: wire_req_id(req_id)? }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request contract details.
    pub(crate) fn req_contract_details(&self, py: Python<'_>, req_id: i64, contract: &Contract) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal(py, req_id, why);
        }
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchContractDetails {
                contract: contract.into(),
                req_id: wire_req_id(req_id)?,
                include_expired: contract.include_expired,
                filters: contract.lookup_filters(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Withdraw a contract lookup.
    ///
    /// Nothing is sent and nothing answers: there is nothing to withdraw. A
    /// gateway asks the venue nothing for this either: it only stops
    /// re-sending a lookup it held back while its connection to the venue was
    /// down, and this client holds none back — a lookup made with no
    /// connection is refused there and then. A lookup already asked for is
    /// still answered, as it is through a gateway.
    fn cancel_contract_data(&self, req_id: i64) -> PyResult<()> {
        let Some(_tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        Ok(())
    }

    /// Request available exchanges for market depth.
    fn req_mkt_depth_exchanges(&self, py: Python<'_>) -> PyResult<()> {
        let refused = crate::types::model::ErrorOrigin::Question { q: crate::types::model::Question::MktDepthExchanges, ends: true };
        let Some(tx) = self.tx_or_report_as(refused)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchMktDepthExchanges) {
            return self.report_refusal_as(py, refused, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Search for matching symbols.
    pub(crate) fn req_matching_symbols(&self, py: Python<'_>, req_id: i64, pattern: &str) -> PyResult<()> {
        super::wire_text("a matching-symbols pattern", pattern)?;
        // Normalised and checked the way the request surface does it: the
        // same pattern reaches the same search service over the same wire,
        // and agreeing on one surface and not the other answers the same
        // call two ways.
        let pattern = crate::api::client::reference::matching_symbols_pattern(pattern)
            .map_err(|refusal| pyo3::exceptions::PyRuntimeError::new_err(refusal.message))?;
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchMatchingSymbols {
                req_id: wire_req_id(req_id)?,
                pattern,
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request option chain parameters.
    ///
    /// Every argument is stated by the caller, as the reference client requires
    /// them to be. With `underlying_sec_type` defaulted to stocks, a caller who
    /// left it off asked about the chains of a stock by that name rather than
    /// being told they had left it off.
    #[pyo3(signature = (req_id, underlying_symbol, fut_fop_exchange, underlying_sec_type, underlying_con_id))]
    pub(crate) fn req_sec_def_opt_params(
        &self,
        py: Python<'_>,
        req_id: i64,
        underlying_symbol: &str,
        fut_fop_exchange: &str,
        underlying_sec_type: &str,
        underlying_con_id: i64,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchOptionParams {
                req_id: wire_req_id(req_id)?,
                symbol: underlying_symbol.to_string(),
                fut_fop_exchange: fut_fop_exchange.to_string(),
                underlying_sec_type: underlying_sec_type.to_string(),
                underlying_con_id,
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request scanner subscription.
    ///
    /// `scanner_subscription_options` is checked as a gateway checks it: `manual`, `0` or `1`, is
    /// taken and changes nothing a gateway sends; any other key is refused
    /// under 10337, another value under 10338, and an entry not written
    /// `key=value` under 320. Where the venue has lifted the key checks, a
    /// `manual` that does not read as the number nought or one is refused
    /// under 321.
    #[pyo3(signature = (req_id, subscription, scanner_subscription_options=None, scanner_subscription_filter_options=None))]
    fn req_scanner_subscription(
        &self,
        req_id: i64,
        subscription: Py<PyAny>,
        scanner_subscription_options: Option<Vec<Py<PyAny>>>,
        scanner_subscription_filter_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        Python::attach(|py| {
            // An absent attribute takes the default. One that is present and
            // cannot be read is a value the caller stated, and is refused
            // rather than run as a different scan under their request id.
            macro_rules! stated {
                ($attr:literal, $kind:ty, $default:expr) => {
                    match subscription.getattr(py, $attr) {
                        Err(_) => $default,
                        Ok(held) if held.is_none(py) => $default,
                        Ok(held) => match held.extract::<$kind>(py) {
                            Ok(value) => value,
                            Err(why) => return self.report_refusal(py, req_id,
                                crate::error_codes::Refusal::validation(format!(
                                    "the scan's {} cannot be read: {why}. Left off it would \
                                     be the default, but stated it names the scan, and one \
                                     run under a different one is not the scan asked for",
                                    $attr,
                                ))),
                        },
                    }
                };
            }
            // Empty, which is what the reference client's own subscription
            // holds for a field nobody set. Named as stocks and top gainers
            // here, a scan nobody described ran as a different scan.
            let instrument = stated!("instrument", String, String::new());
            let location_code = stated!("locationCode", String, String::new());
            let scan_code = stated!("scanCode", String, String::new());
            // Their own "no number stated" is a negative one, which is not a
            // count and is not an unreadable value either: it means the venue
            // picks, and so does its absence here.
            let rows = stated!("numberOfRows", i64, -1);
            let max_items = if rows < 0 { 50 } else { rows.min(u32::MAX as i64) as u32 };
            let filters = match scanner_filters(py, &subscription, &scanner_subscription_filter_options.unwrap_or_default()) {
                Ok(filters) => filters,
                Err(why) => return self.report_refusal(py, req_id, why),
            };
            // Read after the filters, as a gateway reads the list after them.
            if let Some(why) = self.options_refused(py, &crate::client_core::SCANNER_OPTIONS, scanner_subscription_options)? {
                return self.report_refusal(py, req_id, why);
            }
            self.send_control(&tx, ControlCommand::SubscribeScanner {
                req_id: wire_req_id(req_id)?, instrument, location_code, scan_code, max_items, filters,
            }).or_else(|why| Python::attach(|py| self.report_refusal_as(py, super::request_origin(req_id), crate::error_codes::Refusal::not_connected(why.to_string()))))
        })
    }

    /// Cancel scanner subscription.
    fn cancel_scanner_subscription(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelScanner { req_id: wire_req_id(req_id)? }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request scanner parameters XML.
    fn req_scanner_parameters(&self, py: Python<'_>) -> PyResult<()> {
        let refused = crate::types::model::ErrorOrigin::Question { q: crate::types::model::Question::ScannerParameters, ends: true };
        let Some(tx) = self.tx_or_report_as(refused)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchScannerParams) {
            return self.report_refusal_as(py, refused, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request a news article.
    ///
    /// `news_article_options` is checked as a gateway checks it: `manual`, `0` or `1`, is
    /// taken and changes nothing a gateway sends; any other key is refused
    /// under 10337, another value under 10338, and an entry not written
    /// `key=value` under 320. Where the venue has lifted the key checks, a
    /// `manual` that does not read as the number nought or one is refused
    /// under 321.
    #[pyo3(signature = (req_id, provider_code, article_id, news_article_options=None))]
    fn req_news_article(
        &self,
        py: Python<'_>,
        req_id: i64,
        provider_code: &str,
        article_id: &str,
        news_article_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Some(why) = self.options_refused(py, &crate::client_core::NEWS_ARTICLE_OPTIONS, news_article_options)? {
            return self.report_refusal(py, req_id, why);
        }
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchNewsArticle {
                req_id: wire_req_id(req_id)?,
                provider_code: provider_code.to_string(),
                article_id: article_id.to_string(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request historical news.
    ///
    /// Bounds are UTC timestamps, `YYYYMMDD-HH:MM:SS` or `YYYYMMDD HH:MM:SS`,
    /// optionally with fractional seconds. Empty bounds are omitted; unreadable
    /// ones are refused so the window is not lost.
    ///
    /// `historical_news_options` is checked as a gateway checks it: `manual`, `0` or `1`, is
    /// taken and changes nothing a gateway sends; any other key is refused
    /// under 10337, another value under 10338, and an entry not written
    /// `key=value` under 320. Where the venue has lifted the key checks, a
    /// `manual` that does not read as the number nought or one is refused
    /// under 321.
    #[pyo3(signature = (req_id, con_id, provider_codes, start_date_time, end_date_time, total_results, historical_news_options=None))]
    pub(crate) fn req_historical_news(
        &self,
        py: Python<'_>,
        req_id: i64,
        con_id: i64,
        provider_codes: &str,
        start_date_time: &str,
        end_date_time: &str,
        total_results: i32,
        historical_news_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Some(why) = self.options_refused(py, &crate::client_core::HISTORICAL_NEWS_OPTIONS, historical_news_options)? {
            return self.report_refusal(py, req_id, why);
        }
        if let Err(why) = crate::control::news::validate_news_window(
            start_date_time, end_date_time,
        ) {
            return self.report_refusal(py, req_id, why.into());
        }
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchHistoricalNews {
                req_id: wire_req_id(req_id)?,
                con_id: super::wire_con_id("a request for headlines", con_id)?,
                provider_codes: provider_codes.to_string(),
                start_time: start_date_time.to_string(),
                end_time: end_date_time.to_string(),
                // No more than a gateway asks for, whatever was wanted, and a
                // smaller number passed on as stated, below nought included.
                max_results: total_results.min(crate::control::news::MOST_HEADLINES_ASKED_FOR),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }


    /// Withdraw a historical news query.
    ///
    /// The TWS API has no call for this; the venue has a message for it. One
    /// message carrying the number the query went out under, sent for a query
    /// still waiting on its answer. Nothing of a query is kept once it is
    /// answered, as a gateway keeps nothing of one: withdrawn after its
    /// answer, it is refused as naming nothing.
    fn cancel_historical_news(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelHistoricalNews {
                req_id: wire_req_id(req_id)?,
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Ask for a contract's corporate actions over a range of days.
    ///
    /// No callback carries the answer; a refusal arrives on `error` under this
    /// id, as any request's does, and gives the request up: nothing is held for
    /// it after, and there is nothing to withdraw. The answer is held under the
    /// id until `adjustments_for` takes it or `cancel_adjustments` gives it up,
    /// so a request that is neither taken nor withdrawn holds its answer for the
    /// rest of the session. `corporate_actions` asks and waits in one call;
    /// this is the request on its own.
    #[pyo3(signature = (req_id, con_id, sec_type, exchange, start_date, end_date))]
    fn req_adjustments(
        &self,
        py: Python<'_>,
        req_id: i64,
        con_id: i64,
        sec_type: &str,
        exchange: &str,
        start_date: &str,
        end_date: &str,
    ) -> PyResult<()> {
        // Refused for every caller. The answering calls number themselves in a
        // band of their own and hold a number while they wait, so a request
        // numbered inside it has its answer taken by one of those calls, about
        // a contract and a range it did not ask for. That call sends its own
        // command directly and does not come through here.
        if crate::bridge::ReferenceState::is_ask_id(super::wire_u32("req_id", req_id)?) {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "req_id {req_id} is inside the range this client numbers its own \
                 answering calls in, and an answer under it would be taken for one of \
                 theirs: number the request below {}",
                crate::bridge::ReferenceState::ASK_ID_BASE,
            )));
        }
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        // Narrowed the way the request surface narrows it: a contract id of
        // zero, or a negative one, names nothing and the venue answers it with
        // silence — which reads as a contract with no actions rather than a
        // question that was never askable.
        let con_id = u32::try_from(con_id).ok().filter(|id| *id > 0).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "corporate actions are asked for by the venue's id for the contract, \
                 and {con_id} is not one: qualify the contract first and pass what \
                 comes back",
            ))
        })?;
        let wire = wire_req_id(req_id)?;
        // Where its answer is put is said by the engine, in the step that
        // sends it, so a withdrawal and a request again under one number are
        // taken in the order they were asked.
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchAdjustments {
                req_id: wire,
                con_id,
                sec_type: sec_type.to_string(),
                exchange: exchange.to_string(),
                start_date: start_date.to_string(),
                end_date: end_date.to_string(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// The corporate actions answering a `req_adjustments` under this id, once
    /// they have arrived: one dict per action, as `corporate_actions` states
    /// them.
    ///
    /// Taken rather than read: the answer is handed over once and the request
    /// holds nothing after it. `None` until the answer arrives, and for a
    /// request this session is not holding one for. A contract the venue
    /// states nothing for answers with an empty list, which is an answer.
    #[pyo3(signature = (req_id))]
    fn adjustments_for(
        &self, req_id: i64,
    ) -> Option<Vec<std::collections::BTreeMap<String, String>>> {
        let shared = self.shared_state().ok()?;
        // Read as a request is numbered, so an answer an answering call is
        // waiting on is never taken from under it.
        let req_id = wire_req_id(req_id).ok()?;
        let actions = shared.reference.take_adjustments_answering(req_id)?;
        shared.reference.stop_waiting_for_adjustments(req_id);
        Some(actions.into_iter().map(super::ask::stated_action).collect())
    }

    /// Give up on a `req_adjustments`: whatever it holds is let go of, and the
    /// venue is told to stop serving the query.
    ///
    /// For a request whose answer has not come and is no longer wanted: the
    /// venue serves the query until it is withdrawn. A withdrawal naming no
    /// query this client is waiting on, one already answered included, is
    /// reported on `error` under 300.
    #[pyo3(signature = (req_id))]
    fn cancel_adjustments(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let wire = wire_req_id(req_id)?;
        // What it held is let go of in the engine's step, in its place after
        // the request it withdraws.
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(
            &tx, ControlCommand::CancelCorporateActions { req_id: wire },
        ) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request fundamental data.
    ///
    /// The report is asked about the stock the venue's id names: a contract
    /// given by its description is looked up first, as a gateway looks it up.
    /// A contract not stated as a stock is refused, as a gateway refuses it.
    ///
    /// `fundamental_data_options` is taken and nothing in it is checked or
    /// applied, as through a gateway: a gateway reads no option list on this
    /// request.
    #[pyo3(signature = (req_id, contract, report_type, fundamental_data_options=None))]
    pub(crate) fn req_fundamental_data(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        report_type: &str,
        fundamental_data_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        let _ = fundamental_data_options;
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = ClientCore::validate_fundamentals_type(&contract.sec_type) {
            return self.report_refusal(py, req_id, why);
        }
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchFundamentalData {
                req_id: wire_req_id(req_id)?,
                contract: contract.into(),
                report_type: report_type.to_string(),
                filters: contract.lookup_filters(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel fundamental data.
    fn cancel_fundamental_data(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelFundamentalData { req_id: wire_req_id(req_id)? }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request historical tick data.
    ///
    /// `ignore_size` asks the venue to leave out a bid/ask change that moves
    /// only a size, and what it answers is passed on as it stands, as a
    /// gateway passes it: nothing is filtered here. A gateway asks for
    /// midpoint ticks that way whatever the caller asked, and so does this
    /// client; for trades it is not asked. One session saw the venue answer
    /// the same with the filter as without it.
    ///
    /// `misc_options` is checked as a gateway checks it: `manual`, `0` or `1`, is
    /// taken and changes nothing a gateway sends; any other key is refused
    /// under 10337, another value under 10338, and an entry not written
    /// `key=value` under 320. Where the venue has lifted the key checks, a
    /// `manual` that does not read as the number nought or one is refused
    /// under 321.
    #[pyo3(signature = (req_id, contract, start_date_time="", end_date_time="", number_of_ticks=1000, what_to_show="TRADES", use_rth=1, ignore_size=false, misc_options=None))]
    fn req_historical_ticks(
        &self,
        py: Python<'_>,
        req_id: i64,
        contract: &Contract,
        start_date_time: &str,
        end_date_time: &str,
        number_of_ticks: i32,
        what_to_show: &str,
        use_rth: i32,
        ignore_size: bool,
        misc_options: Option<Vec<Py<PyAny>>>,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal(py, req_id, why);
        }
        if let Some(why) = self.options_refused(py, &crate::client_core::HISTORICAL_TICKS_OPTIONS, misc_options)? {
            return self.report_refusal(py, req_id, why);
        }
        if let Err(why) = crate::control::historical::tick_data_type(what_to_show)
            .map(|_| ())
            .and_then(|()| crate::control::historical::validate_tick_window(
                start_date_time, end_date_time,
            ))
        {
            return self.report_refusal(py, req_id, why.into());
        }
        // A contract given by id alone is named by the engine before the
        // request goes: a request states the contract's type and its
        // exchange, and both are the venue's to say.
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchHistoricalTicks {
                contract: contract.into(),
                req_id: wire_req_id(req_id)?,
                start_date_time: start_date_time.to_string(),
                end_date_time: end_date_time.to_string(),
                number_of_ticks: super::wire_u32("number_of_ticks", number_of_ticks as i64)?,
                what_to_show: what_to_show.to_string(),
                use_rth: use_rth != 0,
                ignore_size,
                include_expired: contract.include_expired,
                filters: contract.lookup_filters(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Withdraw a historical ticks request.
    ///
    /// Nothing is sent and nothing answers: there is nothing to withdraw, as
    /// for `cancel_contract_data`. A gateway only stops re-sending a request
    /// it held back while its connection to the venue was down, which this
    /// client never does. Ticks already asked for still arrive, and a request
    /// waiting for its contract to be named still goes once it is, as through
    /// a gateway.
    fn cancel_historical_ticks(&self, req_id: i64) -> PyResult<()> {
        let Some(_tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        Ok(())
    }

    /// Request market rule details.
    fn req_market_rule(&self, py: Python<'_>, market_rule_id: i32) -> PyResult<()> {
        let refused = crate::types::model::ErrorOrigin::Question { q: crate::types::model::Question::MarketRule(market_rule_id), ends: true };
        let Some(_connected) = self.tx_or_report_as(refused)? else { return Ok(()) };
        // Released before the callback below — see the note in
        // req_completed_orders.
        let shared = self.shared.lock().unwrap().clone();
        if let Some(shared) = shared
            && let Some(rule) = shared.reference.market_rule(market_rule_id) {
                // Objects with names on them, as the reference client hands
                // them over and as the Rust surface here already did. A pair of
                // numbers left a program reading `lowEdge` holding a tuple.
                let steps: Vec<Py<pyo3::PyAny>> = rule.price_increments.iter()
                    .map(|pi| Py::new(py, super::super::contract::PriceIncrementPy {
                        low_edge: pi.low_edge,
                        increment: pi.increment,
                    }).map(|o| o.into_any()))
                    .collect::<PyResult<_>>()?;
                let list = pyo3::types::PyList::new(py, steps)?;
                self.deliver(py, "market_rule", (market_rule_id as i64, list.as_any()))?;
                return Ok(());
            }
        // Answered, not logged. A caller waiting on a callback that will never
        // come cannot tell that apart from a slow venue, and the other client
        // here has answered this all along.
        //
        // Under the number and against the id the reference client reports a
        // miss under: 322, against -1. The rule's number is not a request
        // number — nothing was ever sent under it — and this is not a
        // malformed request, so neither the id nor the validation code the
        // refusals above carry is the one a caller branching on the pair
        // reads there.
        self.report_refusal_as(
            py,
            refused,
            crate::error_codes::Refusal::stated(322, format!(
                "market rule {market_rule_id} has not been seen on this session. Rules \
                 arrive with the details of a contract that uses them, so ask for such a \
                 contract first"
            )),
        )
    }

    /// Request histogram data.
    #[pyo3(signature = (req_id, contract, use_rth, time_period))]
    pub(crate) fn req_histogram_data(&self, py: Python<'_>, req_id: i64, contract: &Contract, use_rth: bool, time_period: &str) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal(py, req_id, why);
        }
        // A contract given by id alone is named by the engine before the
        // request goes: a request states the contract's type and its
        // exchange, and both are the venue's to say.
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchHistogramData {
                req_id: wire_req_id(req_id)?,
                contract: contract.into(),
                use_rth,
                period: time_period.to_string(),
                filters: contract.lookup_filters(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel histogram data.
    fn cancel_histogram_data(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelHistogramData { req_id: wire_req_id(req_id)? }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request historical trading schedule.
    #[pyo3(signature = (req_id, contract, end_date_time="", duration_str="1 M", use_rth=true))]
    pub(crate) fn req_historical_schedule(
        &self, py: Python<'_>, req_id: i64, contract: &Contract,
        end_date_time: &str, duration_str: &str, use_rth: bool,
    ) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal(py, req_id, why);
        }
        // A contract given by id alone is named by the engine before the
        // request goes: a request states the contract's type and its
        // exchange, and both are the venue's to say.
        if let Err(why) = self.send_control(&tx, ControlCommand::FetchHistoricalSchedule {
                contract: contract.into(),
                req_id: wire_req_id(req_id)?,
                end_date_time: end_date_time.into(),
                duration: duration_str.into(),
                use_rth,
                filters: contract.lookup_filters(),
            }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }
}

/// `ScannerSubscription` attribute -> the scanner filter code it selects. Everything a
/// caller sets beyond instrument / location / scan code is a filter, and a filter the
/// subscription drops is a different result set.
const SCANNER_FILTERS: &[(&str, &str)] = &[
    ("abovePrice", "priceAbove"),
    ("belowPrice", "priceBelow"),
    ("aboveVolume", "volumeAbove"),
    ("marketCapAbove", "marketCapAbove1e6"),
    ("marketCapBelow", "marketCapBelow1e6"),
    ("moodyRatingAbove", "moodyRatingAbove"),
    ("moodyRatingBelow", "moodyRatingBelow"),
    ("spRatingAbove", "spRatingAbove"),
    ("spRatingBelow", "spRatingBelow"),
    ("maturityDateAbove", "maturityDateAbove"),
    ("maturityDateBelow", "maturityDateBelow"),
    ("couponRateAbove", "couponRateAbove"),
    ("couponRateBelow", "couponRateBelow"),
    ("averageOptionVolumeAbove", "avgOptVolumeAbove"),
];

/// `stockTypeFilter` name -> its filter value. Anything else, `ALL` included, is no
/// filter.
fn stk_types_code(name: &str) -> &'static str {
    match name.to_ascii_uppercase().as_str() {
        "STOCK" => "exc:ETF",
        "ETF" => "inc:ETF",
        "CORP" => "inc:CORP",
        "ADR" => "inc:ADR",
        "REIT" => "inc:REIT",
        "CEF" => "inc:CEF",
        _ => "",
    }
}

/// One filter value, or `None` when the attribute is missing or left at its unset
/// default. One that is present and cannot be read is a refusal, not an unset
/// filter: sent without it, the scan would run narrower or wider than the one
/// described.
fn scanner_filter_value(py: Python<'_>, sub: &Py<PyAny>, attr: &str) -> Result<Option<String>, crate::error_codes::Refusal> {
    let Ok(value) = sub.getattr(py, attr) else { return Ok(None) };
    if value.is_none(py) {
        return Ok(None);
    }
    if let Ok(n) = value.extract::<f64>(py) {
        // An unset numeric filter arrives as `sys.float_info.max` or `2**31 - 1`, and
        // sending either as a bound would empty the scan.
        if n == f64::MAX || n == f64::from(i32::MAX) {
            return Ok(None);
        }
        return Ok(Some(n.to_string()));
    }
    if let Ok(text) = value.extract::<String>(py) {
        return Ok((!text.is_empty()).then_some(text));
    }
    Err(unreadable_filter(attr))
}

/// What a filter that is stated and cannot be read is reported as. Left off,
/// it would be no filter; stated it narrows the scan, and one run without it
/// is not the scan asked for.
fn unreadable_filter(what: &str) -> crate::error_codes::Refusal {
    crate::error_codes::Refusal::validation(format!(
        "the scan's {what} is stated but cannot be read, and a scan run \
         without it is not the scan asked for",
    ))
}

/// Collect the subscription's filters, then the caller's explicit filter tags, which
/// win
/// over the named attribute selecting the same code.
fn scanner_filters(py: Python<'_>, sub: &Py<PyAny>, filter_options: &[Py<PyAny>]) -> Result<Vec<(String, String)>, crate::error_codes::Refusal> {
    let mut filters: Vec<(String, String)> = Vec::new();
    for (attr, code) in SCANNER_FILTERS {
        if let Some(value) = scanner_filter_value(py, sub, attr)? {
            filters.push(((*code).to_string(), value));
        }
    }

    match sub.getattr(py, "excludeConvertible") {
        Err(_) => {}
        Ok(v) if v.is_none(py) => {}
        Ok(v) => match v.extract::<bool>(py) {
            Ok(true) => filters.push(("excludeConvertible".to_string(), "true".to_string())),
            Ok(false) => {}
            Err(_) => return Err(unreadable_filter("excludeConvertible")),
        },
    }
    match sub.getattr(py, "stockTypeFilter") {
        Err(_) => {}
        Ok(v) if v.is_none(py) => {}
        Ok(v) => {
            let Ok(name) = v.extract::<String>(py) else {
                return Err(unreadable_filter("stockTypeFilter"));
            };
            let stk_types = stk_types_code(&name);
            if !stk_types.is_empty() {
                filters.push(("stkTypes".to_string(), stk_types.to_string()));
            }
        }
    }

    // The pairs a caller sets the scan's own controls with. A gateway reads
    // them and keeps them as the scan's settings, so they are taken rather
    // than refused. Where they go from there is not established, and this
    // protocol's scan carries a filter list and no settings field, so they are
    // not carried: said once, and the scan goes as the rest of it states.
    match sub.getattr(py, "scannerSettingPairs") {
        Err(_) => {}
        Ok(v) if v.is_none(py) => {}
        Ok(v) => match v.extract::<String>(py) {
            Ok(pairs) => crate::control::scanner::note_setting_pairs(&pairs),
            Err(_) => return Err(unreadable_filter("scannerSettingPairs")),
        },
    }

    for (at, option) in filter_options.iter().enumerate() {
        let what = format!("filter tag at position {at}");
        let Ok(tag) = option.getattr(py, "tag") else {
            return Err(unreadable_filter(&what));
        };
        let Ok(tag) = tag.extract::<String>(py) else {
            return Err(unreadable_filter(&what));
        };
        if tag.is_empty() {
            continue;
        }
        let value = match option.getattr(py, "value") {
            Err(_) => return Err(unreadable_filter(&format!("value of {tag}"))),
            Ok(v) => v,
        };
        let Ok(value) = value.extract::<String>(py) else {
            return Err(unreadable_filter(&format!("value of {tag}")));
        };
        filters.retain(|(code, _)| *code != tag);
        filters.push((tag, value));
    }
    Ok(filters)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespace(py: Python<'_>, fields: &str) -> Py<PyAny> {
        py.eval(&std::ffi::CString::new(format!("__import__('types').SimpleNamespace({fields})")).unwrap(), None, None)
            .unwrap().unbind()
    }

    #[test]
    fn a_described_stock_can_request_adjusted_history() {
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let wrapper = py.eval(
                c"__import__('builtins').type('W', (), {'__init__': lambda s: setattr(s, 'calls', []), '__getattr__': lambda s, n: (lambda *a: s.calls.append((n, a)))})()",
                None, None,
            ).unwrap().unbind();
            client.__init__(wrapper.clone_ref(py)).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            *client.control_tx.lock().unwrap() = Some(tx);
            *client.shared.lock().unwrap() = Some(std::sync::Arc::new(crate::bridge::SharedState::new()));
            client.connected.store(true, std::sync::atomic::Ordering::Release);
            let contract = Contract {
                symbol: "AAPL".into(), sec_type: "STK".into(), exchange: "SMART".into(),
                currency: "USD".into(), primary_exchange: "NASDAQ".into(), ..Default::default()
            };

            client.req_historical_data(
                py, 7, &contract, "", "1 M", "1 day", "ADJUSTED_LAST", 1, 1, false, None,
            ).unwrap();

            let ControlCommand::FetchHistorical {
                req_id, contract: sent, what_to_show, filters, ..
            } = rx.try_recv().expect("the engine resolves the description before asking for bars") else {
                panic!("the request asks for historical bars");
            };
            assert_eq!(req_id, 7);
            assert_eq!(sent, ContractRef::from(&contract));
            assert_eq!(sent.con_id, 0);
            assert_eq!(filters.primary_exchange, "NASDAQ");
            assert_eq!(what_to_show, "ADJUSTED_LAST");
            assert!(wrapper.getattr(py, "calls").unwrap().cast_bound::<pyo3::types::PyList>(py).unwrap().is_empty());
        });
    }

    #[test]
    fn scanner_subscription_attributes_become_filters() {
        Python::initialize();
        Python::attach(|py| {
            let sub = namespace(py, "abovePrice=10.0, belowPrice=1.7976931348623157e+308, \
                aboveVolume=2147483647, marketCapAbove=1.7976931348623157e+308, \
                moodyRatingAbove='', spRatingAbove='A', averageOptionVolumeAbove=500, \
                excludeConvertible=True, stockTypeFilter='etf'");
            assert_eq!(scanner_filters(py, &sub, &[]).unwrap(), vec![
                ("priceAbove".to_string(), "10".to_string()),
                ("spRatingAbove".to_string(), "A".to_string()),
                ("avgOptVolumeAbove".to_string(), "500".to_string()),
                ("excludeConvertible".to_string(), "true".to_string()),
                ("stkTypes".to_string(), "inc:ETF".to_string()),
            ]);
        });
    }

    #[test]
    fn explicit_filter_tags_replace_the_attribute_for_the_same_code() {
        Python::initialize();
        Python::attach(|py| {
            let sub = namespace(py, "abovePrice=10.0, excludeConvertible=False, stockTypeFilter='ALL'");
            let options = [
                namespace(py, "tag='priceAbove', value='20'"),
                namespace(py, "tag='usdMarketCapAbove', value='10000'"),
            ];
            assert_eq!(scanner_filters(py, &sub, &options).unwrap(), vec![
                ("priceAbove".to_string(), "20".to_string()),
                ("usdMarketCapAbove".to_string(), "10000".to_string()),
            ]);
        });
    }

    /// A corporate-actions request holds its answer until it is taken, holds
    /// nothing once it has been, and holds nothing once it is given up — on
    /// this surface as on the other.
    ///
    /// The only way to an answer here was the call that asks and waits, which
    /// holds this client while it waits: a program with a loop of its own
    /// could send the request and had nowhere to read what answered it.
    #[test]
    fn a_corporate_actions_request_holds_its_answer_until_it_is_taken() {
        use crate::control::adjustments::{AdjustedContract, Adjustment, AdjustmentKind};
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let wrapper = py.eval(
                c"__import__('builtins').type('W', (), {'__init__': lambda s: setattr(s, 'calls', []), '__getattr__': lambda s, n: (lambda *a: s.calls.append((n, a)))})()",
                None, None,
            ).unwrap().unbind();
            client.__init__(wrapper).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let shared = std::sync::Arc::new(crate::bridge::SharedState::new());
            *client.control_tx.lock().unwrap() = Some(tx);
            *client.shared.lock().unwrap() = Some(shared.clone());
            client.connected.store(true, std::sync::atomic::Ordering::Release);
            let nvda = || AdjustedContract { con_id: "4815747".into(), ..Default::default() };
            let split = || vec![Adjustment {
                kind: Some(AdjustmentKind::Split), date: "20240610".into(), value: "10".into(),
                ..Default::default()
            }];

            client.req_adjustments(py, 41, 4815747, "STK", "SMART", "20240101", "20241231").unwrap();
            assert!(matches!(rx.try_recv(), Ok(ControlCommand::FetchAdjustments { req_id: 41, .. })));
            assert_eq!(client.adjustments_for(41), None, "nothing has arrived");
            // The engine's step says where the answer goes, and there is no
            // engine behind this client.
            shared.reference.expect_adjustments(41);
            shared.reference.note_adjustments(nvda(), split(), 41);
            let taken = client.adjustments_for(41).expect("the answer to request 41");
            assert_eq!(taken.len(), 1);
            assert_eq!(taken[0]["kind"], "SS");
            assert_eq!(taken[0]["value"], "10");
            shared.reference.note_adjustments(nvda(), split(), 41);
            assert_eq!(client.adjustments_for(41), None, "the slot went with the answer");

            client.req_adjustments(py, 42, 4815747, "STK", "SMART", "20240101", "20241231").unwrap();
            let _ = rx.try_recv();
            client.cancel_adjustments(py, 42).unwrap();
            assert!(matches!(rx.try_recv(), Ok(ControlCommand::CancelCorporateActions { req_id: 42 })));
            shared.reference.note_adjustments(nvda(), split(), 42);
            assert_eq!(client.adjustments_for(42), None, "a withdrawn request keeps no answer");

            // Withdrawn after the engine gave the session up: nothing is sent,
            // and nothing is held either.
            client.req_adjustments(py, 43, 4815747, "STK", "SMART", "20240101", "20241231").unwrap();
            let _ = rx.try_recv();
            shared.reference.set_session_over("the test ended it");
            client.cancel_adjustments(py, 43).unwrap();
            assert!(rx.try_recv().is_err(), "nothing goes to an engine that stopped");
            shared.reference.note_adjustments(nvda(), split(), 43);
            assert_eq!(client.adjustments_for(43), None, "and the request holds nothing");
        });
    }

    /// A scan's settings pairs are taken, not refused: a gateway reads them
    /// and keeps them. They are not carried, which is said once at WARN, and
    /// the scan goes with the filters it states.
    #[test]
    fn a_scanner_setting_is_taken_and_the_scan_goes() {
        Python::initialize();
        Python::attach(|py| {
            let sub = namespace(py, "scannerSettingPairs='Annual,true', abovePrice=5.0");
            let filters = scanner_filters(py, &sub, &[]).expect("a stated setting is taken");
            assert_eq!(filters, [("priceAbove".to_string(), "5".to_string())], "the scan's filters go");
            let sub = namespace(py, "scannerSettingPairs=''");
            assert!(scanner_filters(py, &sub, &[]).is_ok(), "an unset setting is no refusal");
        });
    }

    /// A filter that is stated and cannot be read narrows the scan it is
    /// dropped from, and a scan run narrower is not the one asked for.
    /// Refused rather than dropped.
    #[test]
    fn a_filter_that_cannot_be_read_refuses_the_scan() {
        Python::initialize();
        Python::attach(|py| {
            let sub = namespace(py, "abovePrice=[1, 2]");
            let why = scanner_filters(py, &sub, &[])
                .expect_err("a stated filter that cannot be read is not dropped");
            assert!(why.message.contains("abovePrice"), "{}", why.message);

            let sub = namespace(py, "");
            let options = [namespace(py, "tag='priceAbove', value=10")];
            assert!(scanner_filters(py, &sub, &options).is_err(),
                "and neither is an explicit filter whose value cannot be read");
        });
    }

    /// A contract id of zero names no contract, and this surface narrows it
    /// the way the request surface does: the venue answers a question about
    /// contract zero with silence, which reads as a contract with no news.
    #[test]
    fn a_contract_id_of_zero_is_refused_where_the_request_surface_refuses_it() {
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let ns = pyo3::types::PyDict::new(py);
            py.run(
                c"class W:\n    def __getattr__(self, name):\n        return lambda *args: None\nw = W()",
                None,
                Some(&ns),
            ).unwrap();
            let wrapper = ns.get_item("w").unwrap().unwrap().unbind();
            client.__init__(wrapper).unwrap();
            let (tx, _rx) = std::sync::mpsc::channel();
            *client.control_tx.lock().unwrap() = Some(tx);

            let err = client
                .req_historical_news(py, 1, 0, "BRFG", "", "", 10, None)
                .expect_err("a contract id of zero names no contract");
            assert!(err.to_string().contains("is not one"), "{err}");
        });
    }

    /// The pattern rides the wire as one field's value, so this surface
    /// refuses one carrying the byte that separates fields the way the
    /// request surface does: sent anyway, what follows the byte would go out
    /// as fields the request never stated.
    #[test]
    fn a_pattern_carrying_the_field_separator_is_refused() {
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let ns = pyo3::types::PyDict::new(py);
            py.run(
                c"class W:\n    def __getattr__(self, name):\n        return lambda *args: None\nw = W()",
                None,
                Some(&ns),
            ).unwrap();
            let wrapper = ns.get_item("w").unwrap().unwrap().unbind();
            client.__init__(wrapper).unwrap();

            let err = client
                .req_matching_symbols(py, 1, "AAPL\x011=999")
                .expect_err("a pattern cannot carry the byte that separates fields");
            assert!(err.to_string().contains("separates fields"), "{err}");
        });
    }

    /// The pattern goes out as the venue would have sent it, on this surface
    /// as on the other, and one it would not have sent at all is refused
    /// here rather than asked.
    #[test]
    fn a_matching_symbols_pattern_is_sent_as_the_venue_sends_it() {
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let wrapper = py.eval(
                c"__import__('builtins').type('W', (), {'__init__': lambda s: setattr(s, 'calls', []), '__getattr__': lambda s, n: (lambda *a: s.calls.append((n, a)))})()",
                None, None,
            ).unwrap().unbind();
            client.__init__(wrapper).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            *client.control_tx.lock().unwrap() = Some(tx);
            *client.shared.lock().unwrap() = Some(std::sync::Arc::new(crate::bridge::SharedState::new()));
            client.connected.store(true, std::sync::atomic::Ordering::Release);

            client.req_matching_symbols(py, 8, "  APPLE   INC ").unwrap();
            let ControlCommand::FetchMatchingSymbols { pattern, .. } =
                rx.try_recv().expect("the search is asked for") else {
                panic!("the request asks the search service");
            };
            assert_eq!(pattern, "APPLE INC", "trimmed, and its runs of spaces collapsed");

            let err = client
                .req_matching_symbols(py, 8, "   ")
                .expect_err("the venue refuses this rather than answering it");
            assert!(err.to_string().contains("visible characters"), "{err}");
            assert!(rx.try_recv().is_err(), "and nothing was asked");
        });
    }

    /// A rule this session has not been told about is reported against -1
    /// under 322, the way the reference client reports one. The rule's number is
    /// not a request number, and a caller keying off the pair branches on
    /// both halves.
    #[test]
    fn an_unseen_market_rule_is_reported_against_minus_one_under_322() {
        Python::initialize();
        Python::attach(|py| {
            let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
            let wrapper = py.eval(
                c"__import__('builtins').type('W', (), {'__init__': lambda s: setattr(s, 'calls', []), '__getattr__': lambda s, n: (lambda *a: s.calls.append((n, a)))})()",
                None, None,
            ).unwrap().unbind();
            client.__init__(wrapper.clone_ref(py)).unwrap();
            let (tx, _rx) = std::sync::mpsc::channel();
            *client.control_tx.lock().unwrap() = Some(tx);

            client.req_market_rule(py, 26).unwrap();

            let calls = wrapper.getattr(py, "calls").unwrap();
            let calls = calls.bind(py).cast::<pyo3::types::PyList>().unwrap();
            assert_eq!(calls.len(), 1, "the miss is answered, once");
            let (name, args): (String, Vec<Py<PyAny>>) = calls.get_item(0).unwrap().extract().unwrap();
            assert_eq!(name, "error");
            assert_eq!(args[0].extract::<i64>(py).unwrap(), -1,
                "the rule's number is not the number this is reported against");
            assert_eq!(args[2].extract::<i32>(py).unwrap(), 322);
            assert!(args[3].extract::<String>(py).unwrap().contains("market rule 26"));
        });
    }
}
