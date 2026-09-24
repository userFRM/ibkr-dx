//! Account-related methods: positions, PnL, account summary/updates.

use pyo3::prelude::*;

use crate::error_codes::Refusal;
use crate::types::model::{ErrorOrigin, Question};
use crate::types::*;
use super::EClient;
use super::super::contract::Contract;
use super::super::super::types::PRICE_SCALE_F;

impl EClient {
    /// The contract a position is a position in. Prefers the secdef cache,
    /// which carries exchange/localSymbol/tradingClass, and falls back to the
    /// wire-derived `PositionInfo` fields when it is cold.
    pub(crate) fn position_contract(
        &self, py: Python<'_>, pi: &PositionInfo, shared: &crate::bridge::SharedState,
    ) -> PyResult<Contract> {
        match self.core.get_contract(pi.con_id, shared) {
            Some(ac) => Contract::from_api(py, &ac),
            None => Ok(Contract {
                con_id: pi.con_id,
                symbol: pi.symbol.clone(),
                sec_type: pi.sec_type.clone(),
                currency: pi.currency.clone(),
                multiplier: pi.multiplier.clone(),
                ..Default::default()
            }),
        }
    }
}

impl EClient {
    /// The account a P&L request names, checked as the other surface checks
    /// it.
    fn check_pnl_account(&self, account: &str) -> Result<(), Refusal> {
        let accounts = self.accounts.lock().unwrap().clone();
        let shared = self.shared_state().map_err(|e| Refusal::not_connected(e.to_string()))?;
        crate::client_core::ClientCore::check_pnl_account(&shared, &accounts, &self.account(), account)
            .inspect_err(|why| log::warn!("{}", why.message))
    }
}

#[pymethods]
impl EClient {
    /// Subscribe to the named account's profit. The account is checked as a
    /// gateway checks it. Each request has its own subscription; a repeated
    /// active request number is refused under 102. A model is taken and not
    /// applied, with a log notice once per session.
    #[pyo3(signature = (req_id, account, model_code=""))]
    fn req_pnl(&self, py: Python<'_>, req_id: i64, account: &str, model_code: &str) -> PyResult<()> {
        // A failed connection check leaves the request number free.
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        if let Err(why) = self.check_pnl_account(account) {
            return self.report_refusal(py, req_id, why);
        }
        let account = if account.eq_ignore_ascii_case("All") || account == "AllNonProp" {
            crate::client_core::ClientCore::note_account_selection(self.shared_state()?.as_ref(), account);
            self.account()
        } else { account.to_string() };
        if let Err(why) = self.core.subscribe_pnl(req_id, &account) {
            return self.report_refusal(py, req_id, why);
        }
        let acct = account.to_string();
        crate::client_core::ClientCore::note_account_selection(self.shared_state()?.as_ref(), model_code);
        // Answered on the error callback and returned normally, as a request
        // made before connecting already is. Raising instead is a path a
        // caller written against the reference client does not take, so the
        // two clients answered the same failure differently.
        if let Err(why) =
            self.send_control(&tx, ControlCommand::SubscribePnl { req_id, single: false, account: acct })
        {
            // A failed admission leaves the request number free.
            self.core.unsubscribe_pnl(req_id);
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel P&L subscription.
    fn cancel_pnl(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        self.core.unsubscribe_pnl(req_id);
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        // A failed send is reported. Discarded, the subscription stays up and
        // the caller is told the cancel succeeded.
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelPnl { req_id, single: false }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Subscribe to a position's profit in the named account.
    /// The account is checked as for the account-level profit. A model is
    /// taken and not applied, with a log notice once per session.
    #[pyo3(signature = (req_id, account, model_code, con_id))]
    fn req_pnl_single(&self, py: Python<'_>, req_id: i64, account: &str, model_code: &str, con_id: i64) -> PyResult<()> {
        let Some(_tx) = self.tx_or_report(-1)? else { return Ok(()) };
        if let Err(why) = self.check_pnl_account(account) {
            return self.report_refusal(py, req_id, why);
        }
        let account = if account.eq_ignore_ascii_case("All") || account == "AllNonProp" {
            crate::client_core::ClientCore::note_account_selection(self.shared_state()?.as_ref(), account);
            self.account()
        } else { account.to_string() };
        if let Err(why) = self.core.subscribe_pnl_single(req_id, con_id, &account) {
            return self.report_refusal(py, req_id, why);
        }
        crate::client_core::ClientCore::note_account_selection(self.shared_state()?.as_ref(), model_code);
        if let Err(why) = self.send_control(&_tx, ControlCommand::SubscribePnl { req_id, single: true, account: account.to_string() }) {
            self.core.unsubscribe_pnl_single(req_id);
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }


    /// Cancel single-position P&L subscription.
    fn cancel_pnl_single(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(_tx) = self.tx_or_report(-1)? else { return Ok(()) };
        self.core.unsubscribe_pnl_single(req_id);
        if let Err(why) = self.send_control(&_tx, ControlCommand::CancelPnl { req_id, single: true }) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request an account summary. `All` answers for every account the login
    /// holds. Account groups and `AllNonProp` are taken and not applied, with
    /// a log notice once per session. Validation and the limit of two standing
    /// summary requests follow a gateway.
    #[pyo3(signature = (req_id, group_name, tags))]
    fn req_account_summary(&self, py: Python<'_>, req_id: i64, group_name: &str, tags: &str) -> PyResult<()> {
        let Some(_tx) = self.tx_or_report(-1)? else { return Ok(()) };
        let shared = self.shared_state()?;
        let accounts = if group_name == "All" { self.accounts.lock().unwrap().clone() } else {
            crate::client_core::ClientCore::note_account_selection(&shared, group_name);
            vec![self.account()]
        };
        let checked = crate::client_core::ClientCore::check_account_summary(&shared, group_name, tags)
            .and_then(|()| self.core.subscribe_account_summary(req_id, tags, accounts.clone()));
        if let Err(why) = checked {
            return self.report_refusal(py, req_id, why);
        }
        for account in accounts {
            if let Err(why) = self.send_control(&_tx, ControlCommand::RefreshAccount { account }) {
                return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
            }
        }
        Ok(())
    }

    /// Cancel account summary.
    fn cancel_account_summary(&self, req_id: i64) -> PyResult<()> {
        let Some(_tx) = self.tx_or_report(-1)? else { return Ok(()) };
        self.core.unsubscribe_account_summary(req_id);
        Ok(())
    }

    /// Request all positions.
    ///
    /// Before a session exists this is reported on the error callback and the
    /// call returns, as every other request made before connecting is. A
    /// program written against the reference client has no exception handling
    /// around a request, because that client does not raise there.
    fn req_positions(&self, py: Python<'_>) -> PyResult<()> {
        let refused = ErrorOrigin::Question { q: Question::Positions, ends: true };
        let Some(tx) = self.tx_or_report_as(refused)? else { return Ok(()) };
        // Answered where the account has stated what it holds, which the
        // venue does as a session opens: the engine holds the question until
        // then, and names the holdings' contracts for a moment after, so
        // nothing waits here. Every holding and the end, as the account stands
        // where the answer stands in the session's order, and each move after
        // it on `position`: watched from there.
        if let Err(why) = self.send_control(&tx, ControlCommand::Ask(crate::types::Ask::Positions)) {
            return self.report_refusal_as(py, refused, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel positions.
    // nothing to withdraw at the venue: it pushes what the account holds when
    // the session opens and keeps it current. What stops is the reporting,
    // where the engine confirms the cancel in its place, after everything the
    // question was answered with; a `reqPositions` the engine still holds is
    // withdrawn, never answered.
    fn cancel_positions(&self, py: Python<'_>) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(-1)? else { return Ok(()) };
        let retire = crate::types::Retirement::Question(Question::Positions);
        if let Err(why) = self.send_control(&tx, ControlCommand::Retire(retire)) {
            return self.report_refusal_as(py, ErrorOrigin::Session, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Subscribe to the named account's figures and holdings, or withdraw
    /// the subscription. A single-account login ignores the name as a gateway
    /// does. Subscribing asks the venue to restate that account now; the engine
    /// holds the answer until its download ends or the existing wait expires.
    #[pyo3(signature = (subscribe, acct_code=""))]
    fn req_account_updates(&self, py: Python<'_>, subscribe: bool, acct_code: &str) -> PyResult<()> {
        // A subscription is the question of the account's figures; its
        // withdrawal, like `cancelPositions`, is the session's.
        let refused = if subscribe {
            ErrorOrigin::Question { q: Question::AccountUpdates, ends: true }
        } else {
            ErrorOrigin::Session
        };
        let Some(tx) = self.tx_or_report_as(refused)? else { return Ok(()) };
        let accounts = self.accounts.lock().unwrap().clone();
        let shared = self.shared_state()?;
        if let Err(why) = crate::client_core::ClientCore::check_account_updates(
            &shared, &accounts, subscribe, acct_code,
        ) {
            return self.report_refusal_as(py, refused, why);
        }
        let account = if crate::client_core::ClientCore::login_holds_several_accounts(&shared)
            && !acct_code.eq_ignore_ascii_case("All") && acct_code != "AllNonProp" {
            acct_code.to_string()
        } else {
            if acct_code == "AllNonProp" { crate::client_core::ClientCore::note_account_selection(&shared, acct_code); }
            self.account()
        };
        // A withdrawal is the question's cancel: the engine withdraws a
        // subscription it still holds and confirms the withdrawal in its
        // place, after everything the subscription was answered with.
        // Subscribed where the answer stands in the session's order, once the
        // account has stated itself, which is where the first batch and its
        // end are stated from.
        let command = if subscribe {
            ControlCommand::Ask(crate::types::Ask::AccountUpdates { account })
        } else {
            ControlCommand::Retire(crate::types::Retirement::Question(Question::AccountUpdates))
        };
        if let Err(why) = self.send_control(&tx, command) {
            return self.report_refusal_as(py, refused, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request managed accounts list. Answered with every account this login
    /// holds, comma separated, matching the reference client.
    ///
    /// Before a session exists there are no accounts to name, and an empty
    /// list reads as a login holding none rather than as a question asked too
    /// early.
    fn req_managed_accts(&self, py: Python<'_>) -> PyResult<()> {
        let refused = ErrorOrigin::Question { q: Question::ManagedAccounts, ends: true };
        let Some(_connected) = self.tx_or_report_as(refused)? else { return Ok(()) };
        self.deliver(py, "managed_accounts", (self.accounts_csv().as_str(),))?;
        Ok(())
    }

    /// Subscribe to the named account's figures under this request number.
    /// `ledger_and_nlv` selects the per-currency ledger and net liquidation.
    /// A model is taken and not applied, with a log notice once per session.
    /// The initial batch ends with `account_update_multi_end`; changes keep
    /// arriving until the request is cancelled.
    #[pyo3(signature = (req_id, account, model_code, ledger_and_nlv=false))]
    fn req_account_updates_multi(
        &self, py: Python<'_>, req_id: i64, account: &str, model_code: &str, ledger_and_nlv: bool,
    ) -> PyResult<()> {
        // Reported and returned, as `req_positions` above and every other
        // request before connecting. Raising made this one request out of the
        // set the caller had to guard.
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        crate::client_core::ClientCore::note_account_selection(self.shared_state()?.as_ref(), model_code);
        let account = if account.is_empty() || account.eq_ignore_ascii_case("All") || account == "AllNonProp" {
            crate::client_core::ClientCore::note_account_selection(self.shared_state()?.as_ref(), account);
            self.account()
        } else { account.to_string() };
        let ask = crate::types::Ask::AccountUpdatesMulti { req_id, account, model_code: model_code.to_string(), ledger_and_nlv };
        if let Err(why) = self.send_control(&tx, ControlCommand::Ask(ask)) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel multi-account updates.
    ///
    /// The request stops being reported to. The venue keeps the account current
    /// whether or not anyone is listening; what stops is the reporting — a
    /// figure that moves after this is no longer delivered on
    /// `accountUpdateMulti` for this request.
    ///
    /// A request the engine still holds is withdrawn, never answered; one it
    /// answered stops where the withdrawal stands, after its answer.
    fn cancel_account_updates_multi(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(-1)? else { return Ok(()) };
        let retire = crate::types::Retirement::AccountUpdatesMulti(req_id);
        if let Err(why) = self.send_control(&tx, ControlCommand::Retire(retire)) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Subscribe to holdings of the named account under this request number.
    /// A model is taken and not applied, with a log notice once per session.
    #[pyo3(signature = (req_id, account, model_code))]
    fn req_positions_multi(&self, py: Python<'_>, req_id: i64, account: &str, model_code: &str) -> PyResult<()> {
        // As above.
        let Some(tx) = self.tx_or_report(req_id)? else { return Ok(()) };
        crate::client_core::ClientCore::note_account_selection(self.shared_state()?.as_ref(), model_code);
        let account = if account.is_empty() || account.eq_ignore_ascii_case("All") || account == "AllNonProp" {
            crate::client_core::ClientCore::note_account_selection(self.shared_state()?.as_ref(), account);
            self.account()
        } else { account.to_string() };
        let ask = crate::types::Ask::PositionsMulti { req_id, account, model_code: model_code.to_string() };
        if let Err(why) = self.send_control(&tx, ControlCommand::Ask(ask)) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel multi-account positions.
    ///
    // nothing to withdraw: the venue pushes what the account holds and keeps
    // it current whether or not anyone is listening, as for
    // `cancel_positions`. What stops is the reporting — a holding that moves
    // after this is no longer delivered on `position_multi` for this request.
    //
    // A request the engine still holds is withdrawn, never answered; one it
    // answered stops where the withdrawal stands, after its answer.
    fn cancel_positions_multi(&self, py: Python<'_>, req_id: i64) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(-1)? else { return Ok(()) };
        let retire = crate::types::Retirement::PositionsMulti(req_id);
        if let Err(why) = self.send_control(&tx, ControlCommand::Retire(retire)) {
            return self.report_refusal(py, req_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Read account state snapshot. Returns a dict with all account values.
    fn account_snapshot(&self) -> PyResult<Option<Py<PyAny>>> {
        let shared = match self.shared.lock().unwrap().clone() {
            Some(s) => s,
            None => return Ok(None),
        };
        // Nothing where the venue has not stated the account whole on this
        // connection. Answered from what it last said instead, a caller
        // sizing an order after a drop read the buying power and the excess
        // liquidity from before it, with nothing to say so — and gated on
        // whether anything had been heard, the first figure of the new
        // connection let the rest of the pre-drop struct through.
        if !shared.portfolio.account_download_complete() {
            return Ok(None);
        }
        let acct = shared.portfolio.account();
        Python::attach(|py| {
            let ps = PRICE_SCALE_F;
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("net_liquidation", acct.net_liquidation as f64 / ps)?;
            dict.set_item("buying_power", acct.buying_power as f64 / ps)?;
            dict.set_item("total_cash_value", acct.total_cash_value as f64 / ps)?;
            dict.set_item("gross_position_value", acct.gross_position_value as f64 / ps)?;
            dict.set_item("unrealized_pnl", acct.unrealized_pnl as f64 / ps)?;
            dict.set_item("realized_pnl", acct.realized_pnl as f64 / ps)?;
            dict.set_item("daily_pnl", acct.daily_pnl as f64 / ps)?;
            dict.set_item("init_margin_req", acct.init_margin_req as f64 / ps)?;
            dict.set_item("maint_margin_req", acct.maint_margin_req as f64 / ps)?;
            dict.set_item("available_funds", acct.available_funds as f64 / ps)?;
            dict.set_item("excess_liquidity", acct.excess_liquidity as f64 / ps)?;
            dict.set_item("settled_cash", acct.settled_cash as f64 / ps)?;
            dict.set_item("accrued_cash", acct.accrued_cash as f64 / ps)?;
            dict.set_item("margin_used", acct.margin_used as f64 / ps)?;
            dict.set_item("equity_with_loan", acct.equity_with_loan as f64 / ps)?;
            dict.set_item("cushion", acct.cushion as f64 / ps)?;
            dict.set_item("leverage", acct.leverage as f64 / ps)?;
            dict.set_item("sma", acct.sma as f64 / ps)?;
            dict.set_item("day_trades_remaining", acct.day_trades_remaining)?;
            Ok(Some(dict.into_any().unbind()))
        })
    }

    /// Holdings the venue reports that this broker does not hold itself:
    /// positions held away at another broker, and rows it marks as shown but
    /// not held.
    ///
    /// Kept apart from `req_positions`, which answers what the account itself
    /// holds. The reference client has no call for these — its own front end
    /// shows them in a separate table — so this is the only way to reach them.
    /// One dict per holding: `con_id`, `symbol`, `sec_type`, `currency`,
    /// `position`, `avg_cost`, and `held`, which is `"Away"` for a position
    /// held at another broker, `"DisplayOnly"` for a row shown but not held,
    /// and `"Aside"` for one reported apart without saying why. Empty with no
    /// session.
    fn positions_elsewhere(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        shared.portfolio.positions_elsewhere().into_iter().map(|row| {
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("con_id", row.con_id)?;
            dict.set_item("symbol", row.symbol)?;
            dict.set_item("sec_type", row.sec_type)?;
            dict.set_item("currency", row.currency)?;
            dict.set_item("position", row.position)?;
            dict.set_item("avg_cost", row.avg_cost as f64 / PRICE_SCALE_F)?;
            dict.set_item("held", held_elsewhere_name(row.held))?;
            Ok(dict.into_any().unbind())
        }).collect()
    }

    /// The account figures describing one of the sets of holdings the account
    /// does not hold itself, as name, value and the currency each is stated
    /// in. A figure stated in two currencies is two figures.
    ///
    /// `held` names the set as `positions_elsewhere` does: `"Away"`,
    /// `"DisplayOnly"` or `"Aside"`. The venue states these the same way it
    /// states the account's own, and mixing them in would overstate what the
    /// account is worth, so they are kept where the holdings they describe are
    /// kept. Empty with no session.
    #[pyo3(signature = (held))]
    fn values_elsewhere(&self, held: &str) -> PyResult<Vec<(String, String, String)>> {
        let held = match held {
            "Away" => HeldElsewhere::Away,
            "DisplayOnly" => HeldElsewhere::DisplayOnly,
            "Aside" => HeldElsewhere::Aside,
            other => return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "{other:?} names none of the holdings kept elsewhere: \"Away\", \
                 \"DisplayOnly\" or \"Aside\"",
            ))),
        };
        let Ok(shared) = self.shared_state() else { return Ok(Vec::new()) };
        Ok(shared.portfolio.values_elsewhere(held))
    }
}

/// What a caller calls one of the venue's other sets of holdings: the Rust
/// client's own name for it, which `values_elsewhere` takes back.
fn held_elsewhere_name(held: HeldElsewhere) -> &'static str {
    match held {
        HeldElsewhere::Away => "Away",
        HeldElsewhere::DisplayOnly => "DisplayOnly",
        HeldElsewhere::Aside => "Aside",
    }
}

// Not a Python method: a helper the calls above share.
impl EClient {
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::SharedState;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    /// A connected client whose engine is a channel the test reads, and a
    /// wrapper that keeps every callback it is handed.
    fn wired_client(
        py: Python<'_>,
    ) -> (EClient, std::sync::mpsc::Receiver<ControlCommand>, Py<PyAny>) {
        let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
        let ns = pyo3::types::PyDict::new(py);
        py.run(
            c"class W:
    def __init__(self): self.calls = []
    def __getattr__(self, name):
        return lambda *args: self.calls.append((name,) + args)
w = W()",
            None,
            Some(&ns),
        ).unwrap();
        let wrapper = ns.get_item("w").unwrap().unwrap().unbind();
        client.__init__(wrapper.clone_ref(py)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        *client.shared.lock().unwrap() = Some(Arc::new(SharedState::new()));
        client.shared_state().unwrap().set_session_account("DU123");
        *client.control_tx.lock().unwrap() = Some(tx);
        *client.account_id.lock().unwrap() = Some("DU123".into());
        client.connected.store(true, Ordering::Release);
        (client, rx, wrapper)
    }

    /// What the venue reports holding elsewhere is read on this surface as on
    /// the other: apart from the account's own holdings, and each set of its
    /// figures by the name the holdings carry.
    ///
    /// Held by the engine and reachable only from Rust, a Python program could
    /// not see a position held away at all.
    #[test]
    fn holdings_kept_elsewhere_are_read_apart_from_the_accounts_own() {
        Python::initialize();
        Python::attach(|py| {
            let (client, _rx, _wrapper) = wired_client(py);
            assert!(client.positions_elsewhere(py).unwrap().is_empty(), "nothing reported yet");
            let shared = client.shared_state().unwrap();
            shared.portfolio.set_position_elsewhere(PositionElsewhere {
                con_id: 265598, symbol: "AAPL".into(), sec_type: "STK".into(),
                currency: "USD".into(), position: 30.0,
                avg_cost: (182.5 * PRICE_SCALE_F) as i64, held: HeldElsewhere::Away,
            });
            shared.portfolio.set_value_elsewhere(
                HeldElsewhere::Away, "NetLiquidation".into(), "5475".into(), "USD".into(),
            );

            let held = client.positions_elsewhere(py).unwrap();
            assert_eq!(held.len(), 1);
            let row = held[0].bind(py);
            assert_eq!(row.get_item("con_id").unwrap().extract::<i64>().unwrap(), 265598);
            assert_eq!(row.get_item("avg_cost").unwrap().extract::<f64>().unwrap(), 182.5);
            assert_eq!(row.get_item("held").unwrap().extract::<String>().unwrap(), "Away");

            assert_eq!(
                client.values_elsewhere("Away").unwrap(),
                vec![("NetLiquidation".to_string(), "5475".to_string(), "USD".to_string())],
            );
            assert!(client.values_elsewhere("DisplayOnly").unwrap().is_empty(), "another set");
            assert!(client.values_elsewhere("away").is_err(), "a name no set carries is refused");
        });
    }

    /// A held account is named on both profit requests without a refusal.
    #[test]
    fn a_profit_request_carries_the_named_account() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, wrapper) = wired_client(py);
            *client.accounts.lock().unwrap() = vec!["DU123".into(), "DU999".into()];
            client.req_pnl(py, 7, "DU999", "").unwrap();
            client.req_pnl_single(py, 8, "DU999", "", 265_598).unwrap();
            for id in [7, 8] {
                assert!(matches!(rx.try_recv(), Ok(ControlCommand::SubscribePnl { req_id, account, .. })
                    if req_id == id && account == "DU999"));
            }
            client.dispatch_once(py, &client.shared_state().unwrap()).unwrap();
            assert_eq!(wrapper.bind(py).getattr("calls").unwrap().len().unwrap(), 0);
        });
    }

    /// No account, and one the login does not hold, are refused as a gateway
    /// refuses them, in its words, and take nothing.
    #[test]
    fn a_profit_request_a_gateway_refuses_takes_nothing() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, wrapper) = wired_client(py);
            client.req_pnl(py, 7, "", "").unwrap();
            client.req_pnl_single(py, 8, "DU555", "", 265_598).unwrap();
            assert!(rx.try_recv().is_err(), "the venue is asked nothing");
            assert!(client.core.pnl_req_id.lock().unwrap().is_empty());
            assert!(client.core.pnl_single_reqs.lock().unwrap().is_empty());
            client.dispatch_once(py, &client.shared_state().unwrap()).unwrap();
            let heard = wrapper.bind(py).getattr("calls").unwrap()
                .extract::<Vec<(String, i64, i64, i64, String, String)>>().unwrap();
            let refused: Vec<(i64, i64, String)> = heard.into_iter()
                .filter(|(name, ..)| name == "error")
                .map(|(_, req_id, _, code, message, _)| (req_id, code, message))
                .collect();
            assert_eq!(refused, vec![
                (7, 321, "Account must not be empty".to_string()),
                (8, 321, "Invalid account code".to_string()),
            ]);
        });
    }
}
