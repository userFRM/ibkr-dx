//! Order placement, cancellation, open orders, executions, completed orders.

use std::sync::atomic::Ordering;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::types::model::{
    ExecutionFilter, OrderCancel,
};
use crate::error_codes::Refusal;
use crate::client_core::ClientCore;
use crate::types::*;
use super::EClient;
use super::super::contract::{Contract, Order};

/// What a withdrawal states about itself: who is withdrawing it, whether a
/// person entered it, and when.
///
/// Nothing where it states nothing — no object, or one left as it comes. Read
/// by attribute, as every object a caller fills in is read here, and a plain
/// string is taken as the time on its own. Each is written as its text, as the
/// reference client writes it, and the indicator is read back as a whole
/// number, as a gateway reads it: one that does not read as one refuses the
/// withdrawal in a gateway's words.
fn withdrawal_states(py: Python<'_>, order_cancel: Option<&Py<PyAny>>) -> PyResult<Result<OrderCancel, Refusal>> {
    let Some(held) = order_cancel else { return Ok(Ok(OrderCancel::default())) };
    if let Ok(time) = held.extract::<String>(py) {
        return Ok(Ok(time.into()));
    }
    let text = |attr: &str| -> PyResult<Option<String>> {
        match held.getattr(py, attr).ok().filter(|v| !v.is_none(py)) {
            Some(v) => Ok(Some(v.bind(py).str()?.to_cow()?.into_owned())),
            None => Ok(None),
        }
    };
    // Unset where nobody set it: the number this protocol writes for an
    // integer nobody set, and nothing where the object has no such field.
    // ponytail: ASCII digits; a gateway also reads other scripts' digits.
    let manual_order_indicator = match text("manualOrderIndicator")? {
        None => i32::MAX,
        Some(written) => match written.parse::<i32>() {
            Ok(number) => number,
            Err(_) => return Ok(Err(Refusal::stated(
                crate::error_codes::REQUEST_NOT_READ,
                format!(
                    "Error reading request: Unable to parse field: 'Manual Order Indicator' \
                     for input string: '{written}'",
                ),
            ))),
        },
    };
    Ok(Ok(OrderCancel {
        manual_order_cancel_time: text("manualOrderCancelTime")?.unwrap_or_default(),
        ext_operator: text("extOperator")?.unwrap_or_default(),
        manual_order_indicator,
    }))
}


#[pymethods]
impl EClient {
    /// Place an order.
    ///
    /// A request the client will not send is reported under the number the
    /// reference client reports it under, and the call returns. A program
    /// moved from that client has an `error` handler and no exception
    /// handling around a request, because nothing it was written against
    /// raises there. A send the engine can no longer take is reported the
    /// same way, stating what has already reached the engine and what has
    /// not.
    ///
    /// `slOrderId` / `slOrderType` and `ptOrderId` / `ptOrderType` construct
    /// children from the selected account preset. State each child id with
    /// `PRESET` (case-insensitive). Preset loading and any quote wait run in
    /// the engine; the call returns without waiting. The parent goes first,
    /// then stop loss and profit taker. `transmit=False` holds the family
    /// until a parent or child transmits it. Replacing the parent preserves
    /// its existing children. Percentage-allocation sizing from group or
    /// model holdings is not carried; use explicitly sized parent and child
    /// orders where their quantities depend on it.
    pub(crate) fn place_order(&self, py: Python<'_>, order_id: i64, contract: &Contract, order: &Order) -> PyResult<()> {
        if let Err(why) = self.core.refuse_if_readonly("an order") {
            return self.refuse_placement(py, order_id, Refusal::validation(why));
        }
        let Some(tx) = self.tx_or_report_for_trading(self.placement_origin(order_id))? else { return Ok(()) };

        let session = self.order_session();
        let mut api_order = order.to_api();
        let written = super::written_option_list(order.order_misc_options.bound(py).iter())?;
        match ClientCore::read_option_list(&crate::client_core::ORDER_OPTIONS, &written, &session.features) {
            Ok(read) => {
                api_order.order_misc_options = read.into_iter()
                    .map(|(tag, value)| crate::types::model::TagValue { tag: tag.into(), value: value.into() })
                    .collect();
            }
            Err(why) => return self.refuse_placement(py, order_id, why),
        }
        if let Err(why) = ClientCore::validate_order_destination(&contract.exchange) {
            return self.refuse_placement(py, order_id, why.into());
        }

        api_order.conditions = match order.convert_conditions(py) {
            Ok(conditions) => conditions,
            Err(why) => return self.refuse_placement(py, order_id, Refusal::validation(why)),
        };
        // The fields whose Python value is a list of objects: the conversion
        // cannot read one without the interpreter, so they are filled here.
        // An object this client cannot read is a refusal, not an empty value:
        // read as absent, a leg goes out unpriced, an algo runs on the venue's
        // defaults, and a tag the protocol does not carry stops being refused
        // for stating it.
        api_order.order_combo_legs = match order.convert_order_combo_legs(py) {
            Ok(legs) => legs,
            Err(why) => return self.refuse_placement(py, order_id, Refusal::validation(why)),
        };
        api_order.algo_params = match order.convert_algo_params(py) {
            Ok(params) => params,
            Err(why) => return self.refuse_placement(py, order_id, Refusal::validation(why)),
        };
        api_order.smart_combo_routing_params = match order.convert_smart_combo_routing_params(py) {
            Ok(params) => params,
            Err(why) => return self.refuse_placement(py, order_id, Refusal::validation(why)),
        };
        // The legs and the hedge the caller states: Python objects, which
        // need the interpreter to read. The contract itself is built whole
        // below, once the venue has named it.
        let api_contract = crate::types::model::Contract {
            // A leg this client cannot read is a refusal, like the other
            // fields read off the caller's objects above, and is reported
            // the same way.
            combo_legs: match contract.combo_legs_api(py) {
                Ok(legs) => legs,
                Err(why) => return self.refuse_placement(py, order_id, Refusal::validation(why)),
            },
            // Every field is read from the caller's object. A delta or price
            // defaulted to zero hedges the order against nothing.
            delta_neutral_contract: match contract.delta_neutral_contract.as_ref() {
                None => None,
                Some(d) => {
                    let read = |name: &str| -> Result<f64, String> {
                        d.getattr(py, name)
                            .and_then(|v| v.extract(py))
                            .map_err(|e| format!("the hedging contract states no readable {name}: {e}"))
                    };
                    let hedge = (|| {
                        Ok::<_, String>(crate::types::model::DeltaNeutralContract {
                            con_id: d
                                .getattr(py, "conId")
                                .and_then(|v| v.extract(py))
                                .map_err(|e| format!("the hedging contract states no readable conId: {e}"))?,
                            delta: read("delta")?,
                            price: read("price")?,
                        })
                    })();
                    match hedge {
                        Ok(hedge) => Some(hedge),
                        Err(why) => {
                            return self.refuse_placement(py, order_id, Refusal::validation(why))
                        }
                    }
                }
            },
            ..Default::default()
        };
        // Checked against the session as a gateway checks it: which accounts
        // the login holds and what the venue enabled.
        let (warnings, refused) = ClientCore::retired_instructions(&api_order, &session);
        let validation = ClientCore::validate_order(&api_order, &session);
        if let Err(why) = crate::client_core::attached_checks::check_selectors(&api_order, session.enables("NOAPISLPTSGL")) {
            return self.refuse_placement(py, order_id, why);
        }
        if let Err(why) = validation {
            if refused.is_some_and(|r| r.code == why.code) {
                for warning in warnings {
                    self.refuse_placement(py, order_id, warning)?;
                }
            }
            return self.refuse_placement(py, order_id, why);
        }
        if let Err(why) = ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.refuse_placement(py, order_id, why);
        }
        // From here on, the order as this session sends it.
        let api_order = ClientCore::as_sent(&api_order, &session).into_owned();
        if let Err(why) = ClientCore::validate_supported_instructions(&api_order) {
            return self.refuse_placement(py, order_id, why.into());
        }
        if let Err(why) = ClientCore::validate_combo_legs(
            &contract.sec_type, api_contract.combo_legs.len(),
        ) {
            return self.refuse_placement(py, order_id, why);
        }
        for (at, leg) in api_contract.combo_legs.iter().enumerate() {
            if let Err(why) = ClientCore::validate_leg(at, leg) {
                return self.refuse_placement(py, order_id, why);
            }
        }
        if let Err(why) = ClientCore::validate_order_contract(
            contract.con_id,
            &contract.sec_type,
            &crate::types::model::contract_identity(
                &contract.last_trade_date_or_contract_month, contract.strike,
                &contract.right, &contract.multiplier, &contract.currency,
            ),
        ) {
            return self.refuse_placement(py, order_id, why.into());
        }
        // The same guard the Rust surface applies. Without it here, whether a
        // caller is protected from an order the venue will refuse in silence
        // depends on which language they wrote in.
        if let Ok(shared) = self.shared_state()
            && let Err(why) = ClientCore::refuse_unpermitted_sec_type(
                &shared.reference.order_permissions(), &contract.sec_type,
            )
        {
            return self.refuse_placement(py, order_id, why);
        }

        // The legs and the hedge read off the caller's objects above, beside
        // the contract as the caller stated it. A contract the caller
        // described is named by the engine, once per description, and the
        // naming keeps what the caller said: an order carrying no id matches
        // nothing and is answered by silence.
        let api_contract = crate::types::model::Contract {
            combo_legs: api_contract.combo_legs,
            delta_neutral_contract: api_contract.delta_neutral_contract,
            ..contract.to_api()
        };

        if order_id > crate::bridge::MAX_ORDER_ID as i64 {
            return self.refuse_placement(py, order_id, Refusal::validation(format!("place_order: order_id {order_id} is past the highest this client can carry an order under ({})", crate::bridge::MAX_ORDER_ID)));
        }
        let oid = order_id as u64;
        if order_id > 0 && oid < crate::bridge::MAX_ORDER_ID && !crate::api::client::a_question_of_ours(oid) {
            self.next_order_id.fetch_max(oid + 1, Ordering::AcqRel);
            self.save_order_ids(py, self.next_order_id.load(Ordering::Acquire));
        }
        // The order as it goes, under its number and the client it goes out
        // under. Left at nought, the order read back could not be told from
        // one placed under client zero, and restating one as the other
        // collides with whatever is held there.
        let mut tracked_order = api_order.clone();
        tracked_order.order_id = oid as i64;
        tracked_order.client_id = self.client_id.load(Ordering::Acquire);
        // As on the other surface, the engine takes it from here: it names and
        // registers the contract, checks the order against what this session
        // placed and what the venue is working, builds it, and sends it or
        // keeps it for a later transmit — refusing it under its own number
        // where it will not. Nothing waits here.
        let placement = crate::types::Placement {
            allocator: self.next_order_id.clone(),
            order_id: oid,
            contract: api_contract,
            order: tracked_order,
            // What a gateway says about an order it places anyway, on the
            // order's number, once the order has gone or is kept.
            warnings,
        };
        if let Err(why) = py.detach(|| self.send_control(&tx, ControlCommand::Place(Box::new(placement)))) {
            return self.refuse_placement(py, order_id, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Exercise or lapse a long option position.
    ///
    /// `exercise_action` is 1 to exercise and 2 to lapse; anything else is
    /// refused.
    ///
    /// `account` is the account the exercise is taken on. A login holding
    /// several has to name one it holds; a login holding one takes it on its
    /// own, and an account other than its own holds no position here, which
    /// is answered under 322 as a gateway answers it.
    ///
    /// The instruction is checked as a gateway checks it, by the engine, which
    /// holds it while it does: the account's position in the option must be
    /// above nothing, or it is refused under 322 ("No unlapsed position exists
    /// in this option in account ..."), and no more than the position goes.
    /// The option's in-the-money figure (generic tick 493) is asked for
    /// whatever `override` says: one already held is used, and otherwise the
    /// engine watches for one, with no bound, as a gateway does. With
    /// `override` 0, an exercise of an option not in the money and a lapse of
    /// one in it are refused under 322, as a gateway refuses them; with 1 they
    /// go. `override` itself travels on no tag: it names this check, which is
    /// made before the order is built. The position and quantity are checked
    /// in the account named.
    #[pyo3(signature = (req_id, contract, exercise_action, exercise_quantity, account, r#override,
                        manual_order_time="", customer_account="", professional_customer=false))]
    fn exercise_options(
        &self, py: Python<'_>, req_id: i64, contract: &Contract, exercise_action: i32,
        exercise_quantity: i32, account: &str, r#override: i32,
        manual_order_time: &str, customer_account: &str, professional_customer: bool,
    ) -> PyResult<()> {
        if self.number_unread(req_id)? { return Ok(()); }
        let exercising = crate::types::model::ErrorOrigin::Order { id: req_id, op: crate::types::model::OrderOp::Exercise };
        if let Err(why) = self.core.refuse_if_readonly("an exercise") {
            return self.report_refusal_as(py, exercising, Refusal::validation(why));
        }
        // The session is what names the account an exercise would be taken on,
        // so it is established before the one the caller named is compared
        // against it. Without a session there is nothing to compare and nothing
        // to send, and the caller is told that rather than told about its
        // account.
        let Some(tx) = self.tx_or_report_for_trading(exercising)? else { return Ok(()) };
        if let Err(why) = crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month) {
            return self.report_refusal_as(py, exercising, why);
        }
        let (action, qty, account) = match ClientCore::validate_exercise(
            exercise_action, exercise_quantity, account, &self.order_session(),
        ) {
            Ok(checked) => checked,
            Err(why) => return self.report_refusal_as(py, exercising, why),
        };
        if let Err(why) = ClientCore::validate_order_contract(
            contract.con_id,
            &contract.sec_type,
            &crate::types::model::contract_identity(
                &contract.last_trade_date_or_contract_month, contract.strike,
                &contract.right, &contract.multiplier, &contract.currency,
            ),
        ) {
            return self.report_refusal_as(py, exercising, why.into());
        }

        let oid = if req_id > 0 {
            req_id as u64
        } else {
            0
        };
        // Spent, as a placement spends it.
        if oid > 0 && oid < crate::bridge::MAX_ORDER_ID {
            self.save_order_ids(py, oid + 1);
        }
        // The engine registers the option and sends the instruction, and
        // refuses a stated number the venue is working an order under.
        let exercise = crate::types::Exercise {
            allocator: (req_id <= 0).then(|| self.next_order_id.clone()),
            req_id,
            order_id: oid,
            stated: req_id > 0,
            contract: contract.to_api(),
            action,
            qty: crate::types::qty_from_wire(qty as i64),
            account,
            states: crate::client_core::ExerciseStates {
                manual_order_time: manual_order_time.to_string(),
                customer_account: customer_account.to_string(),
                professional_customer,
            },
            override_: r#override != 0,
        };
        if let Err(why) = self.send_control(&tx, ControlCommand::Exercise(Box::new(exercise))) {
            return self.report_refusal_as(py, exercising, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel an order.
    ///
    /// The second argument is what the reference client states about the
    /// withdrawal itself — when a person entered it, on whose authority, and
    /// whether a person entered it at all. It is taken as that object or as
    /// the time alone.
    ///
    /// Who is withdrawing it and whether a person entered it travel on the
    /// cancel, as a gateway writes them: from the withdrawal, not from the
    /// placement. A time does not travel. A gateway sends it only where the
    /// venue has turned that record on for the login, and this client does
    /// not read whether it has. The cancel goes anyway and the caller is told
    /// the time did not: refused outright, a live order would be left standing
    /// over a record this client does not send. Taken silently it would be withdrawn
    /// without the record while the caller had given one, so it is said. A
    /// time a gateway cannot read is refused as a gateway refuses it, under
    /// 10301, and nothing is withdrawn.
    ///
    /// Nothing is said as the cancel goes out, as a gateway says nothing then:
    /// asked for, the order reads `PendingCancel`, and the venue's next report
    /// on it states it.
    #[pyo3(signature = (order_id, order_cancel=None))]
    fn cancel_order(&self, py: Python<'_>, order_id: i64, order_cancel: Option<Py<PyAny>>) -> PyResult<()> {
        let cancelling = crate::types::model::ErrorOrigin::Order { id: order_id, op: crate::types::model::OrderOp::Cancel };
        if let Err(why) = self.core.refuse_if_readonly("a cancel") {
            return self.report_refusal_as(py, cancelling, Refusal::validation(why));
        }
        let Some(tx) = self.tx_or_report_for_trading(cancelling)? else { return Ok(()) };
        let oid = order_id as u64;
        let stated = match withdrawal_states(py, order_cancel.as_ref())?.and_then(|stated| {
            ClientCore::check_cancel_time(&stated.manual_order_cancel_time)?;
            crate::api::client::wire_text("a withdrawal's operator", &stated.ext_operator)?;
            Ok(stated)
        }) {
            Ok(stated) => stated,
            Err(why) => return self.report_refusal_as(py, cancelling, why),
        };
        // The engine withdraws a placement it has not sent by forgetting it,
        // with the changes behind it and what hangs from it, as on the other
        // surface; answers a withdrawal of an order this session saw finish
        // as not cancellable; and, once the venue has named the account's
        // working set, one naming an order nothing is working as no such
        // order. Where the withdrawal happens and states a time, it says the
        // time did not travel.
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelOrder { order_id: oid, stated }) {
            return self.report_refusal_as(py, cancelling, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel an order identified by `permId` — stable across sessions, unlike
    /// the local order id. The cancel frame is orderId-only, so the local id is
    /// looked up from the open-order cache; fails if `perm_id` is not tracked.
    /// Where more than one working order's record carries it, the order held
    /// under that number is the one withdrawn, as a gateway holds one order
    /// under a number, and of those records the one whose order the engine
    /// holds.
    fn cancel_order_by_perm_id(&self, py: Python<'_>, perm_id: i64) -> PyResult<()> {
        if let Err(why) = self.core.refuse_if_readonly("a cancel") {
            return self.report_refusal(py, -1, Refusal::validation(why));
        }
        if perm_id == 0 {
            return self.report_refusal(py, -1, Refusal::validation(
                "cancel_order_by_perm_id: perm_id must be non-zero",
            ));
        }
        let Some(tx) = self.tx_or_report_for_trading(crate::types::model::ErrorOrigin::Session)? else { return Ok(()) };
        // Found once the venue has named the working set, as on the other
        // surface, and withdrawn under the number its reports carry here.
        if let Err(why) = self.send_control(&tx, ControlCommand::CancelOrderByPermId { perm_id }) {
            return self.report_refusal(py, -1, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Cancel every order the account is working.
    ///
    /// This wire carries no request to withdraw everything, so it is composed
    /// here: one cancel for each order held, which is what a caller asking for
    /// everything back is asking for. What is held is what the venue named as working at connect and
    /// what this session placed since. The venue names the former after the
    /// connect returns, so a global cancel issued straight away waits for that
    /// naming, as asking for the open orders does, and covers what was named.
    /// Where the naming does not finish within the wait, what had been named
    /// is still withdrawn and the call says so rather than returning as
    /// though every order were covered: a partial cancel that reads as one
    /// beats the same cancel in silence, which reads as a complete answer.
    ///
    /// What the withdrawal states — who is withdrawing and whether a person
    /// entered it — travels on every cancel, as a gateway states it on every
    /// order it withdraws. A time does not: the reference client writes none
    /// on a withdrawal of everything, so a gateway never reads one, and one
    /// stated here goes the same way.
    #[pyo3(signature = (order_cancel=None))]
    fn req_global_cancel(&self, py: Python<'_>, order_cancel: Option<Py<PyAny>>) -> PyResult<()> {
        if let Err(why) = self.core.refuse_if_readonly("a global cancel") {
            return self.report_refusal(py, -1, Refusal::validation(why));
        }
        let Some(tx) = self.tx_or_report_for_trading(crate::types::model::ErrorOrigin::Session)? else { return Ok(()) };
        let stated = match withdrawal_states(py, order_cancel.as_ref())?.and_then(|stated| {
            crate::api::client::wire_text("a withdrawal's operator", &stated.ext_operator)?;
            Ok(stated)
        }) {
            Ok(stated) => stated,
            Err(why) => return self.report_refusal(py, -1, why),
        };
        // Everything the engine holds goes with everything working, and what
        // is working is withdrawn once the venue has named it; where the
        // naming does not finish within its bound, what had been named goes
        // and the caller is told what is not covered.
        if let Err(why) = self.send_control(&tx, ControlCommand::GlobalCancel { stated }) {
            return self.report_refusal(py, -1, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }

    /// Request next valid order ID.
    ///
    /// `num_ids` has no effect, as on a gateway: the next valid id is answered
    /// whatever number is asked for.
    ///
    /// Before a session exists there is no counter to answer from: the id an
    /// account may next use is the venue's to state. Answering announces zero,
    /// which names no order the venue will hold and is refused on placement.
    /// Reported the way the reference client reports every request made before
    /// connecting.
    #[pyo3(signature = (num_ids=1))]
    fn req_ids(&self, py: Python<'_>, num_ids: i32) -> PyResult<()> {
        let Some(tx) = self.tx_or_report(-1)? else { return Ok(()) };
        // The mark this is read off is raised by a replay that lands after the
        // connection does, so the engine holds the question until the replay
        // is over; answered where it then stands in the session's order, from
        // the counter as it stands there.
        if let Err(why) = self.send_control(&tx, ControlCommand::Ask(crate::types::Ask::NextValidId)) {
            return self.report_refusal(py, -1, Refusal::not_connected(why.to_string()));
        }
        let _ = num_ids;
        Ok(())
    }

    /// Reserve the next order ID above the saved counter and venue replay.
    /// The account and API client's counter is written under an exclusive
    /// lock before returning. A storage failure warns once in the log and
    /// leaves allocation using session memory and venue replay.
    fn next_order_id(&self, py: Python<'_>) -> i64 {
        self.take_order_id(py) as i64
    }

    /// The first id past everything the account has used that a request can
    /// also carry.
    ///
    /// A caller that numbers its orders and its requests out of one counter —
    /// which is how `ib_async` is written — needs both at once: clear of every
    /// id an order has spent, and inside the numbers a request can carry. An
    /// account that has been given a wider order id than that has no such
    /// number above it, so this answers with one past the widest the account
    /// has used that a request can carry, and the counting goes on from there.
    /// After a connect it waits, for at most three seconds in all, for the venue
    /// to name the orders the account is working.
    ///
    /// Raises RuntimeError where even that is not a number a request can
    /// carry. Answers 1 where there is no session.
    fn next_shared_id(&self, py: Python<'_>) -> PyResult<i64> {
        let Ok(shared) = self.shared_state() else { return Ok(1) };
        py.detach(|| crate::api::client::next_shared_id_of(&shared))
            .map_err(|refusal| PyRuntimeError::new_err(refusal.message))
    }

    /// Request all open orders for this client.
    ///
    /// An order the venue states held is among them only where this session
    /// placed it: a gateway holds an order it sent working whatever the venue
    /// states of it later, and leaves out one it first learns of held.
    fn req_open_orders(&self, py: Python<'_>) -> PyResult<()> {
        self.ask_open_orders(py, crate::types::model::Question::OpenOrders)
    }

    /// Request all open orders across all clients.
    ///
    /// The same answer as `req_open_orders`. A gateway narrows that one to the
    /// orders of the client asking, and this client does not.
    fn req_all_open_orders(&self, py: Python<'_>) -> PyResult<()> {
        self.ask_open_orders(py, crate::types::model::Question::AllOpenOrders)
    }

    /// Binding orders entered elsewhere to this client.
    ///
    /// Served for client 0. Any other client is refused, as a gateway refuses a
    /// client other than 0: asked to bind, with 321, as a request that fails
    /// validation; asked not to, with 327. On a gateway, client 0's flag turns
    /// binding on or off; here what binding asks for is the default, since this
    /// session is told about every order on the account, whoever entered it.
    /// So `b_auto_bind` changes nothing whichever way it is set, and nothing
    /// goes to the venue.
    ///
    /// `order_bound` does not follow from this call. It is fired once for each
    /// order the venue restates when the session opens that this session did
    /// not place, pairing its permanent id, the number the session that placed
    /// it sent it under, with the order id it is reached under here.
    #[pyo3(signature = (b_auto_bind))]
    fn req_auto_open_orders(&self, b_auto_bind: bool) -> PyResult<()> {
        // Nothing goes to the wire. The request is refused for any client id
        // but 0, and otherwise sets state that does not apply here: this
        // session is told about every order on the account whether or not it
        // placed them. The refusal is the only observable part.
        let Some(_tx) = self.tx_or_report(-1)? else { return Ok(()) };
        if self.client_id.load(std::sync::atomic::Ordering::Acquire) != 0 {
            let (code, reason) = if b_auto_bind {
                (crate::error_codes::Refusal::VALIDATION,
                 "only the client numbered zero can bind orders entered elsewhere")
            } else {
                (crate::error_codes::Refusal::AUTO_BIND_NOT_THIS_CLIENT,
                 "orders entered elsewhere are bound to the client numbered zero")
            };
            crate::python::compat::client::stubs::report_unserviceable_with(self, -1, code, reason);
        }
        Ok(())
    }

    /// Request execution reports.
    ///
    /// Before a session exists this is reported on the error callback, as
    /// every other request made before connecting is. Answered instead, the
    /// answer waits for a dispatch pass no session is there to make, and the
    /// caller hears nothing at all.
    ///
    /// `lastNDays` and `specificDates` select days as a gateway selects them,
    /// counted on the session's time zone. A date that does not read as a
    /// number, or is not a day of the calendar, is refused under 320, as a
    /// gateway refuses it. The executions answered from reach back to midnight
    /// six days before the logon in UTC, or to the logon's own day for a
    /// session set to today's executions, so the earliest days asked for can
    /// be missing some; those days are named on `error` under 321 ahead of the
    /// answer, which still comes. `acctCode` is ignored on a login holding one
    /// account and refused on one holding several where the login does not
    /// hold it, as a gateway does both.
    #[pyo3(signature = (req_id, exec_filter=None))]
    fn req_executions(&self, py: Python<'_>, req_id: i64, exec_filter: Option<Py<PyAny>>) -> PyResult<()> {
        let Some(_connected) = self.tx_or_report(req_id)? else { return Ok(()) };
        let filter = if let Some(ref fobj) = exec_filter {
            let get = |attr: &str| -> String {
                fobj.getattr(py, pyo3::types::PyString::new(py, attr))
                    .and_then(|v| v.extract::<String>(py))
                    .unwrap_or_default()
            };
            let get_i64 = |attr: &str| -> i64 {
                fobj.getattr(py, pyo3::types::PyString::new(py, attr))
                    .and_then(|v| v.extract::<i64>(py))
                    .unwrap_or_default()
            };
            // The reference leaves `lastNDays` at UNSET_INTEGER and
            // `specificDates` at None; an object without them reads as 0 and
            // none, which ask for no window either.
            let last_n_days = get_i64("lastNDays").clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            // Each written onto the wire as its text, as the reference writes
            // it, and read there as a whole number: one that does not read as
            // one refuses the request.
            let mut specific_dates = Vec::new();
            let stated = fobj.getattr(py, pyo3::types::PyString::new(py, "specificDates")).ok();
            if let Some(dates) = stated.filter(|v| !v.is_none(py)) {
                for date in dates.bind(py).try_iter()? {
                    let date = date?;
                    let text = match date.extract::<i64>() {
                        Ok(number) => number.to_string(),
                        Err(_) => date.str()?.to_string(),
                    };
                    let Ok(day) = text.parse::<i32>() else {
                        self.report_refusal(py, req_id, Refusal::stated(
                            crate::error_codes::REQUEST_NOT_READ,
                            format!(
                                "Error reading request: Unable to parse field: 'Trading Days' \
                                 for input string: '{text}'",
                            ),
                        ))?;
                        // The end still comes, as it does for every request
                        // refused on this surface.
                        return self.deliver(py, "exec_details_end", (req_id,));
                    };
                    specific_dates.push(day);
                }
            }
            ExecutionFilter {
                last_n_days,
                specific_dates,
                symbol: get("symbol"),
                sec_type: get("secType"),
                exchange: get("exchange"),
                // Either vocabulary: the comparison reads the order action
                // and the venue's word for the side alike, so this surface
                // sends what it was given.
                side: get("side"),
                acct_code: get("acctCode"),
                // Dropping these silently replayed executions the caller had
                // filtered out — another client's fills, or ones before the
                // requested cutoff.
                client_id: get_i64("clientId"),
                time: get("time"),
            }
        } else {
            ExecutionFilter::default()
        };

        // The executions kept, then the end, as they stand where the answer
        // stands in the session's order: every fill delivered before it is in
        // it.
        self.shared_state()?.push_call_record(crate::bridge::Record::Answer(
            crate::bridge::Answer::Executions { req_id, filter },
        ));
        Ok(())
    }

    /// Request completed orders.
    ///
    /// `api_only` asks for the orders entered through an API rather than by
    /// hand. The venue states no origin beside a finished order, and it does
    /// number the ones an API placed: an order that went out through one
    /// carries the number that API gave it, and one typed in carries none. So
    /// `true` is answered with the orders the venue numbered.
    #[pyo3(signature = (api_only=false))]
    fn req_completed_orders(&self, api_only: bool) -> PyResult<()> {
        let question = crate::types::model::Question::CompletedOrders;
        let refused = crate::types::model::ErrorOrigin::Question { q: question, ends: true };
        let Some(tx) = self.tx_or_report_as(refused)? else { return Ok(()) };
        // Asked of the venue, not only of this session: what finished while
        // this program was watching is a fraction of what the account has
        // done. The engine asks one question at a time, in the order they
        // were asked, and answers each where its end stands in the session's
        // order: a second asked while the first is out waits its turn.
        self.send_control(&tx, crate::types::ControlCommand::FetchCompletedOrders { api_only }).or_else(|why| Python::attach(|py| self.report_refusal_as(py, refused, crate::error_codes::Refusal::not_connected(why.to_string()))))
    }
}

/// This client's own helpers, kept out of the block above.
///
/// Everything in a `#[pymethods]` block is published, whether or not it names
/// anything the reference client has. A plain helper written there arrived on
/// the Python surface as a call of its own — and this one fabricates an error
/// at the caller's wrapper, so a program could be told a withdrawal's
/// annotation had not travelled when nothing had been withdrawn at all.
impl EClient {
    /// File what the venue has stated finished into the archive the answers
    /// are read from, where an answer is delivered.
    pub(crate) fn archive_completed_orders(&self, shared: &crate::bridge::SharedState) {
        // Read off the queue once and kept. It empties as it is read and
        // the venue does not send these again, so a second request would
        // otherwise be answered with none of them, and with default objects
        // for any whose record has been retired.
        {
            let mut archive = self.completed.lock().unwrap();
            // What the venue has taken back goes first. A trade cancel or
            // correction returns a finished order to a working quantity,
            // and the bridge can only drop the completion it still holds —
            // this is the copy it cannot reach. Applied before the
            // arrivals below, an order taken back and then finished again
            // keeps the new record and loses the superseded one.
            for (order_id, venue_order) in shared.orders.drain_order_corrections() {
                archive.retain(|(_, order, _, named)| {
                    order.order_id != order_id as i64 || *named != venue_order
                });
                // And the eviction armed for it when it finished. A bust or a
                // correction puts the order back to a working quantity, and an
                // eviction still standing took its record away on the next pass —
                // after which the order reads as one the venue is not working.
                self.deferred_evictions.lock().unwrap().remove(&order_id);
            }
            for co in shared.orders.drain_completed_orders() {
                let status_str = crate::types::order_status::order_status_str(co.status);
                // The order as the venue's answer stated it, where the
                // completion carries that, and otherwise the record held under
                // its number as the completion was taken, as on the other
                // surface. The number is not the order: the venue can have
                // finished two under one, and the record under it is whichever
                // this session holds.
                let tracked = co.stated.is_none()
                    .then(|| self.core.open_orders.lock().unwrap().get(&co.order_id).cloned())
                    .flatten();
                let rich_info = co.stated.or(co.held).map(|record| *record);
                // The state the venue stated where it stated one, under the
                // status this client names it by, which is canonical rather
                // than whatever the stored state last held.
                let state = crate::types::model::OrderState {
                    status: status_str.into(),
                    ..rich_info.as_ref().map(|(_, _, state)| state.clone()).unwrap_or_default()
                };
                // The venue's own order where it stated one, as on the
                // other surface, with the client that placed it where the
                // venue names none.
                let (contract, mut order) = match (tracked, rich_info) {
                    // The record's contract carries the legs and the hedge
                    // the caller stated, which no definition carries.
                    (Some(o), Some((_, order, _))) => (o.contract, order),
                    (None, Some((contract, order, _))) => (contract, order),
                    (Some(o), None) => (o.contract, o.order),
                    (None, None) => (
                        crate::types::model::Contract::default(),
                        crate::types::model::Order {
                            order_id: co.order_id as i64,
                            ..Default::default()
                        },
                    ),
                };
                if order.client_id == 0 {
                    order.client_id = self.core.placing_client(shared, co.order_id);
                }
                // Filled out from what the venue has said about the
                // contract, as the open-order answer already is on both
                // surfaces and as the other surface's completed answer is.
                // Taken verbatim here, an order lost its exchange, its
                // multiplier and its local symbol the moment it finished,
                // and only on this binding.
                let contract = if contract.con_id != 0 {
                    self.core.get_contract(contract.con_id, shared).unwrap_or(contract)
                } else {
                    contract
                };
                // Replaced where this order is already in the archive, as
                // on the other surface: the venue restates an order once
                // the memory of it has aged out, and pushed again the
                // caller was handed the same order twice. Under the venue's
                // own name for it: two orders the venue finished under one
                // number are two orders.
                match archive.iter().position(|(_, held, _, named): &(_, crate::types::model::Order, _, String)| {
                    held.order_id == order.order_id
                        && (held.perm_id == order.perm_id
                            || held.perm_id == 0
                            || order.perm_id == 0)
                        && *named == co.venue_order
                }) {
                    Some(at) => archive[at] = (contract, order, state, co.venue_order),
                    None => archive.push((contract, order, state, co.venue_order)),
                }
                // Bound `order_cache` growth: terminal entries are no
                // longer needed once what they carried has been read out.
                // Handed to the side that reads the fills rather than
                // freed here. That side is the only one that can tell when
                // a record is finished with: a fill taken off the queue but
                // not yet reported still needs it, and from here looks like
                // no fill at all.
                self.deferred_evictions.lock().unwrap().insert(co.order_id);
            }
        }
    }

    /// Every working order, then `open_order_end`, answered where the answer
    /// stands in the session's order.
    pub(crate) fn ask_open_orders(&self, py: Python<'_>, question: crate::types::model::Question) -> PyResult<()> {
        let refused = crate::types::model::ErrorOrigin::Question { q: question, ends: true };
        let Some(tx) = self.tx_or_report_as(refused)? else { return Ok(()) };
        // The venue names the working orders unprompted after a connect.
        // Answering before that replay lands reports none of them, and a
        // caller that reads "nothing" places the same order twice. The engine
        // holds the question until the replay is over, or its bound has
        // passed — said ahead of the answer where the venue had begun naming
        // and not finished — and the orders and the end stand where the answer
        // does in the session's order.
        if let Err(why) = self.send_control(&tx, ControlCommand::Ask(crate::types::Ask::OpenOrders(question))) {
            return self.report_refusal_as(py, refused, Refusal::not_connected(why.to_string()));
        }
        Ok(())
    }


}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::SharedState;
    use std::sync::Arc;

    /// A connected client whose engine takes what the test hands it, and a
    /// wrapper that keeps every callback it is handed.
    fn wired_client(
        py: Python<'_>,
    ) -> (EClient, crate::api::client::tests::Engine, Arc<SharedState>, Py<PyAny>) {
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
        let shared = Arc::new(SharedState::new());
        let (tx, rx) = std::sync::mpsc::channel();
        *client.shared.lock().unwrap() = Some(shared.clone());
        *client.control_tx.lock().unwrap() = Some(tx);
        *client.account_id.lock().unwrap() = Some("DU123".into());
        client.connected.store(true, Ordering::Release);
        let rx = crate::api::client::tests::Engine::new(rx, &shared);
        (client, rx, shared, wrapper)
    }

    /// The venue names the working orders after the connect returns, and a
    /// global cancel is composed from what has been named. Issued before the
    /// naming lands, it waits for it — without the wait it counted no
    /// instruments, sent nothing, and returned without an error.
    #[test]
    fn a_global_cancel_waits_for_the_venue_to_name_the_working_orders() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, shared, _wrapper) = wired_client(py);
            let venue = shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                venue.market.set_instrument_count(1);
                venue.orders.set_replay_done();
            });
            client.req_global_cancel(py, None).unwrap();
            let sent: Vec<ControlCommand> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            assert!(
                matches!(sent.as_slice(), [ControlCommand::Order(OrderRequest::GlobalCancel { instruments, .. })] if instruments == &[0]),
                "the order the venue named is withdrawn: {sent:?}",
            );
        });
    }

    /// The numbers a Python session hands out, and the ones its caller places
    /// under, are there for the next session on the same file.
    #[test]
    fn a_python_session_saves_the_order_ids_it_spends() {
        Python::initialize();
        Python::attach(|py| {
            let scratch = crate::api::client::tests::Scratch::new("python-order-ids");
            let path = scratch.0.join("ids.json");
            for (placed, expected) in [(Some(80), 1), (None, 81), (None, 82)] {
                let (client, _rx, shared, _wrapper) = wired_client(py);
                shared.orders.open_order_ids(Some(&path), "DU123");
                shared.orders.set_replay_done();
                assert_eq!(client.take_order_id(py), expected);
                if let Some(id) = placed {
                    client.place_order(py, id, &bracket_contract(), &bracket_order(true, 0)).unwrap();
                }
            }
        });
    }

    /// A family numbered from the ids this session hands out is placed under
    /// them, however wide the account's ids have grown: the children take an
    /// id as the parent does.
    #[test]
    fn attached_children_take_the_ids_the_session_hands_out() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, shared, _wrapper) = wired_client(py);
            crate::api::client::tests::attach_from_the_selected_preset(&shared);
            shared.orders.set_replay_done();
            shared.orders.note_the_venue_named(1_787_685_160_171_345);
            let [parent, stop, profit] = [(); 3].map(|_| client.take_order_id(py));
            let order = Py::new(py, Order {
                override_percentage_constraints: true,
                lmt_price: 100.0,
                sl_order_type: "PRESET".into(),
                pt_order_type: "PRESET".into(),
                ..bracket_order(true, 0)
            }).unwrap();
            order.bind(py).setattr("slOrderId", stop).unwrap();
            order.bind(py).setattr("ptOrderId", profit).unwrap();
            client.place_order(py, parent as i64, &bracket_contract(), &order.borrow(py)).unwrap();
            let sent: Vec<u64> = rx.try_iter().filter_map(|command| match command {
                ControlCommand::Order(request @ OrderRequest::SubmitEx { .. }) => Some(request.order_id()),
                _ => None,
            }).collect();
            assert_eq!(sent, [parent, stop, profit]);
        });
    }

    /// An order held back for a later transmit was never given to the venue,
    /// so a withdrawal of everything forgets it rather than sending a cancel
    /// the venue would refuse — left held, it would go out behind the next
    /// thing that transmits, after the caller had asked for everything back.
    #[test]
    fn a_global_cancel_forgets_the_orders_held_for_a_later_transmit() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, shared, _wrapper) = wired_client(py);
            shared.orders.set_replay_done();
            client.place_order(py, 7, &bracket_contract(), &bracket_order(false, 0)).unwrap();
            assert!(rx.keeps(7), "held for a later transmit");
            client.req_global_cancel(py, None).unwrap();
            assert!(!rx.keeps(7), "nothing is still held");
            let sent: Vec<ControlCommand> = rx.try_iter().collect();
            assert!(
                !sent.iter().any(|c| matches!(c, ControlCommand::Order(OrderRequest::SubmitEx { .. }))),
                "and nothing of it goes out: {sent:?}",
            );
        });
    }

    /// A global cancel issued before the venue has finished naming the
    /// working orders is not answered in silence: what had been named is
    /// still withdrawn, and the caller is told on the error callback that
    /// the naming had not finished — a partial cancel that reads as one,
    /// rather than as a complete answer that is not.
    #[test]
    fn a_global_cancel_says_when_the_venue_has_not_finished_naming() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, shared, wrapper) = wired_client(py);
            shared.market.set_instrument_count(1);
            // The venue began naming and never said it had finished: the wait
            // runs out with something named and something possibly not. An
            // account named nothing at all is the other case, and is told
            // nothing — there is no uncovered order to warn about.
            shared.orders.note_naming_began();
            client.req_global_cancel(py, None).unwrap();
            let sent: Vec<ControlCommand> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            assert!(
                matches!(sent.as_slice(), [ControlCommand::Order(OrderRequest::GlobalCancel { instruments, .. })] if instruments == &[0]),
                "what had been named is still withdrawn: {sent:?}",
            );
            // Said where it stands in the session's order: a read delivers it.
            client.dispatch_once(py, &shared).unwrap();
            let calls = wrapper.bind(py).getattr("calls").unwrap();
            let heard: Vec<(String, i64, i64, i64, String, String)> = (0..calls.len().unwrap())
                .filter_map(|i| calls.get_item(i).unwrap().extract().ok())
                .collect();
            assert!(
                heard.iter().any(|(name, req_id, _, code, message, _)| {
                    name == "error"
                        && *req_id == -1
                        && *code == crate::error_codes::Refusal::NO_ANSWER as i64
                        && message.contains("had not finished naming")
                }),
                "the caller is told on the error callback what was and was not covered: {heard:?}",
            );
        });
    }

    /// Asking what the account is working before the venue has finished
    /// naming it answers with what had arrived, and that is what an account
    /// working nothing looks like. The caller is told which of the two it is
    /// reading, so a strategy does not take a partial snapshot for a flat
    /// account and place again what it already has on.
    #[test]
    fn open_orders_say_when_the_snapshot_is_not_known_to_be_whole() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, shared, wrapper) = wired_client(py);
            // The venue began naming and never said it had finished. An
            // account named nothing at all is the other case, and is told
            // nothing — there is no missing order to warn about.
            shared.orders.note_naming_began();
            client.req_open_orders(py).unwrap();
            crate::api::client::tests::the_engine_answers(&rx, &shared);
            client.dispatch_once(py, &shared).unwrap();
            let calls = wrapper.bind(py).getattr("calls").unwrap();
            let told: Vec<(String, i64, i64, i64, String, String)> = (0..calls.len().unwrap())
                .filter_map(|i| calls.get_item(i).unwrap().extract().ok())
                .collect();
            assert!(
                told.iter().any(|(name, req_id, _, code, message, _)| {
                    name == "error"
                        && *req_id == -1
                        && *code == crate::error_codes::Refusal::NO_ANSWER as i64
                        && message.contains("had not finished naming")
                }),
                "the caller is told the snapshot is not known to be whole: {told:?}",
            );
        });
    }

    /// An order placed under client zero keeps that client id when another
    /// session reads it back, and an order this session placed reads under
    /// the client it went out under.
    ///
    /// Zero is a client, not an absence: restated as the observer's own, an
    /// order placed elsewhere read as one this session held under the same
    /// id, and whatever this session held under that id read as another's.
    #[test]
    fn an_order_placed_by_client_zero_keeps_that_client_id_when_read_back() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, shared, wrapper) = wired_client(py);
            shared.orders.set_replay_done();
            client.client_id.store(7, Ordering::Release);
            client.core.set_api_client_id(7);
            shared.orders.set_api_client_id(7);
            client.core.con_id_to_instrument.lock().unwrap().insert(756733, 0);
            // Placed by this session, held back from the venue.
            client.place_order(py, 3, &bracket_contract(), &bracket_order(false, 0)).unwrap();
            // Placed under client zero, as the venue reports one.
            client.core.track_order(
                4,
                crate::types::model::Contract {
                    con_id: 756733, symbol: "SPY".into(), ..Default::default()
                },
                crate::types::model::Order {
                    order_id: 4, action: "BUY".into(), total_quantity: 1.0,
                    order_type: "LMT".into(), lmt_price: 10.0, ..Default::default()
                },
                0,
            );
            client.req_open_orders(py).unwrap();
            crate::api::client::tests::the_engine_answers(&rx, &shared);
            client.dispatch_once(py, &shared).unwrap();
            let calls = wrapper.bind(py).getattr("calls").unwrap();
            let mut read_back = std::collections::BTreeMap::new();
            for i in 0..calls.len().unwrap() {
                let call = calls.get_item(i).unwrap();
                let name: String = call.get_item(0).unwrap().extract().unwrap();
                if name != "openOrder" {
                    continue;
                }
                let order_id: i64 = call.get_item(1).unwrap().extract().unwrap();
                let client_id: i64 = call.get_item(3).unwrap()
                    .getattr("clientId").unwrap().extract().unwrap();
                read_back.insert(order_id, client_id);
            }
            assert_eq!(
                read_back.get(&3), Some(&7),
                "the order this session placed reads under the client it went out under: {read_back:?}",
            );
            assert_eq!(
                read_back.get(&4), Some(&0),
                "the order placed under client zero is not restated as this session's: {read_back:?}",
            );
        });
    }

    /// A quote feed the engine has given up on does not stop this surface
    /// withdrawing a live order.
    ///
    /// Every request here read the session's flag, which any transport being
    /// given up on raises — so an order the trading connection would have
    /// carried was refused because the prices had stopped, and a caller could
    /// not withdraw what it already had working. The other surface reads the
    /// trading connection's own state before it takes an order, and this one
    /// now reads the same thing.
    #[test]
    fn a_quote_feed_that_ended_does_not_stop_an_order_being_withdrawn() {
        Python::initialize();
        Python::attach(|py| {
            let (client, shared, wrapper) = placed_client(py);
            let (tx, rx) = std::sync::mpsc::channel::<ControlCommand>();
            *client.control_tx.lock().unwrap() = Some(tx);
            let rx = crate::api::client::tests::Engine::new(rx, &shared);

            // A withdrawal names an order the venue is working, or it is
            // answered rather than sent under a number the venue never gave.
            for order_id in [11, 12] {
                shared.orders.push_order_info(order_id, crate::bridge::RichOrderInfo {
                    contract: crate::types::model::Contract {
                        con_id: 756733, symbol: "SPY".into(), ..Default::default()
                    },
                    order: crate::types::model::Order { order_id: order_id as i64, ..Default::default() },
                    order_state: crate::types::model::OrderState {
                        status: "Submitted".into(), ..Default::default()
                    },
                    last_exec: Default::default(),
                });
            }

            shared.reference.set_session_over("the market data farm");
            client.cancel_order(py, 11, None).unwrap();
            assert!(
                matches!(
                    rx.try_recv(),
                    Ok(ControlCommand::Order(OrderRequest::Cancel { order_id: 11, .. })),
                ),
                "the trading connection still carries the withdrawal",
            );
            assert!(error_calls(py, &client, &wrapper).is_empty(), "and nothing is reported");

            // Once that connection is the one that has ended, it is refused.
            shared.reference.set_trading_over("the trading connection");
            client.cancel_order(py, 12, None).unwrap();
            assert!(rx.try_recv().is_err(), "nothing reaches the engine");
            assert_eq!(
                error_calls(py, &client, &wrapper).len(), 1,
                "and the caller is told, on the callback a refusal is reported on",
            );
        });
    }

    /// A connected client whose engine is a channel the test drives, with a
    /// wrapper that keeps every callback it is handed.
    fn placed_client(py: Python<'_>) -> (EClient, Arc<SharedState>, Py<PyAny>) {
        let ns = pyo3::types::PyDict::new(py);
        py.run(
            c"class W:
    def __init__(self): self.calls = []
    def __getattr__(self, name):
        return lambda *args: self.calls.append((name,) + args)
w = W()",
            None,
            Some(&ns),
        )
        .unwrap();
        let wrapper = ns.get_item("w").unwrap().unwrap().unbind();
        let client = EClient::__new__(&pyo3::types::PyTuple::empty(py), None);
        client.__init__(wrapper.clone_ref(py)).unwrap();
        let shared = Arc::new(SharedState::new());
        *client.shared.lock().unwrap() = Some(shared.clone());
        *client.account_id.lock().unwrap() = Some("DU123".into());
        client.connected.store(true, Ordering::Release);
        // No venue behind a test session, so the replay of what the account
        // already has on is over before it starts. A withdrawal reads that
        // replay before deciding whether it names anything.
        shared.orders.set_replay_done();
        (client, shared, wrapper)
    }

    /// The contract the bracket tests place their orders on.
    fn bracket_contract() -> Contract {
        Contract {
            con_id: 756733,
            symbol: "IBM".into(),
            sec_type: "STK".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            ..Default::default()
        }
    }

    /// One order of the bracket tests.
    fn bracket_order(transmit: bool, parent_id: i64) -> Order {
        Order {
            action: "BUY".into(),
            total_quantity: 100.0,
            order_type: "LMT".into(),
            lmt_price: 10.0,
            transmit,
            parent_id,
            ..Default::default()
        }
    }

    /// An order placed by description goes out, is recorded and is cached
    /// under the contract the venue named.
    ///
    /// The reference client's own examples place by symbol. The submit was
    /// built from the caller's description, which carries no contract id, so
    /// it passed the engine's check that the slot still holds that contract
    /// unread, as no order from the other surface does. The record and the
    /// cache are read back beside it: one contract serves all three, and a
    /// key that parts from its value is the same defect under another name.
    #[test]
    fn an_order_placed_by_description_carries_the_contract_the_venue_named() {
        Python::initialize();
        Python::attach(|py| {
            let (client, shared, _wrapper) = placed_client(py);
            let (tx, rx) = std::sync::mpsc::channel::<ControlCommand>();
            *client.control_tx.lock().unwrap() = Some(tx);
            let rx = crate::api::client::tests::Engine::new(rx, &shared);
            let described = Contract { con_id: 0, ..bracket_contract() };
            // Named already, so the lookup is answered from the record rather
            // than the venue: the question is what the naming is used for.
            let key = crate::client_core::ClientCore::description_key(&described.to_api());
            rx.engine().intake.remember_named(key, bracket_contract().to_api());
            client.place_order(py, 9, &described, &bracket_order(true, 0)).unwrap();
            let sent = rx.try_recv();
            client.dispatch_once(py, &shared).unwrap();
            assert!(
                matches!(sent, Ok(ControlCommand::Order(OrderRequest::SubmitEx { con_id: 756733, order_id: 9, .. }))),
                "the submit names the contract the venue named: {sent:?}",
            );
            let recorded = client.core.open_orders.lock().unwrap().get(&9).map(|o| o.contract.con_id);
            assert_eq!(recorded, Some(756733), "and so does the record");
            let cached = client.core.contract_cache.lock().unwrap().get(&756733).map(|c| c.con_id);
            assert_eq!(cached, Some(756733), "and the cache holds that contract under its own number");
        });
    }

    /// The error callbacks the wrapper was handed, as id, code and message.
    fn error_calls(py: Python<'_>, client: &EClient, wrapper: &Py<PyAny>) -> Vec<(i64, i64, String)> {
        // A refusal is a record in the session's order: a read delivers it.
        if let Ok(shared) = client.shared_state() {
            client.dispatch_once(py, &shared).unwrap();
        }
        let all: Vec<(String, i64, i64, i64, String, String)> = wrapper
            .getattr(py, "calls")
            .unwrap()
            .extract(py)
            .unwrap();
        all.into_iter()
            .filter(|(name, _, _, _, _, _)| name == "error")
            .map(|(_, id, _stamp, code, message, _)| (id, code, message))
            .collect()
    }

    /// A bracket whose engine takes the whole family: all three go out, in
    /// the order they were placed, and nothing is left held behind them.
    #[test]
    fn a_bracket_whose_engine_takes_the_family_sends_all_three_in_order() {
        Python::initialize();
        Python::attach(|py| {
            let (client, shared, wrapper) = placed_client(py);
            let (tx, rx) = std::sync::mpsc::channel::<ControlCommand>();
            *client.control_tx.lock().unwrap() = Some(tx);
            let rx = crate::api::client::tests::Engine::new(rx, &shared);
            client.place_order(py, 3, &bracket_contract(), &bracket_order(false, 0)).unwrap();
            client.place_order(py, 4, &bracket_contract(), &bracket_order(false, 3)).unwrap();
            client.place_order(py, 5, &bracket_contract(), &bracket_order(true, 3)).unwrap();
            let received: Vec<ControlCommand> = rx.try_iter().collect();
            assert!(
                matches!(
                    received.as_slice(),
                    [
                        ControlCommand::Order(OrderRequest::SubmitEx { con_id: 756733, order_id: 3, .. }),
                        ControlCommand::Order(OrderRequest::SubmitEx { con_id: 756733, order_id: 4, .. }),
                        ControlCommand::Order(OrderRequest::SubmitEx { con_id: 756733, order_id: 5, .. }),
                    ],
                ),
                "the family goes in the order it was placed: {received:?}",
            );
            assert!(error_calls(py, &client, &wrapper).is_empty(), "a family that went is not reported");
            assert!(!rx.keeps(3), "nothing is held after the transmit");
            assert!(!rx.keeps(4), "nothing is held after the transmit");
        });
    }


    /// A placement the engine can no longer take is told on the error
    /// callback under its own number, rather than handed over as an
    /// exception.
    #[test]
    fn a_placement_the_engine_can_no_longer_take_is_reported_under_its_number() {
        Python::initialize();
        Python::attach(|py| {
            let (client, _shared, wrapper) = placed_client(py);
            let (tx, rx) = std::sync::mpsc::channel::<ControlCommand>();
            *client.control_tx.lock().unwrap() = Some(tx);
            // The engine has stopped: nothing takes what is placed.
            drop(rx);
            client.place_order(py, 3, &bracket_contract(), &bracket_order(false, 0)).unwrap();
            client.place_order(py, 4, &bracket_contract(), &bracket_order(true, 3)).unwrap();
            let errors = error_calls(py, &client, &wrapper);
            let told: Vec<(i64, i64)> = errors.iter().map(|(id, code, _)| (*id, *code)).collect();
            assert_eq!(told, [(3, 504), (4, 504)], "each is told on the error callback: {errors:?}");
            assert!(errors.iter().all(|(_, _, message)| message.contains("Engine stopped")), "{errors:?}");
        });
    }

    /// Cancelling an order the venue was never given forgets it, rather than
    /// asking the venue to withdraw something it does not have.
    ///
    /// Sent, the venue answers that it knows no such order and the command
    /// stays queued to go out behind the next thing that transmits: a caller
    /// that cancelled a parent and then sent its stop-loss had the parent it
    /// had cancelled placed for it.
    #[test]
    fn cancelling_a_held_order_forgets_it_rather_than_placing_it_later() {
        Python::initialize();
        Python::attach(|py| {
            let (client, shared, _wrapper) = placed_client(py);
            let (tx, rx) = std::sync::mpsc::channel::<ControlCommand>();
            *client.control_tx.lock().unwrap() = Some(tx);
            let rx = crate::api::client::tests::Engine::new(rx, &shared);

            client.place_order(py, 3, &bracket_contract(), &bracket_order(false, 0)).unwrap();
            client.cancel_order(py, 3, None).unwrap();
            client.place_order(py, 4, &bracket_contract(), &bracket_order(true, 0)).unwrap();

            let sent: Vec<ControlCommand> = std::iter::from_fn(|| rx.try_recv().ok())
                .filter(|c| matches!(c, ControlCommand::Order(_)))
                .collect();
            for cmd in &sent {
                if let ControlCommand::Order(OrderRequest::SubmitEx { order_id, .. }) = cmd {
                    assert_ne!(
                        *order_id, 3,
                        "the order that was cancelled is not placed later: {sent:?}",
                    );
                }
                assert!(
                    !matches!(cmd, ControlCommand::Order(OrderRequest::Cancel { order_id: 3, .. })),
                    "and the venue is not asked to withdraw one it never had: {sent:?}",
                );
            }
            assert!(!rx.keeps(3), "nothing of it is left queued");
            client.dispatch_once(py, &shared).unwrap();
            assert!(!client.core.is_order_tracked(3), "and its id no longer reads as an order");
        });
    }




    /// A combination leg the caller states and this client cannot read is a
    /// refusal, as the other unreadable fields are, and is answered on the
    /// error callback: the exception this used to raise was somewhere a
    /// caller written against the reference client has no handling.
    #[test]
    fn an_unreadable_combination_leg_is_reported_and_the_call_returns() {
        Python::initialize();
        Python::attach(|py| {
            let (client, _shared, wrapper) = placed_client(py);
            let (tx, _rx) = std::sync::mpsc::channel::<ControlCommand>();
            *client.control_tx.lock().unwrap() = Some(tx);
            let contract = bracket_contract();
            // A leg with no contract id: it names no contract, and the list
            // is refused.
            let leg = py.eval(c"type('ComboLeg', (), {})()", None, None).unwrap();
            contract.combo_legs.bound(py).append(leg).unwrap();
            client.place_order(py, 3, &contract, &bracket_order(true, 0)).unwrap();
            let errors = error_calls(py, &client, &wrapper);
            let (id, code, message) = errors.last().expect("the caller is told on the error callback");
            assert_eq!(*id, 3);
            assert_eq!(*code, Refusal::VALIDATION as i64);
            assert!(message.contains("combo leg 0 has no conId"), "{message}");
        });
    }

    /// The working orders carry what the venue said their fills went at.
    ///
    /// The venue states an average on every report, and this client holds it.
    /// Answered as nought, a caller read "filled three hundred at an average
    /// of nothing" — a reading it cannot tell from a real one — while the live
    /// path on this same surface publishes the stated figure, so one order read
    /// two ways depending on which callback the caller happened to see.
    #[test]
    fn the_working_orders_carry_what_the_venue_said_their_fills_went_at() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, shared, wrapper) = wired_client(py);
            shared.orders.set_replay_done();
            shared.orders.push_order_info(77, crate::bridge::RichOrderInfo {
                contract: crate::types::model::Contract {
                    symbol: "SPY".into(), sec_type: "STK".into(), ..Default::default()
                },
                order: crate::types::model::Order {
                    order_id: 77, action: "BUY".into(), total_quantity: 500.0,
                    filled_quantity: 300.0, order_type: "LMT".into(), lmt_price: 100.0,
                    ..Default::default()
                },
                order_state: crate::types::model::OrderState {
                    status: "Submitted".into(), ..Default::default()
                },
                last_exec: crate::types::model::Execution {
                    avg_price: 99.5, price: 99.75, cum_qty: 300.0, ..Default::default()
                },
            });

            client.req_open_orders(py).unwrap();
            crate::api::client::tests::the_engine_answers(&rx, &shared);
            client.dispatch_once(py, &shared).unwrap();

            let calls = wrapper.bind(py).getattr("calls").unwrap();
            let said = (0..calls.len().unwrap())
                .map(|i| calls.get_item(i).unwrap())
                .find(|call| {
                    let name = call.get_item(0).unwrap().extract::<String>().unwrap();
                    name == "order_status" || name == "orderStatus"
                })
                .map(|call| (
                    call.get_item(5).unwrap().extract::<f64>().unwrap(),
                    call.get_item(8).unwrap().extract::<f64>().unwrap(),
                ))
                .expect("the order is reported");
            assert_eq!(
                said, (99.5, 99.75),
                "the average of everything filled, and what the last of it went at",
            );
        });
    }

    /// One venue order is answered once, whatever it is named along the way.
    ///
    /// A record can hold an order under no permanent id, where nothing has
    /// named the order's number to it yet. A caller released then holds a copy
    /// under none, and the answer that arrives afterwards carries the same
    /// order with one — the later answer supersedes the earlier, it does not
    /// join it.
    #[test]
    fn an_order_named_permanently_after_it_was_answered_replaces_its_earlier_copy() {
        Python::initialize();
        Python::attach(|py| {
            let (client, _rx, shared, wrapper) = wired_client(py);
            let known = |perm_id: i64| crate::bridge::RichOrderInfo {
                contract: Default::default(),
                order: crate::types::model::Order {
                    order_id: 31, perm_id, ..Default::default()
                },
                order_state: Default::default(),
                last_exec: Default::default(),
            };
            let finished = || crate::types::CompletedOrder {
                venue_order: String::new(), stated: None, held: None,
                order_id: 31, instrument: 0, status: crate::types::OrderStatus::Filled,
                filled_qty: 100, timestamp_ns: 0,
            };
            // The engine's side of the question: the answer, where the end
            // of the run of reports stands.
            let answering = |shared: &SharedState| {
                shared.push_call_record(crate::bridge::Record::Answer(
                    crate::bridge::Answer::CompletedOrders { api_only: false },
                ));
            };
            // Answered where the answer stands: a read delivers it.
            let answers = |client: &EClient| {
                client.dispatch_once(py, &shared).unwrap();
                let calls = wrapper.bind(py).getattr("calls").unwrap();
                let said = (0..calls.len().unwrap())
                    .filter(|i| {
                        let name = calls.get_item(*i).unwrap().get_item(0).unwrap()
                            .extract::<String>().unwrap();
                        name == "completed_order" || name == "completedOrder"
                    })
                    .count();
                calls.call_method0("clear").unwrap();
                said
            };

            shared.orders.push_order_info(31, known(0));
            shared.orders.push_completed_order(finished());
            client.req_completed_orders(false).unwrap();
            answering(&shared);
            assert_eq!(answers(&client), 1);

            shared.orders.push_order_info(31, known(777));
            shared.orders.refile_completed_order(finished());
            client.req_completed_orders(false).unwrap();
            answering(&shared);
            assert_eq!(
                answers(&client), 1,
                "the one order read as two once the venue had named it",
            );
        });
    }

    /// Every order the venue states finished comes back on this surface too,
    /// however many were sent under one number: its archive is its own, and
    /// held by the number it kept one order per number.
    #[test]
    fn every_order_the_venue_has_finished_comes_back_though_numbers_repeat() {
        Python::initialize();
        Python::attach(|py| {
            let (client, rx, shared, wrapper) = wired_client(py);
            client.req_completed_orders(false).unwrap();
            {
                let mut engine = rx.engine();
                let engine = &mut *engine;
                engine.ccp.completed_orders_open = true;
                for frame in crate::api::client::tests::A_FINISHED_ANSWER.lines() {
                    engine.ccp.process_ccp_message(
                        frame.replace('|', "\x01").as_bytes(), &mut None, &mut engine.context,
                        &shared, &None, &mut crate::engine::hot_loop::HeartbeatState::new(), "DU123",
                    );
                }
            }
            let heard = || {
                client.dispatch_once(py, &shared).unwrap();
                let calls = wrapper.bind(py).getattr("calls").unwrap();
                let mut heard: Vec<(i64, String)> = (0..calls.len().unwrap())
                    .map(|i| calls.get_item(i).unwrap())
                    .filter(|call| {
                        let name = call.get_item(0).unwrap().extract::<String>().unwrap();
                        name == "completed_order" || name == "completedOrder"
                    })
                    .map(|call| (
                        call.get_item(2).unwrap().getattr("permId").unwrap().extract().unwrap(),
                        call.get_item(3).unwrap().getattr("completedTime").unwrap().extract().unwrap(),
                    ))
                    .collect();
                calls.call_method0("clear").unwrap();
                heard.sort();
                heard
            };
            let order = |perm_id: i64, time: &str| (perm_id, time.to_string());
            assert_eq!(heard(), [
                order(1787685160171345, "20260924-14:02:28"),
                order(1787685160171345, "20260924-14:04:18"),
                order(1787685160171345, "20260924-14:04:54"),
                order(1787685160171345, "20260924-14:10:15"),
                order(1787685160171345, "20260924-15:01:48"),
                order(1787685160171371, "20260925-09:55:06"),
                order(1787685160171371, "20260925-15:01:02"),
            ], "each order the venue finished, once");

            // The venue takes one of them back: the other under its number
            // stays finished.
            shared.orders.push_order_correction(
                1787685160171371, "00a0b0c0.0000d0e0.0000b002",
                crate::bridge::RichOrderInfo {
                    contract: Default::default(), order: Default::default(),
                    order_state: Default::default(), last_exec: Default::default(),
                },
            );
            client.req_completed_orders(false).unwrap();
            shared.push_call_record(crate::bridge::Record::Answer(
                crate::bridge::Answer::CompletedOrders { api_only: false },
            ));
            assert_eq!(
                heard().into_iter().filter(|(perm_id, _)| *perm_id == 1787685160171371).collect::<Vec<_>>(),
                [order(1787685160171371, "20260925-09:55:06")],
                "the order taken back leaves, and the other under its number stays",
            );
        });
    }
}
