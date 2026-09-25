//! Order placement, cancellation, execution replay, and algo parsing.

use std::sync::atomic::Ordering;

use crate::error_codes::Refusal;
use crate::types::model::{ExecutionFilter, OrderOp};
use crate::client_core::ClientCore;
use crate::types::*;

use super::{Contract, Order, EClient};

impl EClient {
    // ── Orders ──

    /// Refuse a security type the venue does not permit this account to trade.
    ///
    /// The venue states its permissions at logon, keyed by security type, and it
    /// refuses an order on an unpermitted type by returning it Inactive with no
    /// text at all — so without this check the caller is told nothing. Silence
    /// here is not permission: when the venue stated no permissions, there is
    /// nothing to enforce and the order goes.
    fn check_sec_type_permitted(&self, sec_type: &str) -> Result<(), Refusal> {
        // The validator's own words under its own number, not flattened onto
        // the general one: a caller branches on which refusal it got.
        ClientCore::refuse_unpermitted_sec_type(
            &self.shared.reference.order_permissions(), sec_type,
        )
    }

    /// Security type → the order types the venue permits for it, as stated at
    /// logon. Empty until the session is up.
    pub fn order_permissions(&self) -> std::collections::HashMap<String, Vec<String>> {
        self.shared.reference.order_permissions()
    }

    /// The order types permitted for one security type, or `None` when the type
    /// is not permitted at all. A combination is named `COMB`.
    pub fn permitted_order_types(&self, sec_type: &str) -> Option<Vec<String>> {
        self.shared.reference.permitted_order_types(&sec_type.to_ascii_uppercase())
    }

    /// Feature tokens the venue enables for this account: those stated at
    /// logon, and those the account configuration adds afterwards.
    pub fn enabled_features(&self) -> Vec<String> {
        self.shared.reference.enabled_features()
    }

    /// What this session says about an order that a gateway checks it
    /// against: the account, every account the login holds, and what the
    /// venue enabled.
    pub(crate) fn order_session(&self) -> crate::client_core::OrderSession {
        let (logon_accounts, advisor) = self.shared.reference.login();
        crate::client_core::OrderSession {
            account: self.account_id.clone(),
            accounts: self.accounts.clone(),
            logon_accounts,
            advisor,
            features: self.shared.reference.enabled_features(),
        }
    }

    /// Which algorithms the venue offers, keyed `PROVIDER/SECTYPE`.
    ///
    /// Stated on the session rather than per contract. An algorithm absent
    /// here is one this account may not use, and an order naming it is
    /// refused by the venue.
    pub fn algorithms(&self) -> std::collections::HashMap<String, Vec<String>> {
        self.shared.reference.algorithms()
    }

    /// The algorithms offered for one security type, across every provider.
    pub fn algorithms_for(&self, sec_type: &str) -> Vec<String> {
        self.shared.reference.algorithms_for(sec_type)
    }

    /// The sets of order defaults this account holds, as
    /// `(key, attributes, when it last changed)`.
    ///
    /// The venue keeps one per security type and fills parts of an order the
    /// caller left unstated from them: the size a compete order competes with
    /// and the offset it competes by, where neither was named. So the same
    /// call on two accounts is not the same order, and the reference client's
    /// surface has no way to say which sets are in force.
    ///
    /// The key is the venue's own — `s=STK`, or `s=CASH&tc=EUR` where a
    /// currency splits it. The attributes are as the venue writes them, `&`
    /// between them: `v=` names the set's variant and `a=1` marks it active,
    /// so `v=1&a=1` is an active set and `v=1` one that is not. The values in
    /// a set are asked for separately and are not carried here.
    ///
    /// The moment says *when* a set last changed, which is what tells a caller
    /// whether an order it sent at a given time was filled in from the old
    /// defaults or the new. Empty where the venue stated none.
    pub fn order_presets(&self) -> Vec<(String, String, String)> {
        self.shared.reference.order_presets()
    }

    /// Refuse where the trading connection has stopped being retried.
    ///
    /// Gated on that connection's own state rather than the session's: the
    /// session flag is set by any transport ending, the market-data farm
    /// included, and refusing on it would refuse what a live trading
    /// connection would have carried. Where the trading connection itself has
    /// stopped, anything given here is recorded and buffered and never sent,
    /// and the caller is told it did something the venue never saw.
    fn refuse_if_trading_is_over(&self, what: &str) -> Result<(), Refusal> {
        match self.shared.reference.trading_over() {
            Some(why) => Err(Refusal::not_connected(format!(
                "the trading connection has ended and is not being retried ({why}), so \
                 {what} given here would be recorded and never sent: open a session again",
            ))),
            None => Ok(()),
        }
    }

    /// Place an order. Matches `placeOrder` in C++.
    ///
    /// An order names its contract by the venue's id. A caller who states a
    /// description instead of an id — which every example written against
    /// the reference client does — has it named by the engine, once the order
    /// itself is known to be one the venue would take: an order that names no
    /// contract is one the venue has nothing to match, and answers with
    /// nothing at all. Once per description: the answer is kept, and later
    /// orders on the same contract are sent without asking again.
    ///
    /// Nothing waits here. What needs no venue is checked at the call and a
    /// refusal of it is delivered in its place in the session's order; the
    /// engine then names and registers the contract, checks the order against
    /// what this session placed and what the venue is working — a number
    /// already finished, a replace naming another contract, a change a
    /// gateway refuses — builds it, and sends it or keeps it, and refuses it
    /// under its own number where it will not. A change of an order the
    /// engine has not sent yet goes after it, and its withdrawal withdraws it.
    ///
    /// `sl_order_id` / `sl_order_type` and `pt_order_id` / `pt_order_type`
    /// construct stop-loss and profit-taking children from the selected account
    /// preset. State the child id with `PRESET` (case-insensitive). The engine
    /// loads the preset, builds the children once and sends the parent, stop
    /// loss and profit taker in that order. `transmit = false` holds the family
    /// until a parent or child transmits it. Replacing a parent changes that
    /// order and preserves its existing children. Percentage-allocation
    /// sizing from group or model holdings is not carried; use explicitly
    /// sized parent and child orders where their quantities depend on it.
    pub fn place_order(&self, order_id: i64, contract: &Contract, order: &Order) {
        if let Err(why) = self.try_place_order(order_id, contract, order) {
            self.refuse_placement(order_id, &why);
        }
    }

    /// [`place_order`](Self::place_order), with a refusal at the call handed
    /// back to the caller rather than pushed into the session's order.
    pub(crate) fn try_place_order(&self, order_id: i64, contract: &Contract, order: &Order) -> Result<(), Refusal> {
        // Gated on the trading connection's own state rather than the
        // session's. The session flag is set by any transport ending, the
        // market-data farm included, and refusing an order on that takes the
        // trading connection down with the quote feed — on a connection that
        // would have carried it. This one is set only where the trading
        // connection itself has stopped being retried, which is where an order
        // accepted here would join a buffer nothing drains and be reported as
        // sent while never reaching the venue.
        self.refuse_if_trading_is_over("an order")?;
        self.core.refuse_if_readonly("an order").map_err(Refusal::validation)?;
        let session = self.order_session();
        ClientCore::read_option_list(
            &crate::client_core::ORDER_OPTIONS,
            &ClientCore::written_options(&order.order_misc_options), &session.features,
        )?;
        ClientCore::validate_order_destination(&contract.exchange)?;

        // Validate order params and contract before the engine takes it.
        let (warnings, refused) = ClientCore::retired_instructions(order, &session);
        let validation = ClientCore::validate_order(order, &session);
        crate::client_core::attached_checks::check_selectors(order, session.enables("NOAPISLPTSGL"))?;
        if let Err(why) = validation {
            if refused.is_some_and(|r| r.code == why.code)
                && u64::try_from(order_id).is_ok()
            {
                for warning in warnings {
                    self.refuse_placement(order_id, &warning);
                }
            }
            return Err(why);
        }
        ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month)?;
        // From here on, the order as this session sends it.
        let sent = ClientCore::as_sent(order, &session);
        let order: &Order = &sent;
        ClientCore::validate_supported_instructions(order)?;
        ClientCore::validate_combo_legs(&contract.sec_type, contract.combo_legs.len())?;
        for (at, leg) in contract.combo_legs.iter().enumerate() {
            ClientCore::validate_leg(at, leg)?;
        }
        ClientCore::validate_order_contract(
            contract.con_id,
            &contract.sec_type,
            &crate::types::model::contract_identity(
                &contract.last_trade_date_or_contract_month, contract.strike,
                &contract.right, &contract.multiplier, &contract.currency,
            ),
        )?;
        self.check_sec_type_permitted(&contract.sec_type)?;

        if order_id > crate::bridge::MAX_ORDER_ID as i64 {
            return Err(Refusal::validation(format!("place_order: order_id {order_id} is past the highest this client can carry an order under ({}); ask for one with next_order_id()", crate::bridge::MAX_ORDER_ID)));
        }
        let oid = order_id as u64;
        // Said once, as the paths that hand out numbers say it: a program
        // numbering its orders and its requests out of one counter has a
        // number here that a request cannot carry.
        crate::bridge::say_if_past_a_request_id(oid);

        // The number this call is spending, so the allocator does not hand it
        // out again. It counts from the highest the venue has named, and the
        // venue has not named this one yet — a program keeping its own numbers
        // off `next_valid_id`, which is the reference client's own idiom, then
        // asked for one and was given a number it had put on the market
        // moments before. A preview's own number is not one of those.
        if order_id > 0 && oid < crate::bridge::MAX_ORDER_ID && !super::a_question_of_ours(oid) {
            self.next_order_id.fetch_max(oid + 1, Ordering::AcqRel);
            self.shared.orders.save_order_ids(self.next_order_id.load(Ordering::Acquire));
        }

        // The record carries the number the order went out under. Cached from
        // the caller's object unchanged, an order placed without one read back
        // from `open_order` naming no order at all.
        let mut placed = order.clone();
        placed.order_id = oid as i64;
        self.send(ControlCommand::Place(Box::new(Placement {
            allocator: self.next_order_id.clone(),
            order_id: oid,
            contract: contract.clone(),
            order: placed,
            // What a gateway says about an order it places anyway, on the
            // order's number, once the order has gone or is kept.
            warnings,
        })))
    }

    /// Exercise or lapse a long option position. Matches `exerciseOptions` in C++.
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
    /// whatever `override_` says: one already held is used, and otherwise the
    /// engine watches for one, with no bound, as a gateway does. With
    /// `override_` false, an exercise of an option not in the money and a
    /// lapse of one in it are refused under 322, as a gateway refuses them;
    /// with `true` they go. `override_` itself travels on no tag: it names
    /// this check, which is made before the order is built. The position and
    /// quantity are checked in the account named.
    pub fn exercise_options(
        &self, req_id: i64, contract: &Contract, exercise_action: i32,
        exercise_quantity: i32, account: &str, override_: bool,
        stated: crate::client_core::ExerciseStates,
    ) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            self.refuse_if_trading_is_over("an exercise")?;
            self.core.refuse_if_readonly("an exercise").map_err(Refusal::validation)?;
            crate::client_core::ClientCore::validate_contract_expiry(&contract.last_trade_date_or_contract_month)?;
            let (action, qty, account) = ClientCore::validate_exercise(
                exercise_action, exercise_quantity, account, &self.order_session(),
            )?;
            let identity = crate::types::model::contract_identity(
                &contract.last_trade_date_or_contract_month, contract.strike,
                &contract.right, &contract.multiplier, &contract.currency,
            );
            ClientCore::validate_order_contract(contract.con_id, &contract.sec_type, &identity)?;

            let oid = if req_id > 0 {
                let oid = req_id as u64;
                // An exercise goes to the venue as an order and takes an order's
                // number on the wire, so the number a caller states here is under
                // the same rules a placement's is — whether it names an order the
                // venue is working is checked where the engine takes it.
                if oid > crate::bridge::MAX_ORDER_ID {
                    return Err(Refusal::validation(format!(
                        "exercise_options: {req_id} is past the highest number this client \
                         can carry an order under ({}); pass 0 to be given one",
                        crate::bridge::MAX_ORDER_ID,
                    )));
                }
                // Spent, as a placement spends it: the allocator counts from the
                // highest the venue has named and has not named this one, so
                // without this it hands the same number out again while the venue
                // works the exercise under it.
                self.next_order_id.fetch_max(oid + 1, Ordering::AcqRel);
                self.shared.orders.save_order_ids(self.next_order_id.load(Ordering::Acquire));
                oid
            } else {
                // Written down, as on `place_order` above.
                0
            };
            self.send(ControlCommand::Exercise(Box::new(crate::types::Exercise {
                allocator: (req_id <= 0).then(|| self.next_order_id.clone()),
                req_id,
                order_id: oid,
                stated: req_id > 0,
                contract: contract.clone(),
                action,
                qty: crate::types::qty_from_wire(qty as i64),
                account,
                states: stated,
                override_,
            })))
        })() {
            self.refuse_order(req_id, OrderOp::Exercise, &why);
        }
    }


    /// Cancel an order. Matches `cancelOrder` in C++.
    ///
    /// The second argument is what the withdrawal states about itself —
    /// [`OrderCancel`](crate::types::model::OrderCancel), or a time alone;
    /// `""` states nothing. Who is withdrawing it and whether a person
    /// entered it travel on the cancel, as a gateway writes them: from the
    /// withdrawal, not from the placement.
    ///
    /// A time does not travel. A gateway sends it only where the venue has
    /// turned that record on for the login, and this client does not read
    /// whether it has. The cancel goes anyway and the caller is told the time
    /// did not: a live order left standing because a regulatory annotation
    /// has nowhere to go is the worse of the two. Taken in silence, the order
    /// would come back without the record while the caller had given one. A time a
    /// gateway cannot read is refused as a gateway refuses it, under 10301,
    /// and nothing is withdrawn.
    pub fn cancel_order(
        &self, order_id: i64, order_cancel: impl Into<crate::types::model::OrderCancel>,
    ) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            let order_cancel = order_cancel.into();
            self.refuse_if_trading_is_over("a withdrawal")?;
            self.core.refuse_if_readonly("a cancel").map_err(Refusal::validation)?;
            // Tag 11 order ids start at 1. A negative id cast unchecked becomes a
            // large unsigned one, which the venue answers "no such order".
            let order_id = order_id as u64;
            ClientCore::check_cancel_time(&order_cancel.manual_order_cancel_time)?;
            super::wire_text("a withdrawal's operator", &order_cancel.ext_operator)?;
            // The engine withdraws a placement it has not sent by forgetting it,
            // with the changes behind it and what hangs from it: sent, the venue
            // answers that it knows no such order and the placement goes out
            // behind the next thing that transmits. It answers a withdrawal of an
            // order this session saw finish as not cancellable, and one naming an
            // order nothing is working as no such order — read once the venue has
            // named the account's working set, since an order carried over from a
            // previous session is unknown until then.
            self.send(ControlCommand::CancelOrder { order_id, stated: order_cancel })
        })() {
            self.refuse_order(order_id, OrderOp::Cancel, &why);
        }
    }


    /// Cancel an order identified by `permId` — stable across sessions.
    ///
    /// `permId` is the broker-assigned identifier returned in `order_status`
    /// callbacks and surfaced in account tools. Useful for cancelling an order
    /// placed in a prior session, where the local `order_id` is not retained.
    ///
    /// The withdrawal names an order by its number, so the engine looks the
    /// number up from `permId` among the orders the venue is working, once it
    /// has named them, and withdraws it as [`cancel_order`](Self::cancel_order)
    /// does. A `perm_id` no working order carries is refused under no number.
    pub fn cancel_order_by_perm_id(&self, perm_id: i64) {
        let asked = || -> Result<(), Refusal> {
            self.refuse_if_trading_is_over("a withdrawal")?;
            self.core.refuse_if_readonly("a cancel").map_err(Refusal::validation)?;
            if perm_id == 0 {
                return Err("cancel_order_by_perm_id: perm_id must be non-zero".into());
            }
            // Found once the venue has named the working set, as a withdrawal
            // by number is read: the order this exists for is one carried over
            // from a previous session. Withdrawn, and refused, under the
            // number the order's own reports carry in this session; refused
            // under none where no order carries it.
            self.send(ControlCommand::CancelOrderByPermId { perm_id })
        };
        if let Err(why) = asked() {
            self.refuse_session(&why);
        }
    }

    /// Cancel every order the account is working. Matches `reqGlobalCancel`
    /// in C++.
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
    pub fn req_global_cancel(
        &self, order_cancel: impl Into<crate::types::model::OrderCancel>,
    ) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            let stated = order_cancel.into();
            self.refuse_if_trading_is_over("a withdrawal of every order")?;
            self.core.refuse_if_readonly("a global cancel").map_err(Refusal::validation)?;
            super::wire_text("a withdrawal's operator", &stated.ext_operator)?;
            // Everything the engine holds goes with everything working, and what
            // is working is withdrawn once the venue has named it; where the
            // naming does not finish within its bound, what had been named goes
            // and the caller is told what is not covered.
            self.send(ControlCommand::GlobalCancel { stated })
        })() {
            self.refuse_session(&why);
        }
    }


    /// Request next valid order ID. Matches `reqIds` in C++.
    ///
    /// Answered on `next_valid_id` in its place in the session's order. The
    /// venue names what the account is working after the connect returns, and
    /// the id is floored above it, so the engine holds the question until the
    /// naming is over; nothing waits here.
    pub fn req_ids(&self) {
        if let Err(why) = self.send(ControlCommand::Ask(crate::types::Ask::NextValidId)) {
            self.refuse_session(&why);
        }
    }

    /// The id `next_valid_id` states, as this client stands: stated without
    /// being taken, as the reference client states it — the caller places
    /// under it, and the reservation happens then. Held under the end of the
    /// range the venue's reports can name back, as the reservation is: an id
    /// past it names an order this client could never reconcile against the
    /// answers to it. Waits for nothing.
    pub(crate) fn stated_next_id(&self) -> u64 {
        let stated = self.next_order_id.load(Ordering::Acquire)
            .max(self.shared.orders.working_id_watermark().saturating_add(1))
            .min(crate::bridge::MAX_ORDER_ID);
        crate::bridge::say_if_past_a_request_id(stated);
        stated
    }

    /// Wait for the venue's replay before reserving above it and the saved
    /// counter. Later reports still raise the floor on each reservation.
    fn next_id_base(&self) -> u64 {
        if !self.shared.orders.wait_for_replay() && self.shared.orders.naming_began() {
            // No error travels with an id, so this is said where it can be
            // said. The floor is whatever had been named by now, which is not
            // the whole of what the account is working.
            log::warn!(
                "the venue had not finished naming this account's working orders within \
                 the wait, so the next order id is counted from what it had named and the \
                 venue may refuse an order under it as one it is already working",
            );
        }
        self.shared.orders.working_id_watermark().saturating_add(1)
    }

    /// Reserve the next order ID above the saved counter and venue replay.
    ///
    /// The counter is saved under the account and API client before this
    /// returns. Sessions sharing the configured file reserve under an
    /// exclusive lock. A storage failure warns once and leaves allocation
    /// using session memory and venue replay.
    ///
    /// Zero where there is no id to give, which every placement path refuses.
    /// A number carries no reason with it, so the reason goes out on the
    /// channel a caller already watches as well as to the log: told only in
    /// the log, a caller reads a zero and has nowhere to learn why.
    pub fn next_order_id(&self) -> i64 {
        self.reserve_order_ids(1).unwrap_or_else(|why| {
            log::error!("{}", why.message);
            self.refuse_session(&why);
            0
        })
    }

    /// The first id past everything the account has used that a request can
    /// also carry.
    ///
    /// A caller that numbers its orders and its requests out of one counter
    /// needs both at once: clear of every id an order has spent, and inside the
    /// numbers a request can carry. An account that has been given a wider
    /// order id than that has no such number above it, so this answers with
    /// one past the widest the account has used that a request can carry, and
    /// the counting goes on from there. A read, not a reservation: asked twice,
    /// it answers the same until the venue names a wider id. After a connect it
    /// waits, for at most three seconds in all, for the venue to name the
    /// orders the account is working.
    ///
    /// Refused where even that is not a number a request can carry.
    pub fn next_shared_id(&self) -> Result<i64, Refusal> {
        super::next_shared_id_of(&self.shared)
    }

    /// [`next_shared_id`](Self::next_shared_id), with its wait for the replay
    /// also bounded by `timeout` and by the config's
    /// [`cancel`](super::EClientConfig::cancel), both read at each 10 ms step of the
    /// wait.
    ///
    /// A gateway gives its client the next valid id once it has read the
    /// account's orders. A program bounding that wait, as it bounds the
    /// handshake, is answered [`Refusal::no_answer`] when `timeout` passes
    /// first, and the same, saying so, when the connect is taken back. `None`
    /// is the replay's own bound alone, as `next_shared_id` waits.
    pub fn next_shared_id_within(
        &self, timeout: Option<std::time::Duration>,
    ) -> Result<i64, Refusal> {
        let until = timeout.and_then(|bound| std::time::Instant::now().checked_add(bound));
        match self.shared.orders.wait_for_replay_until(until, self.cancel.as_deref()) {
            crate::bridge::ReplayWait::TimedOut => Err(Refusal::no_answer(format!(
                "the venue had not named what this account is working within {:?}",
                timeout.unwrap_or_default(),
            ))),
            crate::bridge::ReplayWait::TakenBack => Err(Refusal::no_answer(
                "the wait for the venue to name what this account is working was taken back",
            )),
            crate::bridge::ReplayWait::Settled(_) => super::shared_id_past(&self.shared),
        }
    }

    /// The id `next_valid_id` would state now, read without waiting.
    ///
    /// A gateway gives its client the next valid id only once it has read the
    /// account's orders, and raises it past every new order. This is that
    /// floor as it stands at the read: it rises as the venue names what the
    /// account is working, before the read that delivers those orders, and
    /// again after every reconnect. A caller allocating ids clears it at each
    /// allocation. The saved counter also raises it at connect; it is one
    /// where neither the saved counter nor the venue names a prior id. A read,
    /// not a reservation. [`next_shared_id`](Self::next_shared_id) answers the
    /// floor a request can also carry.
    pub fn order_id_floor(&self) -> i64 {
        self.stated_next_id() as i64
    }

    /// Take `n` consecutive ids in one step.
    ///
    /// A bracket occupies three consecutive ids: parent, parent+1, parent+2.
    /// Reserving them in one step keeps a concurrent placement from taking a
    /// child's id or moving the counter back over ids already handed out.
    ///
    /// The whole run has to fit under [`crate::bridge::MAX_ORDER_ID`],
    /// children included. Handed out unchecked, the run crossed the end of the
    /// range the venue's reports can name back: the id itself could never be
    /// reconciled, a bracket's children were an addition past the end of the
    /// signed range, and the counter left behind gave every caller after it a
    /// negative id — which the paths that carry one unsigned turned into an
    /// order number above nine quintillion.
    fn reserve_order_ids(&self, n: u64) -> Result<i64, Refusal> {
        self.next_id_base();
        self.shared.orders.reserve_order_ids(&self.next_order_id, n).map(|id| id as i64)
    }

    // ── Open Orders ──

    /// Request open orders for this client. Matches `reqOpenOrders` in C++.
    ///
    /// Answers with every order working on the account, as
    /// [`req_all_open_orders`](EClient::req_all_open_orders) does. The protocol
    /// carries no client number on an order, so this session cannot tell which
    /// orders it placed; reporting fewer would omit working orders.
    pub fn req_open_orders(&self) {
        self.ask_open_orders(crate::types::model::Question::OpenOrders);
    }

    /// Request all open orders. Matches `reqAllOpenOrders` in C++.
    pub fn req_all_open_orders(&self) {
        self.ask_open_orders(crate::types::model::Question::AllOpenOrders);
    }

    /// Every working order, then `open_order_end`, answered where the answer
    /// stands in the session's order.
    fn ask_open_orders(&self, question: crate::types::model::Question) {
        if self.session_over() {
            return self.refuse_question(question, &Refusal::not_connected("Not connected"));
        }
        // The orders already working are named by the server unprompted after a
        // connect, and answering before that lands reports none of them. A
        // strategy asking what it already has on, at the moment it starts, is
        // exactly who asks this first, and telling it "nothing" is how the same
        // order gets placed twice. So the engine holds the question until the
        // naming is over, or its bound has passed — said ahead of the answer
        // where the venue had begun naming and not finished.
        if let Err(why) = self.send(ControlCommand::Ask(crate::types::Ask::OpenOrders(question))) {
            self.refuse_question(question, &why);
        }
    }

    // ── Completed Orders ──

    /// Request completed orders. Matches `reqCompletedOrders` in C++.
    ///
    /// Asks the venue, and answers where the end of what it states stands in
    /// the session's order: every completed order this session has archived,
    /// then `completed_orders_end`. Nothing waits here.
    ///
    /// `api_only` asks for the orders entered through an API rather than by
    /// hand. The venue states no origin beside a finished order, and it does
    /// number the ones an API placed: an order that went out through one
    /// carries the number that API gave it, and one typed in carries none. So
    /// `true` is answered with the orders the venue numbered.
    pub fn req_completed_orders(&self, api_only: bool) {
        use crate::types::model::Question;
        if self.session_over() {
            return self.refuse_question(Question::CompletedOrders, &Refusal::not_connected("Not connected"));
        }
        // Asked of the venue, not only of this session: what finished while
        // this program was watching is a fraction of what the account has
        // done. The engine asks one question at a time, in the order they
        // were asked, and answers each where its end stands in the session's
        // order: a second asked while the first is out waits its turn.
        if let Err(why) = self.send(ControlCommand::FetchCompletedOrders { api_only }) {
            self.refuse_question(Question::CompletedOrders, &why);
        }
    }

    /// File what the venue has stated finished into the archive the answers
    /// are read from.
    ///
    /// The queue they arrive on empties on read and the venue does not resend
    /// completed orders, so later answers read this archive.
    pub(crate) fn archive_completed_orders(&self) {
        let mut archive = self.completed.lock().unwrap();
        // What the venue has taken back goes first. A trade cancel or
        // correction returns a finished order to a working quantity, and the
        // bridge can only drop the completion it still holds — this is the
        // copy it cannot reach. Applied before the arrivals below, an order
        // taken back and then finished again keeps the new record and loses
        // the superseded one.
        for order_id in self.shared.orders.drain_order_corrections() {
            archive.retain(|(_, order, _)| order.order_id != order_id as i64);
            // And the eviction armed for it when it finished. A bust or a
            // correction puts the order back to a working quantity, and an
            // eviction still standing took its record away on the next read —
            // after which the order reads as one the venue is not working.
            self.deferred_evictions.lock().unwrap().remove(&order_id);
        }
        for order in self.shared.orders.drain_completed_orders() {
            let status_str = crate::types::order_status::order_status_str(order.status);
            let entry = if let Some(info) = self.shared.orders.get_order_info(order.order_id) {
                let mut state = info.order_state;
                state.status = status_str.into();
                // The contract as the venue was told it where the order was
                // placed here — the record carries the legs and the hedge the
                // caller stated, which no definition of one contract carries —
                // and the venue's own, enriched from the definition cache,
                // where it was not.
                let placed_on = self.core.open_orders.lock().unwrap()
                    .get(&order.order_id).map(|placed| placed.contract.clone());
                let contract = match placed_on {
                    Some(contract) => contract,
                    None if info.contract.con_id != 0 => self.core
                        .get_contract(info.contract.con_id, &self.shared)
                        .unwrap_or(info.contract),
                    None => info.contract,
                };
                // The client that placed it, where the venue names none: the
                // venue states no client on this wire, and the record of a
                // placement made here knows whose it was.
                let mut order = info.order;
                if order.client_id == 0 {
                    order.client_id = self.core.placing_client(&self.shared, order.order_id as u64);
                }
                (contract, order, state)
            } else {
                (
                    Contract::default(),
                    Order { order_id: order.order_id as i64, ..Default::default() },
                    crate::types::model::OrderState {
                        status: status_str.into(),
                        ..Default::default()
                    },
                )
            };
            // Replaced where this order is already in the archive, not added
            // beside it. The venue restates an order it has already stated
            // once the memory of it has aged out, and pushed again the caller
            // was handed the same order twice. The caller's own number for it
            // decides, as it does in the queue this was read off.
            match archive.iter().position(|(_, held, _)| held.order_id == entry.1.order_id
                && (held.perm_id == entry.1.perm_id
                    || held.perm_id == 0
                    || entry.1.perm_id == 0))
            {
                Some(at) => archive[at] = entry,
                None => archive.push(entry),
            }
            // Bound `order_cache` growth: terminal entries are no longer needed
            // once what they carried has been read out of them. Handed to the
            // side that reads the fills rather than freed here: a fill taken
            // off the queue but not yet reported still needs it.
            self.deferred_evictions.lock().unwrap().insert(order.order_id);
        }
    }

    // ── Executions ──

    /// Automatically bind future orders to this client. Matches `reqAutoOpenOrders` in
    /// C++.
    ///
    /// What binding asks for is the default here: this session is told about
    /// every order on the account, whoever entered it. Nothing goes to the
    /// venue. On a gateway, client 0's flag turns binding on or off; here every
    /// session is already told about every order, so `b_auto_bind` changes
    /// nothing. This surface names no client, so there is no other client to
    /// refuse.
    ///
    /// [`Wrapper::order_bound`](crate::api::wrapper::Wrapper::order_bound) does not follow from this call. It is fired once
    /// for each order the venue restates when the session opens that this
    /// session did not place, pairing the venue's permanent id with the order
    /// id it is reached under here.
    pub fn req_auto_open_orders(&self, _b_auto_bind: bool) {}

    /// Request execution reports. Matches `reqExecutions` in C++.
    /// Replays stored executions (optionally filtered), firing `exec_details` +
    /// `commission_and_fees_report` for each, then `exec_details_end`, where
    /// the answer stands in the session's order: every fill delivered before
    /// it is in it.
    ///
    /// `last_n_days` and `specific_dates` select days as a gateway selects
    /// them, counted on the session's time zone; a date that is not a day of
    /// the calendar is refused under 320, as a gateway refuses it. The
    /// executions answered from reach back to midnight six days before the
    /// logon in UTC, or to the logon's own day for a session set to today's
    /// executions, so the earliest days asked for can be missing some; those
    /// days are named on `error` under 321 ahead of the answer, which still
    /// comes. `acct_code` is ignored on a login holding one account and
    /// refused on one holding several where the login does not hold it, as a
    /// gateway does both. A refused request is told so on `error` and nothing
    /// else, as a gateway tells it.
    pub fn req_executions(&self, req_id: i64, filter: &ExecutionFilter) {
        if let Some(why) = self.shared.reference.session_over() {
            return self.refuse_request(req_id, &Refusal::not_connected(why));
        }
        self.answer(crate::bridge::Answer::Executions { req_id, filter: filter.clone() });
    }
}

impl EClient {
    /// Send a bracket as the one instruction the engine has for it.
    ///
    /// Three orders under three numbers, linked by the venue: the children are
    /// held until the parent has a position, and whichever fills withdraws the
    /// other. `place_bracket` is the call that states this in a caller's terms;
    /// this is the part that reaches the engine.
    pub(crate) fn submit_bracket(
        &self, contract: &Contract, side: crate::types::Side, quantity: f64,
        entry: f64, take_profit: f64, stop_loss: f64,
    ) -> Result<[i64; 3], Refusal> {
        // Checked here rather than in `place_bracket`: every surface that places a
        // bracket routes through this.
        self.refuse_if_trading_is_over("a bracket")?;
        self.core.refuse_if_readonly("a bracket").map_err(Refusal::validation)?;
        // The checks `place_order` applies. An order on a security type the account
        // is not permitted is returned Inactive with tag 58 empty, so the reason is
        // stated here instead.
        ClientCore::validate_order_destination(&contract.exchange)?;
        ClientCore::validate_order_contract(
            contract.con_id,
            &contract.sec_type,
            &crate::types::model::contract_identity(
                &contract.last_trade_date_or_contract_month, contract.strike,
                &contract.right, &contract.multiplier, &contract.currency,
            ),
        )?;
        self.check_sec_type_permitted(&contract.sec_type)?;
        // Consecutive, because the venue reads the children's numbers as the
        // parent's plus one and two. Taken apart, a bracket links to whatever
        // happened to be placed in between.
        let parent_id = self.reserve_order_ids(3)?;
        let (tp_id, sl_id) = (parent_id + 1, parent_id + 2);
        // The engine registers the contract, records each leg as placed here
        // under its own number before it sends the three — a refusal or a fill
        // then always finds the record it answers — and sends them as the one
        // instruction it has for a bracket.
        self.send(ControlCommand::Bracket(Box::new(crate::types::Bracket {
            contract: contract.clone(),
            parent_id: parent_id as u64,
            side,
            quantity,
            entry,
            take_profit,
            stop_loss,
        })))?;
        Ok([parent_id, tp_id, sl_id])
    }
}
