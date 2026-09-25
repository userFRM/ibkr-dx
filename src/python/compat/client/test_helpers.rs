//! Test helper methods (hidden from public API).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::bridge::SharedState;
use crate::control::historical::{HistoricalBar, HistoricalResponse, HeadTimestampResponse};
use crate::types::*;

use super::EClient;

/// The engine's side of a test-connected client.
///
/// A test session has no venue behind it. What the engine carries in its own
/// loop — an order from the call to the wire, a question answered from what
/// the session holds, the completed-orders exchange — is handed to a loop of
/// its own, and what that loop builds is kept as the commands it would send.
/// Everything else a call sends is kept as it was sent, and a question as it
/// was asked, for a test to read.
pub(crate) struct TestEngine {
    into: std::sync::mpsc::Sender<ControlCommand>,
    engine: crate::engine::hot_loop::HotLoop,
    out: std::collections::VecDeque<String>,
}

impl TestEngine {
    fn new(shared: &Arc<SharedState>) -> Self {
        let mut engine = crate::engine::hot_loop::HotLoop::new(shared.clone(), None, None);
        let (into, taken) = std::sync::mpsc::channel();
        engine.set_control_rx(taken);
        Self { into, engine, out: std::collections::VecDeque::new() }
    }

    /// Take what the calls sent, and give the loop a lap.
    fn take(&mut self, rx: &std::sync::mpsc::Receiver<ControlCommand>) {
        for cmd in rx.try_iter() {
            let order = matches!(
                cmd,
                ControlCommand::Place(_)
                    | ControlCommand::CancelOrder { .. }
                    | ControlCommand::CancelOrderByPermId { .. }
                    | ControlCommand::GlobalCancel { .. }
                    | ControlCommand::Exercise(_)
                    | ControlCommand::Bracket(_)
            );
            let question = matches!(
                cmd,
                ControlCommand::Ask(_)
                    | ControlCommand::Retire(_)
                    | ControlCommand::FetchCompletedOrders { .. }
                    | ControlCommand::Subscribe { .. }
                    | ControlCommand::CancelMktData { .. }
                    | ControlCommand::CancelCalculation { .. }
                    | ControlCommand::SubscribeTbt { .. }
                    | ControlCommand::UnsubscribeTbt { .. }
            );
            // An order is read back as what the loop builds of it; a question
            // or a market-data request as it was asked, since the loop takes it
            // rather than sending it on as it stands.
            if question {
                self.out.push_back(format!("{cmd:?}"));
            }
            if order || question {
                let _ = self.into.send(cmd);
            } else {
                self.out.push_back(format!("{cmd:?}"));
            }
        }
        self.engine.poll_once();
        let built: Vec<String> = self.engine.context_mut().drain_pending_orders()
            .map(|req| format!("{:?}", ControlCommand::Order(req)))
            .collect();
        self.out.extend(built);
    }
}

impl EClient {
    /// Give a test session's engine what the calls have sent so far.
    pub(crate) fn _test_pump(&self) {
        let rx = self._test_control_rx.lock().unwrap();
        let mut engine = self._test_engine.lock().unwrap();
        if let (Some(rx), Some(engine)) = (rx.as_ref(), engine.as_mut()) {
            engine.take(rx);
        }
    }
}

/// A second a test-injected tick can plausibly have happened in.
///
/// The wire states a Unix second and a caller reads it as a moment. Pushing a
/// small constant instead put every injected tick in 1970, which is a shape no
/// real tick has, so a test could not tell a correct timestamp from one scaled
/// by a thousand.
fn a_recent_second() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[pymethods]
impl EClient {
    /// Set the session features stated at logon (test-only).
    #[doc(hidden)]
    fn _test_set_enabled_features(&self, features: Vec<String>) -> PyResult<()> {
        self.shared_state()?.reference.set_enabled_features(features);
        Ok(())
    }

    /// Seed the venue's model for a contract as a market-data subscription
    /// does: the model, then the answers to the calculations kept for it, as
    /// the engine pushes them. Then a model tick carrying the model's own
    /// volatility and prices, which is not the tick the engine builds (that
    /// takes the volatility and the underlying's price from other series):
    /// it stands in for one, to be delivered.
    #[doc(hidden)]
    #[pyo3(signature = (instrument, implied_vol, opt_price, und_price, rho=f64::MAX, fugit=f64::MAX))]
    fn _test_push_option_model(
        &self, instrument: u32, implied_vol: f64, opt_price: f64, und_price: f64,
        rho: f64, fugit: f64,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.market.push_option_computation(crate::types::OptionComputation {
            answers: None,
            instrument,
            implied_vol,
            opt_price,
            und_price,
            rho,
            fugit,
            ..Default::default()
        });
        crate::client_core::answer_kept_calculations(&shared, Some(instrument), None);
        let unstated = f64::MAX;
        shared.market.push_option_tick(crate::bridge::OptionTick {
            instrument,
            kind: crate::bridge::OptionTickKind::Model,
            figures: [implied_vol, unstated, opt_price, unstated, unstated, unstated, unstated, und_price],
            price_based: false,
        });
        Ok(())
    }

    /// Publish a headline about a contract, as a news subscription does.
    #[doc(hidden)]
    fn _test_push_tick_news(
        &self, instrument: u32, provider_code: &str, article_id: &str, headline: &str,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.market.push_tick_news(crate::types::TickNews {
            instrument,
            provider_code: provider_code.to_string(),
            article_id: article_id.to_string(),
            headline: headline.to_string(),
            timestamp: 1_700_000_000,
        });
        Ok(())
    }

    /// Seed the reference data the logon burst carries, so a test can tell a
    /// request that delivers it from one that always answers empty. The map of
    /// venues is stated for a share whose acknowledgement named BBO exchange
    /// `a6`, which `reqSmartComponents` names as `a60001`.
    #[doc(hidden)]
    fn _test_note_reference_data(
        &self, bit_number: i32, exchange: &str, exchange_letter: &str,
        tier_name: &str, tier_val: &str, branding_id: &str,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        const A_SHARE: crate::types::InstrumentId = 0;
        shared.reference.note_bbo_exchange(A_SHARE, "a6", "STK");
        shared.reference.set_smart_components_of(A_SHARE, "STK", vec![crate::types::SmartComponent {
            bit_number,
            exchange: exchange.to_string(),
            exchange_letter: exchange_letter.to_string(),
        }]);
        shared.reference.set_soft_dollar_tiers(vec![crate::types::SoftDollarTier {
            name: tier_name.to_string(),
            val: tier_val.to_string(),
            display_name: tier_name.to_string(),
        }]);
        shared.reference.set_white_branding_id(branding_id.to_string());
        Ok(())
    }

    /// Hold the rows one series stated for a contract, as the farm does when
    /// the series answers (test-only).
    #[doc(hidden)]
    fn _test_note_stated_rows(
        &self, instrument: u32, series: u32, rows: Vec<(f64, f64, f64)>,
    ) -> PyResult<()> {
        self.shared_state()?.market.note_stated_rows(instrument, series, rows);
        Ok(())
    }

    /// Seed one histogram bucket, so a test can read the shape the callback
    /// hands over and not only that it fired (test-only).
    #[doc(hidden)]
    fn _test_push_histogram(&self, req_id: u32, price: f64, count: i64) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.reference.push_histogram_data(
            req_id,
            vec![crate::control::histogram::HistogramEntry { price, count }],
        );
        Ok(())
    }

    /// Seed a trading schedule carrying one session (test-only).
    #[doc(hidden)]
    fn _test_push_historical_schedule(
        &self, req_id: u32, ref_date: &str, open_time: &str, close_time: &str,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.reference.push_historical_schedule(
            req_id,
            crate::types::HistoricalScheduleResponse {
                query_id: String::new(),
                timezone: "US/Eastern".to_string(),
                start_date_time: open_time.to_string(),
                end_date_time: close_time.to_string(),
                sessions: vec![crate::types::ScheduleSession {
                    ref_date: ref_date.to_string(),
                    open_time: open_time.to_string(),
                    close_time: close_time.to_string(),
                }],
            },
        );
        Ok(())
    }

    /// Seed the family codes the logon burst carries (test-only).
    #[doc(hidden)]
    fn _test_set_family_codes(&self, account_id: &str, family_code_str: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.reference.set_family_codes(vec![crate::types::FamilyCode {
            account_id: account_id.to_string(),
            family_code_str: family_code_str.to_string(),
        }]);
        Ok(())
    }

    /// Name the client this session connected under, as `connect` does.
    #[doc(hidden)]
    fn _test_set_client_id(&self, client_id: i32) {
        self.client_id.store(client_id, Ordering::Release);
        self.core.set_api_client_id(client_id);
        if let Some(shared) = self.shared.lock().unwrap().as_ref() {
            shared.orders.set_api_client_id(client_id);
        }
    }

    /// Create a fake "connected" EClient backed by a SharedState + channel.
    #[doc(hidden)]
    #[pyo3(signature = (account_id="TEST123".to_string(), readonly=false, replay_done=true, port=0, accounts=None))]
    fn _test_connect(
        &self, account_id: String, readonly: bool, replay_done: bool, port: i32,
        accounts: Option<Vec<String>>,
    ) -> PyResult<()> {
        self.core.set_readonly(readonly);
        // Claimed rather than read, as on the real connect: two callers
        // racing here both found it clear and both built a session.
        if self
            .connected
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(PyRuntimeError::new_err("Already connected"));
        }
        let shared = Arc::new(SharedState::new());
        shared.orders.set_api_client_id(self.client_id.load(Ordering::Acquire));
        shared.set_session_account(&account_id);
        // No venue behind a test session, so the replay of what the account
        // already has on is over before it starts. Left unsaid, every request
        // for the open orders waits out the bound before answering. A test of
        // that wait itself asks for a session that has not had it yet.
        if replay_done {
            shared.orders.set_replay_done();
        }
        // A real session opens on a message the venue stamps, and that stamp is
        // what this client answers "what time does the venue say it is" from.
        // A test session that carried none had every such request refused for
        // want of one, which is the answer for a session that has heard
        // nothing — not for one that has just opened.
        shared.market.note_venue_time(&crate::protocol::datetime::chrono_free_timestamp());
        let (tx, rx) = std::sync::mpsc::channel();
        *self._test_engine.lock().unwrap() = Some(TestEngine::new(&shared));
        *self.shared.lock().unwrap() = Some(shared);
        *self.control_tx.lock().unwrap() = Some(tx);
        *self.account_id.lock().unwrap() = Some(account_id);
        // Kept alive: dropping the receiving end closes the channel, and every
        // request that sends one then fails on a client reporting itself
        // connected.
        *self._test_control_rx.lock().unwrap() = Some(rx);
        self.next_order_id.store(1000, Ordering::Relaxed);
        self.session_ended.store(false, Ordering::Release);
        self.close_notified.store(false, Ordering::Release);
        self.port.store(port, Ordering::Release);
        let accounts = accounts.unwrap_or_else(|| vec![self.account()]);
        // What a logon names, which the checks on orders and on the account
        // requests read, as they read it on a real session.
        if let Some(shared) = self.shared.lock().unwrap().as_ref() {
            shared.reference.set_login(accounts.clone(), false);
        }
        *self.accounts.lock().unwrap() = accounts;
        Ok(())
    }

    /// Let go of the engine's end of the control channel, leaving a session
    /// that reports itself connected and can send nothing.
    ///
    /// The window a real session reaches this through is narrow — the engine
    /// stops between the check a call makes and the send it then does — and it
    /// is the window in which the two surfaces answered the same failure
    /// differently. Held open here so a test can ask which way a call answers.
    #[doc(hidden)]
    fn _test_drop_engine(&self) {
        *self._test_control_rx.lock().unwrap() = None;
        *self._test_engine.lock().unwrap() = None;
    }

    /// How many disconnects this client has counted.
    ///
    /// What the connect race turns on: a disconnect landing while a connect is
    /// still in the venue's hands has nothing installed to stop, so the only
    /// trace it leaves for that connect to read is this.
    #[doc(hidden)]
    fn _test_disconnects(&self) -> u64 {
        self.disconnects.load(Ordering::Acquire)
    }

    /// Say the quote feed is done for the rest of this session, as the engine
    /// does when it gives up on it.
    #[doc(hidden)]
    fn _test_say_the_feed_is_over(&self, why: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        // Leaked deliberately: the engine states this from a fixed set of
        // reasons, and a test naming its own needs the same lifetime.
        shared.market.set_market_data_over(Box::leak(why.to_string().into_boxed_str()));
        Ok(())
    }

    /// Which slot a number is watching, or nothing.
    #[doc(hidden)]
    fn _test_watching(&self, req_id: i64) -> Option<u32> {
        self.core.watching(req_id)
    }

    /// What this session has queued for the engine, taken and cleared.
    ///
    /// Written out rather than handed over as objects: a test asks whether a
    /// command was sent and which order it names, and the debug rendering
    /// answers both without publishing the engine's own types.
    #[doc(hidden)]
    fn _test_take_commands(&self) -> Vec<String> {
        self._test_pump();
        let mut engine = self._test_engine.lock().unwrap();
        let Some(engine) = engine.as_mut() else { return Vec::new() };
        engine.out.drain(..).collect()
    }

    /// Say which slot a contract's prices arrive in. Worth stating separately
    /// from the reqId mapping: what a position is worth is looked up by
    /// contract, so without this a held position has no price and no P&L.
    #[doc(hidden)]
    fn _test_map_con_id(&self, con_id: i64, instrument: u32) {
        self.core.con_id_to_instrument.lock().unwrap().insert(con_id, instrument);
    }

    /// Map a reqId to an instrument slot, on this side and in the engine's
    /// record of the requests it has taken — under the contract
    /// `_test_map_con_id` put in that slot, where it put one.
    #[doc(hidden)]
    fn _test_map_instrument(&self, req_id: i64, instrument: u32) {
        self.core.req_to_instrument.lock().unwrap().insert(req_id, instrument);
        self.core.instrument_to_req.lock().unwrap().insert(instrument, req_id);
        let con_id = self.core.con_id_to_instrument.lock().unwrap().iter()
            .find_map(|(con_id, at)| (*at == instrument).then_some(*con_id))
            .unwrap_or(0);
        if let Some(engine) = self._test_engine.lock().unwrap().as_mut() {
            engine.engine.md_requests.insert(req_id, crate::engine::hot_loop::market_requests::MdRequest {
                slot: instrument, con_id, series: Vec::new(), news: None, scan: false,
                for_calculation: false,
            });
        }
    }

    /// Say a tick stream is running under the number the caller gave it, in
    /// this slot, as the engine records one it has asked the venue for.
    #[doc(hidden)]
    fn _test_map_tbt(&self, req_id: i64, instrument: u32) {
        if let Some(engine) = self._test_engine.lock().unwrap().as_mut() {
            engine.engine.hmds.tbt_subscriptions.push(crate::engine::hot_loop::hmds::TbtSubscription {
                instrument,
                query_id: format!("tbt_{req_id}"),
                kind: TbtType::Last,
                caller_req_id: req_id,
                venue_id: 0,
                ignore_size: false,
                min_tick: 0,
                size_tick: 0.0,
                running: Default::default(),
            });
        }
    }

    /// Set instrument count on SharedState.
    #[doc(hidden)]
    fn _test_set_instrument_count(&self, count: u32) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.market.set_instrument_count(count);
        Ok(())
    }

    /// Push a quote into SharedState for a given instrument, stamped with the
    /// last trade's time `timestamp_ns` (none where it is nought).
    #[doc(hidden)]
    #[pyo3(signature = (instrument, bid=0.0, ask=0.0, last=0.0, bid_size=0, ask_size=0, last_size=0, volume=0, open=0.0, high=0.0, low=0.0, close=0.0, timestamp_ns=1))]
    fn _test_push_quote(
        &self, instrument: u32,
        bid: f64, ask: f64, last: f64,
        bid_size: i64, ask_size: i64, last_size: i64,
        volume: i64, open: f64, high: f64, low: f64, close: f64,
        timestamp_ns: u64,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        let ps = PRICE_SCALE as f64;
        let q = Quote {
            bid: (bid * ps) as i64, ask: (ask * ps) as i64, last: (last * ps) as i64,
            bid_size: bid_size * QTY_SCALE, ask_size: ask_size * QTY_SCALE,
            last_size: last_size * QTY_SCALE,
            volume: volume * QTY_SCALE,
            open: (open * ps) as i64, high: (high * ps) as i64,
            low: (low * ps) as i64, close: (close * ps) as i64,
            bid_exch_mask: 0, ask_exch_mask: 0, last_exch_mask: 0,
            timestamp_ns,
            halted: 0,
        };
        shared.market.push_quote(instrument, &q);
        Ok(())
    }

    /// Push a fill into SharedState.
    #[doc(hidden)]
    #[pyo3(signature = (instrument, order_id, side, price, qty, remaining, commission=0.0))]
    /// Push a fill into SharedState.
    ///
    /// `qty` and `remaining` are whole shares, which is what a caller writing
    /// the test is thinking in. They are scaled here, so a test states the
    /// size it means rather than the fixed-point figure that stands for it.
    fn _test_push_fill(
        &self, instrument: u32, order_id: u64, side: &str,
        price: f64, qty: i64, remaining: i64, commission: f64,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        let s = match side {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            "SSHORT" => Side::ShortSell,
            _ => return Err(PyRuntimeError::new_err(format!("Invalid side: {side}"))),
        };
        let ps = PRICE_SCALE as f64;
        shared.orders.push_fill(Fill {
            instrument, order_id, side: s,
            price: (price * ps) as i64,
            qty: crate::types::qty_from_wire(qty),
            remaining: crate::types::qty_from_wire(remaining),
            timestamp_ns: 100,
            // Single-print injection: the order total is this print.
            cum_qty: crate::types::qty_from_wire(qty),
            avg_price: (price * ps) as i64,
        });
        // What it cost, the way the venue sends it: its own message, naming the
        // execution it belongs to and stating the currency it is charged in.
        // A fill carries the figure but nothing reads it out of there — the
        // report a caller is told through is drained from the charges, so a
        // fill pushed without one is a fill whose cost reaches nobody.
        if commission != 0.0 {
            let currency = self
                .core
                .open_orders
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&order_id)
                .map(|tracked| tracked.contract.currency.clone())
                .unwrap_or_default();
            shared.orders.push_charge(
                crate::types::model::CommissionAndFeesReport::charged(
                    // The id the fill above is reported under, so the cost and
                    // the execution name the same thing.
                    &format!("{order_id}.100"),
                    commission,
                    &currency,
                ),
            );
        }
        Ok(())
    }

    /// Keep one execution as the session holds those the venue reported,
    /// stamped as the venue stamps it.
    #[doc(hidden)]
    fn _test_store_execution(&self, exec_id: &str, time: &str) {
        self.core.push_execution(
            Default::default(),
            crate::types::model::Execution {
                exec_id: exec_id.to_string(), time: time.to_string(), ..Default::default()
            },
            Default::default(),
        );
    }

    /// State a generic tick's figures on a contract's slot.
    #[doc(hidden)]
    fn _test_push_stated_figures(&self, instrument: u32, tick: u32, values: Vec<f64>) -> PyResult<()> {
        self.shared_state()?.market.note_stated_figures(instrument, tick, values);
        Ok(())
    }

    /// Push an order update into SharedState.
    #[doc(hidden)]
    fn _test_push_order_update(
        &self, order_id: u64, instrument: u32, status: &str,
        filled_qty: f64, remaining_qty: f64,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        let st = match status {
            "PendingSubmit" => OrderStatus::PendingSubmit,
            "PreSubmitted" => OrderStatus::PreSubmitted,
            "Submitted" => OrderStatus::Submitted,
            "PendingCancel" => OrderStatus::PendingCancel,
            "PendingReplace" => OrderStatus::PendingReplace,
            "Filled" => OrderStatus::Filled,
            "PartiallyFilled" => OrderStatus::PartiallyFilled,
            "Cancelled" => OrderStatus::Cancelled,
            "Rejected" => OrderStatus::Rejected,
            "Inactive" => OrderStatus::Inactive,
            _ => return Err(PyRuntimeError::new_err(format!("Invalid status: {status}"))),
        };
        shared.orders.push_order_update(OrderUpdate {
            order_id, instrument, status: st, filled_qty, remaining_qty, avg_price: 0, perm_id: 0, parent_id: 0, timestamp_ns: 100,
        });
        Ok(())
    }

    /// Seed the venue's own order book with an order this client did not
    /// place, as a connect replays one.
    #[doc(hidden)]
    #[pyo3(signature = (
        order_id, symbol, action, total_quantity, lmt_price, status="Submitted".to_string(),
        acct_number="", con_id=0,
    ))]
    fn _test_push_venue_order(
        &self, order_id: u64, symbol: &str, action: &str,
        total_quantity: f64, lmt_price: f64, status: String, acct_number: &str, con_id: i64,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.orders.push_order_info(order_id, crate::bridge::RichOrderInfo {
            contract: crate::types::model::Contract {
                con_id, symbol: symbol.to_string(), sec_type: "STK".into(),
                exchange: "SMART".into(), currency: "USD".into(),
                ..Default::default()
            },
            order: crate::types::model::Order {
                order_id: order_id as i64, action: action.to_string(),
                total_quantity, order_type: "LMT".into(), lmt_price,
                ..Default::default()
            },
            order_state: crate::types::model::OrderState { status, ..Default::default() },
            // The report's own account, where the test states one; the venue
            // names it on tag 1.
            last_exec: crate::types::model::Execution {
                acct_number: acct_number.to_string(), ..Default::default()
            },
        });
        Ok(())
    }

    /// Push a completed order + rich info into SharedState (for req_completed_orders
    /// regression tests).
    #[doc(hidden)]
    #[pyo3(signature = (
        order_id, instrument, status, filled_qty,
        symbol, action, total_quantity, lmt_price,
        completed_status, completed_time, commission_and_fees_currency, warning_text, commission_and_fees,
    ))]
    fn _test_push_completed_order(
        &self,
        order_id: u64, instrument: u32, status: &str, filled_qty: i64,
        symbol: &str, action: &str, total_quantity: f64, lmt_price: f64,
        completed_status: &str, completed_time: &str, commission_and_fees_currency: &str,
        warning_text: &str, commission_and_fees: f64,
    ) -> PyResult<()> {
        use crate::types::model::{Contract as ApiContract, Order as ApiOrder, Execution as ApiExecution, OrderState as ApiOrderState};
        let shared = self.shared_state()?;
        let st = match status {
            "Filled" => OrderStatus::Filled,
            "Cancelled" => OrderStatus::Cancelled,
            "Rejected" => OrderStatus::Rejected,
            _ => return Err(PyRuntimeError::new_err(format!("Invalid status: {status}"))),
        };
        shared.orders.push_completed_order(crate::types::CompletedOrder {
            venue_order: String::new(), stated: None, held: None,
            order_id, instrument, status: st, filled_qty, timestamp_ns: 100,
        });
        shared.orders.push_order_info(order_id, crate::bridge::RichOrderInfo {
            contract: ApiContract {
                symbol: symbol.to_string(),
                sec_type: "STK".into(),
                exchange: "SMART".into(),
                currency: "USD".into(),
                ..Default::default()
            },
            order: ApiOrder {
                order_id: order_id as i64,
                action: action.to_string(),
                total_quantity,
                order_type: "LMT".into(),
                lmt_price,
                ..Default::default()
            },
            order_state: ApiOrderState {
                status: status.to_string(),
                completed_status: completed_status.to_string(),
                completed_time: completed_time.to_string(),
                commission_and_fees_currency: commission_and_fees_currency.to_string(),
                warning_text: warning_text.to_string(),
                commission_and_fees,
                ..Default::default()
            },
            last_exec: ApiExecution::default(),
        });
        Ok(())
    }

    /// Track an order locally (for req_open_orders regression tests).
    #[doc(hidden)]
    #[pyo3(signature = (
        order_id, instrument, symbol, action, total_quantity, lmt_price, parent_id=0,
        currency="USD",
    ))]
    fn _test_track_order(
        &self, order_id: u64, instrument: u32,
        symbol: &str, action: &str, total_quantity: f64, lmt_price: f64,
        parent_id: i64, currency: &str,
    ) -> PyResult<()> {
        use crate::types::model::{Contract as ApiContract, Order as ApiOrder};
        let contract = ApiContract {
            symbol: symbol.to_string(),
            sec_type: "STK".into(),
            exchange: "SMART".into(),
            currency: currency.to_string(),
            ..Default::default()
        };
        let order = ApiOrder {
            order_id: order_id as i64,
            action: action.to_string(),
            total_quantity,
            order_type: "LMT".into(),
            lmt_price,
            parent_id,
            // Under this session's client, as a real placement records it: an
            // order recorded without one reads as another client's, which is
            // what an order the venue names for a client that is not this one
            // is meant to read as.
            client_id: self.client_id.load(Ordering::Acquire),
            ..Default::default()
        };
        // And the engine's own record of what this session placed, which the
        // checks on a withdrawal or a replace read.
        if let Some(engine) = self._test_engine.lock().unwrap().as_mut() {
            engine.engine.intake.note_placed(order_id, order.clone(), instrument);
        }
        self.core.track_order(order_id, contract, order, instrument);
        Ok(())
    }

    /// Push a what-if response into SharedState (for dispatch regression tests).
    #[doc(hidden)]
    #[pyo3(signature = (
        order_id, instrument,
        init_margin_before, maint_margin_before, equity_with_loan_before,
        init_margin_after, maint_margin_after, equity_with_loan_after,
        commission,
    ))]
    fn _test_push_what_if(
        &self,
        order_id: u64, instrument: u32,
        init_margin_before: f64, maint_margin_before: f64, equity_with_loan_before: f64,
        init_margin_after: f64, maint_margin_after: f64, equity_with_loan_after: f64,
        commission: f64,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        let ps = PRICE_SCALE as f64;
        shared.orders.push_what_if(crate::types::WhatIfResponse {
            order_id, instrument,
            init_margin_before: (init_margin_before * ps) as i64,
            maint_margin_before: (maint_margin_before * ps) as i64,
            equity_with_loan_before: (equity_with_loan_before * ps) as i64,
            init_margin_after: (init_margin_after * ps) as i64,
            maint_margin_after: (maint_margin_after * ps) as i64,
            equity_with_loan_after: (equity_with_loan_after * ps) as i64,
            commission: Some((commission * ps) as i64),
            min_commission: None,
            max_commission: None,
            commission_currency: String::new(),
            warning_text: String::new(),
        });
        Ok(())
    }

    /// Push a cancel reject into SharedState.
    #[doc(hidden)]
    fn _test_push_cancel_reject(&self, order_id: u64, instrument: u32, reason_code: i32) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.orders.push_cancel_reject(CancelReject {
            order_id, instrument, reject_type: 1, reason_code,
            // The venue's own word on whether the order still stands rides on
            // the refusal; this helper states a refusal without one.
            answers_a_live_change: true, still_working: None,
            timestamp_ns: 100,
        });
        Ok(())
    }

    /// Push a TBT trade into SharedState, on the trade stream `kind` names.
    #[doc(hidden)]
    #[pyo3(signature = (instrument, price, size, exchange, kind="Last"))]
    fn _test_push_tbt_trade(
        &self, instrument: u32, price: f64, size: i64, exchange: &str, kind: &str,
    ) -> PyResult<()> {
        let kind = match kind {
            "AllLast" => crate::types::TbtType::AllLast,
            "Last" => crate::types::TbtType::Last,
            other => return Err(PyRuntimeError::new_err(format!("no such trade stream: {other}"))),
        };
        let shared = self.shared_state()?;
        let ps = PRICE_SCALE as f64;
        shared.market.push_tbt_trade(TbtTrade {
            // A size is held the way every size is held, so what a test
            // pushes reads back as the number of shares it named.
            req_id: instrument as i64,
            kind,
            instrument,
            price: (price * ps) as i64,
            size: size * crate::types::QTY_SCALE,
            exchange: exchange.to_string(), conditions: String::new(), timestamp: a_recent_second(),
            past_limit: false,
            unreported: false,
        });
        Ok(())
    }

    /// Push a TBT quote into SharedState.
    #[doc(hidden)]
    fn _test_push_tbt_quote(
        &self, instrument: u32, bid: f64, ask: f64, bid_size: i64, ask_size: i64,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        let ps = PRICE_SCALE as f64;
        shared.market.push_tbt_quote(TbtQuote {
            req_id: instrument as i64,
            instrument,
            bid: (bid * ps) as i64, ask: (ask * ps) as i64,
            bid_size: bid_size * crate::types::QTY_SCALE,
            ask_size: ask_size * crate::types::QTY_SCALE,
            timestamp: a_recent_second(),
            bid_past_low: false,
            ask_past_high: false,
        });
        Ok(())
    }

    /// Push one reading of the point between the two into SharedState.
    #[doc(hidden)]
    fn _test_push_tbt_mid(&self, instrument: u32, price: f64) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.market.push_tbt_mid(crate::types::TbtMid {
            req_id: instrument as i64,
            instrument,
            price: (price * PRICE_SCALE as f64) as i64,
            timestamp: a_recent_second(),
        });
        Ok(())
    }

    /// Push a level of a book into SharedState.
    ///
    /// A book is the one stream whose delivery to a caller was never checked
    /// without a session, and that is exactly where it was found not to arrive.
    #[doc(hidden)]
    #[pyo3(signature = (req_id, position, market_maker, operation, side, price, size, is_smart_depth=false))]
    fn _test_push_depth(
        &self, req_id: u32, position: i32, market_maker: &str, operation: i32,
        side: i32, price: f64, size: f64, is_smart_depth: bool,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.market.push_depth_update(crate::types::DepthUpdate {
            req_id, position, market_maker: market_maker.to_string(),
            operation, side, price, size, is_smart_depth,
        });
        Ok(())
    }

    /// Push historical data into SharedState.
    #[doc(hidden)]
    #[pyo3(signature = (req_id, bars, is_complete, timezone = "", ends = None))]
    fn _test_push_historical_data(
        &self, req_id: u32, bars: Vec<(String, f64, f64, f64, f64, i64)>, is_complete: bool,
        timezone: &str, ends: Option<Vec<String>>,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        let bar_list: Vec<HistoricalBar> = bars.into_iter().enumerate().map(|(index, (time, o, h, l, c, v))| {
            HistoricalBar {
                time, open: o, high: h, low: l, close: c, volume: v, wap: 0.0, count: 0,
                end: ends.as_ref().and_then(|values| values.get(index)).cloned().unwrap_or_default(),
            }
        }).collect();
        shared.reference.push_historical_data(req_id, HistoricalResponse {
            query_id: String::new(), timezone: timezone.to_string(), bars: bar_list, is_complete,
        });
        Ok(())
    }

    /// Push one batch of a scan's rows, or its refusal, into SharedState.
    #[doc(hidden)]
    #[pyo3(signature = (req_id, con_ids, error_text=""))]
    fn _test_push_scanner_data(&self, req_id: u32, con_ids: Vec<u32>, error_text: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.reference.push_scanner_data(req_id, crate::control::scanner::ScannerResult {
            entries: con_ids.iter().map(|&con_id| crate::control::scanner::ScannerEntry { con_id }).collect(),
            con_ids,
            scan_time: String::new(),
            error_text: error_text.to_string(),
        });
        Ok(())
    }

    /// Push the calendar's answer to one request into SharedState: its schema,
    /// or with `events` its events.
    #[doc(hidden)]
    #[pyo3(signature = (req_id, json, events=false))]
    fn _test_push_calendar(&self, req_id: u32, json: &str, events: bool) -> PyResult<()> {
        let shared = self.shared_state()?;
        if events {
            shared.reference.push_calendar_events(req_id, json.to_string());
        } else {
            shared.reference.push_calendar_meta_data(req_id, json.to_string());
        }
        Ok(())
    }

    /// Push the venue's refusal of an order into SharedState.
    #[doc(hidden)]
    fn _test_push_order_inactive(&self, order_id: u64, code: i32, message: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.orders.push_order_inactive(
            order_id, crate::types::model::OrderOp::Venue, code, message.to_string(),
        );
        Ok(())
    }

    /// Push a venue refusal of one request into SharedState.
    #[doc(hidden)]
    fn _test_push_historical_error(&self, req_id: u32, code: i32, message: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.reference.push_historical_error(req_id, code, message.to_string());
        Ok(())
    }

    /// Push a head timestamp into SharedState.
    #[doc(hidden)]
    fn _test_push_head_timestamp(&self, req_id: u32, timestamp: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.reference.push_head_timestamp(req_id, HeadTimestampResponse {
            head_timestamp: timestamp.to_string(), timezone: String::new(),
        });
        Ok(())
    }

    /// Push account state into SharedState.
    #[doc(hidden)]
    #[pyo3(signature = (net_liquidation=0.0, buying_power=0.0, daily_pnl=0.0, unrealized_pnl=0.0, realized_pnl=0.0, account=""))]
    fn _test_set_account(
        &self, net_liquidation: f64, buying_power: f64,
        daily_pnl: f64, unrealized_pnl: f64, realized_pnl: f64, account: &str,
    ) -> PyResult<()> {
        let shared = self.shared_state()?;
        let portfolio = shared.portfolio_for(account);
        let ps = PRICE_SCALE as f64;
        let mut acct = portfolio.account();
        acct.net_liquidation = (net_liquidation * ps) as i64;
        acct.buying_power = (buying_power * ps) as i64;
        acct.daily_pnl = (daily_pnl * ps) as i64;
        acct.unrealized_pnl = (unrealized_pnl * ps) as i64;
        acct.realized_pnl = (realized_pnl * ps) as i64;
        portfolio.set_account(&acct);
        // And as the venue states them, which is what the account is reported
        // from: a figure this client holds and the venue never sent is not one
        // a caller hears about.
        for (key, value) in [
            ("NetLiquidation", net_liquidation),
            ("BuyingPower", buying_power),
            ("DailyPnL", daily_pnl),
            ("UnrealizedPnL", unrealized_pnl),
            ("RealizedPnL", realized_pnl),
        ] {
            portfolio.note_account_value(key, &format!("{value:.2}"), "");
        }
        Ok(())
    }

    /// End the session the way the engine ends one it has given up on.
    #[doc(hidden)]
    fn _test_end_session(&self) -> PyResult<()> {
        self.shared_state()?.reference.set_session_over("the test ended it");
        Ok(())
    }

    /// Say the venue has finished naming the orders already working.
    #[doc(hidden)]
    fn _test_finish_order_replay(&self) -> PyResult<()> {
        self.shared_state()?.orders.set_replay_done();
        Ok(())
    }

    /// Say the venue has begun naming the orders already working and has not
    /// finished, with the bound on the wait for it starting now.
    #[doc(hidden)]
    fn _test_begin_order_replay(&self) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.orders.replay_is_pending();
        shared.orders.note_naming_began();
        Ok(())
    }

    /// Say the executions this session holds start at `from`, in seconds
    /// since the epoch.
    #[doc(hidden)]
    fn _test_hold_executions_from(&self, from: i64) -> PyResult<()> {
        self.shared_state()?.reference.set_executions_held_from(Some(from));
        Ok(())
    }

    /// Push historical trade ticks, each stating its moment the way the venue
    /// spells one.
    #[doc(hidden)]
    fn _test_push_historical_ticks(&self, req_id: u32, times: Vec<String>) -> PyResult<()> {
        let shared = self.shared_state()?;
        let ticks = times.into_iter().enumerate().map(|(i, time)| crate::types::HistoricalTickLast {
            time,
            price: 100.0 + i as f64,
            size: 1.0,
            exchange: "NYSE".into(),
            special_conditions: String::new(),
            past_limit: false,
            unreported: false,
        }).collect();
        shared.reference.push_historical_ticks(
            req_id, crate::types::HistoricalTickData::Last(ticks), "TRADES".into(), true,
        );
        Ok(())
    }

    /// Add a second request watching a contract someone else subscribed to,
    /// as a shared subscription does.
    #[doc(hidden)]
    fn _test_follow_instrument(&self, req_id: i64, instrument: u32) {
        self.core.instrument_followers.lock().unwrap()
            .entry(instrument).or_default().push(req_id);
    }

    /// State one account figure the way the venue states it, under its own
    /// key and in its own currency — on the per-currency ledger, where
    /// `ledger` says so.
    #[doc(hidden)]
    #[pyo3(signature = (key, value, currency, ledger=false, account=""))]
    fn _test_note_account_value(&self, key: &str, value: &str, currency: &str, ledger: bool, account: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        let portfolio = shared.portfolio_for(account);
        if ledger {
            portfolio.note_ledger_value(key, value, currency);
        } else {
            portfolio.note_account_value(key, value, currency);
        }
        Ok(())
    }

    /// Say the venue has finished stating the account, which is what the
    /// portfolio and summary reads wait on.
    ///
    /// Opened and ended around whatever the test has already stated, so a
    /// holding set before this is one the download named rather than one the
    /// venue has stopped naming.
    #[doc(hidden)]
    #[pyo3(signature = (account=""))]
    fn _test_finish_account_download(&self, account: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        let portfolio = shared.portfolio_for(account);
        portfolio.set_account_download_complete("AR.1");
        portfolio.account_download_is_settled();
        Ok(())
    }

    /// Push a position into SharedState.
    #[doc(hidden)]
    #[pyo3(signature = (con_id, position, avg_cost, account=""))]
    fn _test_set_position(&self, con_id: i64, position: f64, avg_cost: f64, account: &str) -> PyResult<()> {
        let shared = self.shared_state()?;
        let portfolio = shared.portfolio_for(account);
        let ps = PRICE_SCALE as f64;
        portfolio.set_position_info(PositionInfo {
            con_id, position, avg_cost: (avg_cost * ps) as i64, ..Default::default()
        });
        Ok(())
    }

    #[doc(hidden)]
    fn _test_peek_ask_id(&self) -> PyResult<i64> {
        Ok(super::ask::peek_ask_id(&self.shared_state()?))
    }

    #[doc(hidden)]
    #[pyo3(signature = (req_id, con_id, symbol, right="", sec_type="", last_trade_date=""))]
    fn _test_push_contract_details(&self, req_id: u32, con_id: u32, symbol: &str, right: &str, sec_type: &str, last_trade_date: &str) -> PyResult<()> {
        use crate::control::contracts::{OptionRight, SecurityType};
        let shared = self.shared.lock().unwrap().clone()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("not connected"))?;
        let def = crate::control::contracts::ContractDefinition {
            con_id,
            symbol: symbol.to_string(),
            sec_type: SecurityType::from_fix(sec_type),
            last_trade_date: last_trade_date.to_string(),
            right: match right {
                "C" => Some(OptionRight::Call),
                "P" => Some(OptionRight::Put),
                _ => None,
            },
            ..Default::default()
        };
        shared.reference.push_contract_details(req_id, def);
        Ok(())
    }

    #[doc(hidden)]
    fn _test_push_contract_details_end(&self, req_id: u32) -> PyResult<()> {
        let shared = self.shared.lock().unwrap().clone()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("not connected"))?;
        shared.reference.push_contract_details_end(req_id);
        Ok(())
    }

    /// Run one iteration of the event dispatch loop.
    #[doc(hidden)]
    fn _test_dispatch_once(&self, py: Python<'_>) -> PyResult<()> {
        if !self.connected.load(std::sync::atomic::Ordering::Acquire) {
            return Err(PyRuntimeError::new_err("Not connected"));
        }
        let shared = self.shared_state()?;
        self.dispatch_once(py, &shared)
    }

    /// The connection going, as the engine says it (test-only).
    #[doc(hidden)]
    fn _test_push_disconnect_event(&self) -> PyResult<()> {
        self.shared_state()?.set_connection_lost();
        Ok(())
    }

    /// The connection coming back, as the engine says it (test-only).
    #[doc(hidden)]
    fn _test_push_reconnect_event(&self) -> PyResult<()> {
        self.shared_state()?.set_connection_restored();
        Ok(())
    }

    /// A session stopped, as the engine ends one: the reason, the loss it
    /// records, and the session's last record (test-only).
    #[doc(hidden)]
    fn _test_push_stopped_event(&self) -> PyResult<()> {
        let shared = self.shared_state()?;
        shared.reference.set_session_over(crate::reliability::retry::DisconnectReason::ByDesign.as_str());
        shared.set_connection_lost();
        shared.push_closed();
        Ok(())
    }

    /// The same as `_test_push_disconnect_event`, by the name the engine's
    /// own setter has (test-only).
    #[doc(hidden)]
    fn _test_set_connection_lost(&self) -> PyResult<()> {
        self.shared_state()?.set_connection_lost();
        Ok(())
    }

    /// The same as `_test_push_reconnect_event` (test-only).
    #[doc(hidden)]
    fn _test_set_connection_restored(&self) -> PyResult<()> {
        self.shared_state()?.set_connection_restored();
        Ok(())
    }

    /// The session's last record, as the engine pushes it when its loop ends
    /// (test-only).
    #[doc(hidden)]
    fn _test_push_closed(&self) -> PyResult<()> {
        self.shared_state()?.push_closed();
        Ok(())
    }
}
