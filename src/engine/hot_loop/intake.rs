//! Orders as their callers place them.
//!
//! A call checks what needs no venue and hands the order over. What a gateway
//! does between taking an order and sending it happens here, in the loop's own
//! laps: the contract a caller described is named, the contract registered,
//! the order checked against what this session placed and what the venue is
//! working, built, and sent — or kept until an order that transmits releases
//! it. An order command waiting on any of that holds up only what depends on
//! it: a later command for the same order, a child of a parent still waiting.
//! Its cancel withdraws it, and it ends once: sent, refused, or withdrawn.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::bridge::{OrderBook, Record, TakenOrder};
use crate::client_core::ClientCore;
use crate::error_codes::{
    DUPLICATE_ORDER_ID, NO_SUCH_ORDER, NOT_CANCELLABLE, ORDER_DOES_NOT_MATCH, Refusal,
};
use crate::types::model::{self as api, ErrorOrigin, OrderOp};
use crate::types::{Bracket, ControlCommand, Exercise, InstrumentId, OrderRequest, Placement};

use super::HotLoop;
use super::ccp::OrderNamed;

/// Take the order commands handled by intake, returning the others unchanged.
#[allow(clippy::result_large_err)]
pub(crate) fn order_command(cmd: ControlCommand) -> Result<ControlCommand, ControlCommand> {
    match cmd {
        ControlCommand::Place(_)
        | ControlCommand::CancelOrder { .. }
        | ControlCommand::CancelOrderByPermId { .. }
        | ControlCommand::GlobalCancel { .. }
        | ControlCommand::Exercise(_)
        | ControlCommand::Bracket(_) => Ok(cmd),
        other => Err(other),
    }
}

impl ControlCommand {
    /// The numbers this command acts on.
    fn own(&self) -> impl Iterator<Item = u64> {
        let ids = match self {
            Self::Place(p) => [Some(p.order_id), None, None],
            Self::CancelOrder { order_id, .. } => [Some(*order_id), None, None],
            Self::Exercise(e) => [e.allocator.is_none().then_some(e.order_id), None, None],
            Self::Bracket(b) => [Some(b.parent_id), Some(b.parent_id + 1), Some(b.parent_id + 2)],
            _ => [None; 3],
        };
        ids.into_iter().flatten()
    }

    /// The command's numbers and the parent it hangs from.
    fn depends_on(&self) -> impl Iterator<Item = u64> {
        let parent = match self {
            Self::Place(p) => u64::try_from(p.order.parent_id).ok().filter(|id| *id > 0),
            _ => None,
        };
        self.own().chain(parent)
    }
}

/// An order built and not sent, waiting for one that transmits.
///
/// The field saying whether an order goes now is written into the reference
/// client's own message and never reaches the venue, so nothing on the wire
/// can hold an order back and the engine holds it instead. Kept in the order
/// they were placed, which is the order they go out in.
#[derive(Debug)]
struct Kept {
    order_id: u64,
    /// The order it hangs from, or zero.
    parent_id: i64,
    command: OrderRequest,
}

impl Kept {
    /// Whether what is kept would place the order, rather than revise one the
    /// venue is already working. Forgetting a placement is the whole of a
    /// withdrawal; forgetting a revision leaves the order it revises live.
    fn places_the_order(&self) -> bool {
        matches!(self.command, OrderRequest::SubmitEx { .. } | OrderRequest::SubmitBracket { .. })
    }
}

/// What this session placed or restated, as the checks read it.
#[derive(Debug)]
struct Placed {
    order: api::Order,
    instrument: InstrumentId,
}

/// An order command not yet taken, and the lookup naming its contract, once
/// one is out.
#[derive(Debug)]
struct Pending {
    cmd: ControlCommand,
    lookup: Option<u32>,
    /// The watch an exercise opened on its option's in-the-money figure, by
    /// the engine's own number for it, while it waits for one.
    watch: Option<i64>,
}

impl Pending {
    fn new(cmd: ControlCommand) -> Self {
        Self { cmd, lookup: None, watch: None }
    }
}

/// The series a gateway reads an option's standing in the money off: its
/// intrinsic value, whether it is in the money, and its time value.
const IN_THE_MONEY: u32 = 493;

/// The first number the engine opens its own watches under, apart from every
/// number a caller may state.
const OWN_WATCHES: i64 = crate::bridge::ENGINE_ID_BASE as i64;

/// Whether a market-data request is one the engine opened for itself, which
/// nothing is said of to a caller.
pub(crate) fn engine_owned(req_id: i64) -> bool {
    req_id >= OWN_WATCHES
}

/// A gateway's refusal of an exercise or a lapse against the natural action,
/// from its option's in-the-money figure: the intrinsic value and the
/// attribute that says whether it is in the money. None where it goes.
fn against_the_natural_action(
    action: u8,
    override_: bool,
    value: f64,
    attribute: f64,
) -> Option<Refusal> {
    if override_ {
        return None;
    }
    // A value is not the unset maximum and is a number.
    let valid = value.is_finite() && value != f64::MAX;
    match action {
        1 if !(attribute != 0.0 && valid && value >= 0.0) => Some(Refusal::stated(
            crate::error_codes::REQUEST_NOT_PROCESSED,
            "Error processing request:Exercise ignored because option is not in-the-money.",
        )),
        2 if attribute == 0.0 || (valid && value > 0.0) => Some(Refusal::stated(
            crate::error_codes::REQUEST_NOT_PROCESSED,
            "Error processing request:Lapse ignored because option is in-the-money.",
        )),
        _ => None,
    }
}

/// Whether a command was taken — sent, kept, refused or withdrawn — or waits.
enum Step {
    Done,
    Waits,
}

/// The engine's side of the orders its callers place.
#[derive(Default)]
pub(crate) struct Intake {
    /// Commands not yet taken, in the order they were admitted.
    waiting: VecDeque<Pending>,
    /// Waiting commands per order number.
    waiting_ids: HashMap<u64, usize>,
    /// Naming lookups shared by orders for the same contract.
    naming: HashMap<String, u32>,
    /// Failed lookups still read by commands that shared them.
    unnamed: HashMap<u32, Refusal>,
    /// Orders built and kept until one that transmits releases them.
    kept: Vec<Kept>,
    /// What this session placed or restated, by number.
    placed: HashMap<u64, Placed>,
    /// What the venue named each description as, for the next order on it:
    /// asked again for every order, a program placing a hundred on one
    /// contract sends a hundred lookups for a name that has not changed.
    named: HashMap<String, api::Contract>,
}

impl Intake {
    fn recount_waiting(&mut self) {
        self.waiting_ids.clear();
        for pending in &self.waiting {
            for id in pending.cmd.own() {
                *self.waiting_ids.entry(id).or_default() += 1;
            }
        }
    }

    /// How many commands wait on the venue.
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// Commands kept unsent until a later order transmits them.
    pub(crate) fn kept_count(&self) -> usize {
        self.kept.len()
    }

    /// Whether what is kept under this number would place the order.
    fn keeps_a_placement(&self, order_id: u64) -> bool {
        self.kept.iter().any(|k| k.order_id == order_id && k.places_the_order())
    }

    /// Keep an order back until one that transmits releases it.
    fn keep(&mut self, order_id: u64, parent_id: i64, command: OrderRequest) {
        self.kept.retain(|k| k.order_id != order_id);
        self.kept.push(Kept { order_id, parent_id, command });
    }

    /// Take out what is kept under a number.
    fn release(&mut self, order_id: u64) -> Option<Kept> {
        let at = self.kept.iter().position(|k| k.order_id == order_id)?;
        Some(self.kept.remove(at))
    }

    /// The family an order that transmits releases, in the order it goes:
    /// its parent where that is kept, then what shares the parent or hangs
    /// from it.
    fn family_of(&mut self, order_id: u64, parent_id: i64) -> Vec<Kept> {
        let mut going = Vec::new();
        if parent_id > 0
            && let Some(parent) = self.release(parent_id as u64)
        {
            going.push(parent);
        }
        let (family, rest): (Vec<Kept>, Vec<Kept>) =
            std::mem::take(&mut self.kept).into_iter().partition(|k| {
                k.order_id != order_id
                    && ((parent_id != 0 && k.parent_id == parent_id)
                        || k.parent_id == order_id as i64)
            });
        self.kept = rest;
        going.extend(family);
        going
    }
}

#[cfg(feature = "test-helpers")]
impl Intake {
    /// Note an order as one this session placed, as a test states it.
    pub(crate) fn note_placed(
        &mut self,
        order_id: u64,
        order: api::Order,
        instrument: InstrumentId,
    ) {
        self.placed.insert(order_id, Placed { order, instrument });
    }
}

#[cfg(test)]
impl Intake {
    /// Whether anything is kept under this number for a later transmit.
    pub(crate) fn keeps(&self, order_id: u64) -> bool {
        self.kept.iter().any(|k| k.order_id == order_id)
    }

    /// Remember what the venue named a description as, as a test states it.
    pub(crate) fn remember_named(&mut self, key: String, contract: api::Contract) {
        self.named.insert(key, contract);
    }
}

impl HotLoop {
    /// Take an order command a caller admitted.
    pub(crate) fn take_order_command(&mut self, cmd: ControlCommand) {
        let cmd = match cmd {
            ControlCommand::CancelOrder { order_id, stated } => {
                let withdrawn = self.withdraw_waiting_placement(order_id)
                    || self.withdraw_kept_placement(order_id);
                self.intake.recount_waiting();
                if withdrawn {
                    self.say_the_time_did_not_travel(order_id, &stated);
                    return;
                }
                ControlCommand::CancelOrder { order_id, stated }
            }
            ControlCommand::GlobalCancel { stated } => {
                self.withdraw_everything_held();
                self.intake.recount_waiting();
                ControlCommand::GlobalCancel { stated }
            }
            other => other,
        };
        let behind = cmd.depends_on().any(|id| self.intake.waiting_ids.contains_key(&id));
        let mut pending = Pending::new(cmd);
        if behind || matches!(self.take_one(&mut pending), Step::Waits) {
            for id in pending.cmd.own() {
                *self.intake.waiting_ids.entry(id).or_default() += 1;
            }
            self.intake.waiting.push_back(pending);
        }
    }

    /// Released work keeps its admission order and shares the lap's allowance.
    pub(crate) fn work_through_orders(&mut self, left: &mut usize) {
        // Remove terms only when the venue has finished with an order or said
        // it knows none. A revision then has nothing to revise; a placement
        // kept unsent stays, because the venue has never been given it.
        let orders = &self.shared.orders;
        if orders.take_numbers_changed() {
            self.intake.placed.retain(|id, _| {
                !orders.number_finished(*id) && !orders.number_unknown_to_the_venue(*id)
            });
            self.intake.kept.retain(|k| {
                k.places_the_order()
                    || !(orders.number_finished(k.order_id)
                        || orders.number_unknown_to_the_venue(k.order_id))
            });
        }
        self.intake.waiting_ids.clear();
        for _ in 0..self.intake.waiting.len() {
            let Some(mut pending) = self.intake.waiting.pop_front() else { break };
            let behind =
                pending.cmd.depends_on().any(|id| self.intake.waiting_ids.contains_key(&id));
            if *left == 0 || behind || matches!(self.take_one(&mut pending), Step::Waits) {
                for id in pending.cmd.own() {
                    *self.intake.waiting_ids.entry(id).or_default() += 1;
                }
                self.intake.waiting.push_back(pending);
            } else {
                *left -= 1;
            }
        }
        if !self.intake.unnamed.is_empty() {
            let waiting: HashSet<_> = self.intake.waiting.iter().filter_map(|p| p.lookup).collect();
            self.intake.unnamed.retain(|id, _| waiting.contains(id));
        }
    }

    fn take_one(&mut self, pending: &mut Pending) -> Step {
        match &mut pending.cmd {
            ControlCommand::Place(p) => self.take_placement(p, &mut pending.lookup),
            ControlCommand::CancelOrder { order_id, stated } => {
                let (order_id, stated) = (*order_id, stated.clone());
                self.take_cancel(order_id, &stated)
            }
            ControlCommand::CancelOrderByPermId { perm_id } => {
                let perm_id = *perm_id;
                self.take_cancel_by_perm_id(perm_id)
            }
            ControlCommand::GlobalCancel { stated } => {
                let stated = stated.clone();
                self.take_global_cancel(&stated)
            }
            ControlCommand::Exercise(e) => {
                self.take_exercise(e, &mut pending.lookup, &mut pending.watch)
            }
            ControlCommand::Bracket(b) => self.take_bracket(b),
            _ => unreachable!("intake takes order commands"),
        }
    }

    /// Whether a number names an order the venue is working. What is kept
    /// under it and would place it names an order nothing has submitted.
    fn working(&self, order_id: u64) -> bool {
        let orders = &self.shared.orders;
        let placed_here = self.intake.placed.contains_key(&order_id)
            && !orders.number_finished(order_id)
            && !orders.number_unknown_to_the_venue(order_id);
        (placed_here || orders.venue_is_working(order_id))
            && !self.intake.keeps_a_placement(order_id)
    }

    /// A refusal of an operation on an order, under the order's number.
    fn refuse_order(&self, order_id: i64, op: OrderOp, why: Refusal) {
        log::warn!("order {order_id} refused: {}", why.message);
        self.shared.push_refused(
            ErrorOrigin::Order { id: order_id, op },
            i64::from(why.code),
            why.message,
        );
    }

    /// A number the wire cannot carry names a different contract.
    fn beyond_the_wire(con_id: i64) -> Option<Refusal> {
        u32::try_from(con_id).is_err().then(|| {
            Refusal::validation(format!(
                "contract {con_id} is numbered beyond what a request carries it in, so this one \
             would be answered for a different contract",
            ))
        })
    }

    /// Name a contract before an order or exercise registers it. The lookup
    /// is kept with the instruction so a later lap can finish it.
    fn name_order_contract(
        &mut self,
        contract: &mut api::Contract,
        lookup: &mut Option<u32>,
    ) -> Result<bool, Refusal> {
        let key = if contract.con_id == 0 {
            ClientCore::description_key(contract)
        } else {
            format!("conId:{}", contract.con_id)
        };
        let mut named = match self.intake.named.get(&key) {
            Some(known) => known.clone(),
            None => match *lookup {
                None => {
                    if let Some(asked) = self.intake.naming.get(&key) {
                        *lookup = Some(*asked);
                        return Ok(false);
                    }
                    let asked = self
                        .ccp
                        .name_for_an_order(contract, &mut self.ccp_conn, &mut self.hb, &self.shared)
                        .ok_or_else(|| {
                            Refusal::not_connected(
                                "the order's contract could not be named: there is no connection \
                         to the venue to ask on",
                            )
                        })?;
                    self.intake.naming.insert(key, asked);
                    *lookup = Some(asked);
                    return Ok(false);
                }
                Some(asked) => {
                    if let Some(why) = self.intake.unnamed.get(&asked) {
                        return Err(why.clone());
                    }
                    let Some(at) = self.ccp.orders_named.iter().position(|(rid, _)| *rid == asked)
                    else {
                        return Ok(false);
                    };
                    self.intake.naming.remove(&key);
                    let result = match self.ccp.orders_named.remove(at).1 {
                        OrderNamed::Contract(def) => {
                            let named = api::ContractDetails::from_definition(&def).contract;
                            self.intake.named.insert(key, named.clone());
                            Ok(named)
                        }
                        OrderNamed::Unnamed(listings) => {
                            Err(Refusal::no_definition(super::ccp::unnamed(
                                &contract.sec_type,
                                &contract.symbol,
                                &contract.exchange,
                                listings,
                                "the order could not be placed",
                            )))
                        }
                        OrderNamed::Refused(code, why) => Err(Refusal::stated(code, why)),
                    };
                    if let Err(why) = &result {
                        self.intake.unnamed.insert(asked, why.clone());
                    }
                    result?
                }
            },
        };
        // Naming supplies the contract's terms without removing the hedge or
        // combo legs the caller stated beside them.
        named.delta_neutral_contract = contract.delta_neutral_contract.clone();
        if !contract.combo_legs.is_empty() {
            named.combo_legs = contract.combo_legs.clone();
        }
        *contract = named;
        Ok(true)
    }

    fn take_placement(&mut self, p: &mut Placement, lookup: &mut Option<u32>) -> Step {
        let order_id = p.order_id;
        let op = if self.working(order_id) { OrderOp::Modify } else { OrderOp::Place };
        if p.contract.con_id == 0 && !p.contract.symbol.is_empty() {
            match self.name_order_contract(&mut p.contract, lookup) {
                Ok(true) => {}
                Ok(false) => return Step::Waits,
                Err(why) => {
                    self.refuse_order(order_id as i64, op, why);
                    return Step::Done;
                }
            }
        }
        if let Some(why) = Self::beyond_the_wire(p.contract.con_id) {
            self.refuse_order(order_id as i64, op, why);
            return Step::Done;
        }

        let replacing = op == OrderOp::Modify;
        // A number the venue has already worked an order under names nothing
        // now, so this placement is not a revision — and the venue refuses a
        // repeated number only while it is still working one, so after a fill
        // it takes it as a new order. A caller retrying what it believed had
        // failed was given a second live order.
        if !replacing && self.shared.orders.number_finished(order_id) {
            self.refuse_order(
                order_id as i64,
                OrderOp::Place,
                Refusal::stated(
                    DUPLICATE_ORDER_ID,
                    format!(
                        "order {order_id} has already been worked and finished: place a new \
                     order under a number of its own",
                    ),
                ),
            );
            return Step::Done;
        }
        // A replace names the order and not the contract, so the order stays
        // on the slot it was placed on and one naming another contract is
        // refused rather than recorded against it — settled before anything
        // is registered, since registering spends a slot on a contract this
        // then refuses.
        let placed_on =
            if replacing { self.intake.placed.get(&order_id).map(|p| p.instrument) } else { None };
        let wrong_contract = |symbol: &str| {
            Refusal::stated(
                ORDER_DOES_NOT_MATCH,
                format!(
                    "order {order_id} is working on another contract, and a replace names the order \
             rather than the contract: withdraw it and place a new order to trade {symbol}",
                ),
            )
        };
        let identity = api::contract_identity(
            &p.contract.last_trade_date_or_contract_month,
            p.contract.strike,
            &p.contract.right,
            &p.contract.multiplier,
            &p.contract.currency,
        );
        let instrument = match placed_on {
            Some(placed_on) if p.contract.con_id != 0 => {
                if self.context.market.instrument_by_con_id(p.contract.con_id) != Some(placed_on) {
                    self.refuse_order(
                        order_id as i64,
                        OrderOp::Modify,
                        wrong_contract(&p.contract.symbol),
                    );
                    return Step::Done;
                }
                placed_on
            }
            _ => {
                // An order the venue replayed holds no slot here. The venue's
                // own book names the contract it is on.
                if replacing
                    && let Some(known) = self.shared.orders.get_order_info(order_id)
                    && !ClientCore::names_the_same_contract(&known.contract, &p.contract)
                {
                    self.refuse_order(
                        order_id as i64,
                        OrderOp::Modify,
                        wrong_contract(&p.contract.symbol),
                    );
                    return Step::Done;
                }
                self.register_contract(
                    p.contract.con_id,
                    p.contract.symbol.clone(),
                    &p.contract.sec_type,
                    &p.contract.exchange,
                    &identity,
                    "",
                )
            }
        };

        let command = if replacing {
            if placed_on.is_some_and(|placed_on| placed_on != instrument) {
                self.refuse_order(
                    order_id as i64,
                    OrderOp::Modify,
                    wrong_contract(&p.contract.symbol),
                );
                return Step::Done;
            }
            // A replace is the caller's statement of the order, restated
            // whole; what a gateway refuses in one is refused here.
            let resting = self
                .intake
                .placed
                .get(&order_id)
                .map(|placed| placed.order.clone())
                .or_else(|| self.shared.orders.get_order_info(order_id).map(|info| info.order));
            if let Some(refusal) =
                ClientCore::modify_refusal_of(resting, &p.order, Some(&self.shared))
            {
                self.refuse_order(order_id as i64, OrderOp::Modify, refusal);
                return Step::Done;
            }
            // The statement rides on the replace, built with it, so a
            // statement that cannot be built refuses the replace and two
            // replaces of one order cannot exchange their terms.
            let spec = match ClientCore::build_order_request(
                &p.order,
                order_id,
                instrument,
                Some(&p.contract),
            ) {
                Ok(ControlCommand::Order(OrderRequest::SubmitEx { kind, attrs, .. })) => {
                    Some(Box::new(crate::types::OrderSpec { kind, attrs }))
                }
                Ok(_) => None,
                Err(why) => {
                    self.refuse_order(order_id as i64, OrderOp::Modify, why);
                    return Step::Done;
                }
            };
            OrderRequest::Modify {
                order_id,
                price: ClientCore::replace_price(&p.order),
                qty: crate::types::qty_from_f64(p.order.total_quantity),
                outside_rth: p.order.outside_rth,
                ord_type: p.order.ord_type_byte(),
                tif: p.order.tif_byte(),
                stop_price: ClientCore::replace_trigger(&p.order),
                spec,
            }
        } else {
            match ClientCore::build_order_request(&p.order, order_id, instrument, Some(&p.contract))
            {
                Ok(ControlCommand::Order(built)) => built,
                Ok(other) => {
                    log::error!("order {order_id} was built as {other:?}, which is not an order");
                    return Step::Done;
                }
                Err(why) => {
                    self.refuse_order(order_id as i64, OrderOp::Place, why);
                    return Step::Done;
                }
            }
        };

        // The caller's side records the order before anything the venue says
        // about it: the record stands ahead of the order in the session's
        // order, and the venue's answer to it after.
        if !replacing {
            self.shared.orders.number_placed_again(order_id);
        }
        self.shared.push_call_record(Record::OrderBook(OrderBook::Taken(Box::new(TakenOrder {
            order_id,
            contract: p.contract.clone(),
            order: p.order.clone(),
            instrument,
            restated: replacing,
        }))));
        // A preview is not recorded for the checks; its number is the
        // answering call's own, and a gateway's preview is not an order it holds.
        if !p.order.what_if {
            let mut order = p.order.clone();
            if let Some(before) = self.intake.placed.get(&order_id) {
                // What a replace leaves as it was: the links and the group the
                // order was placed with, which the venue keeps.
                order.parent_id = before.order.parent_id;
                order.oca_group = before.order.oca_group.clone();
                order.oca_type = before.order.oca_type;
            }
            self.intake.placed.insert(order_id, Placed { order, instrument });
        }

        // An order that does not transmit is built and kept, not sent and not
        // refused. One that does sends whatever of its family was kept, in the
        // order it was placed, and then itself. A replace states new terms for
        // an order the venue is already working, so nothing is waiting on it.
        if p.order.transmit {
            // The transmitting order leaves the hold whatever it replaces.
            self.intake.release(order_id);
            let family = if replacing {
                Vec::new()
            } else {
                self.intake.family_of(order_id, p.order.parent_id)
            };
            for member in family {
                if !member.places_the_order() && !self.working(member.order_id) {
                    continue;
                }
                self.context.pending_orders.push(member.command);
            }
            self.context.pending_orders.push(command);
        } else {
            self.intake.keep(order_id, p.order.parent_id, command);
        }
        // What a gateway says about an order it places anyway, on the order's
        // number, as it says it: once the order has gone or is kept.
        for warning in &p.warnings {
            self.shared.orders.push_order_notice(
                order_id,
                op,
                warning.code,
                warning.message.clone(),
            );
        }
        Step::Done
    }

    fn take_cancel(&mut self, order_id: u64, stated: &api::OrderCancel) -> Step {
        // An order this session saw finish is not one it has never heard of:
        // the venue's own answer for it is that it is no longer cancellable.
        if self.shared.orders.number_finished(order_id) {
            self.refuse_order(order_id as i64, OrderOp::Cancel, Refusal::stated(
                NOT_CANCELLABLE,
                format!(
                    "Cancel attempted when order is not in a cancellable state. Order permId = {}",
                    self.shared.orders.get_order_info(order_id).map_or(0, |info| info.order.perm_id),
                ),
            ));
            return Step::Done;
        }
        // Read once the venue has named the account's working set: an order
        // carried over from a previous session is unknown until then, and
        // refusing a withdrawal of a live order is worse than sending one the
        // venue answers. Where the naming did not finish within its bound,
        // nothing is known either way and the withdrawal goes.
        let Some(named) = self.shared.orders.replay_settled() else { return Step::Waits };
        if named && !self.working(order_id) {
            self.refuse_order(
                order_id as i64,
                OrderOp::Cancel,
                Refusal::stated(NO_SUCH_ORDER, format!("no order is working under {order_id}")),
            );
            return Step::Done;
        }
        self.say_the_time_did_not_travel(order_id, stated);
        self.context.pending_orders.push(OrderRequest::Cancel { order_id, stated: stated.clone() });
        Step::Done
    }

    fn take_cancel_by_perm_id(&mut self, perm_id: i64) -> Step {
        // After the venue has named the working set, as a withdrawal by number
        // is read: the order this exists for is one carried over from a
        // previous session.
        if self.shared.orders.replay_settled().is_none() {
            return Step::Waits;
        }
        let found = self
            .shared
            .orders
            .drain_open_orders()
            .into_iter()
            .find(|(_, info)| info.order.perm_id == perm_id)
            .map(|(order_id, _)| order_id);
        let Some(order_id) = found else {
            let why = Refusal::stated(
                NO_SUCH_ORDER,
                format!("cancel_order_by_perm_id: permId {perm_id} not found in open orders"),
            );
            self.shared.push_refused(ErrorOrigin::Session, i64::from(why.code), why.message);
            return Step::Done;
        };
        // Withdrawn under the number the order's own reports carry in this
        // session, as a withdrawal by that number is.
        self.withdraw_kept_placement(order_id);
        self.take_cancel(order_id, &api::OrderCancel::default())
    }

    fn take_global_cancel(&mut self, stated: &api::OrderCancel) -> Step {
        // The venue names what the account is working after the connect, so
        // the withdrawal waits for that naming and covers what was named.
        let Some(named) = self.shared.orders.replay_settled() else { return Step::Waits };
        // One withdrawal per contract the engine holds: this wire carries no
        // withdrawal of everything, so it is composed.
        let count = self.shared.market.instrument_count();
        if count != 0 {
            self.context.pending_orders.push(OrderRequest::GlobalCancel {
                instruments: (0..count).collect(),
                stated: stated.clone(),
            });
        }
        // Only where the venue had begun naming and not finished: an account
        // working nothing is named with nothing, and warning there would cry
        // wolf on every withdrawal against an idle account.
        if !named && self.shared.orders.naming_began() {
            let why = Refusal::no_answer(format!(
                "the venue had not finished naming this account's working orders within \
                 the wait: {count} cancels were sent for what had been named, and what had \
                 not been named is not covered and may still be working",
            ));
            self.shared.push_refused(ErrorOrigin::Session, i64::from(why.code), why.message);
        }
        Step::Done
    }

    /// Take an exercise or a lapse, as a gateway takes one: the account's
    /// position in the option is checked, the option's in-the-money figure is
    /// asked for whatever the override says, the instruction is refused where
    /// it goes against the natural action and the override does not say
    /// otherwise, and what goes is no more than the position.
    fn take_exercise(
        &mut self,
        e: &mut Exercise,
        lookup: &mut Option<u32>,
        watch: &mut Option<i64>,
    ) -> Step {
        if let Some(allocator) = e.allocator.as_ref() {
            if self.shared.orders.replay_settled().is_none() {
                return Step::Waits;
            }
            let floor = self.shared.orders.working_id_watermark().saturating_add(1);
            match allocator.fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |held| {
                    let id = held.max(floor);
                    (id <= crate::bridge::MAX_ORDER_ID).then_some(id.saturating_add(1))
                },
            ) {
                Ok(held) => {
                    e.order_id = held.max(floor);
                    crate::bridge::say_if_past_a_request_id(e.order_id);
                    e.allocator = None;
                }
                Err(_) => {
                    self.refuse_order(
                        e.req_id,
                        OrderOp::Exercise,
                        Refusal::validation("this account has no order id left"),
                    );
                    return Step::Done;
                }
            }
        }
        let c = &e.contract;
        if (c.con_id == 0 && !c.symbol.is_empty())
            || (c.con_id != 0 && (c.sec_type.is_empty() || c.exchange.is_empty()))
        {
            match self.name_order_contract(&mut e.contract, lookup) {
                Ok(true) => {}
                Ok(false) => return Step::Waits,
                Err(why) => {
                    self.refuse_order(e.req_id, OrderOp::Exercise, why);
                    return Step::Done;
                }
            }
        }
        let refused =
            |this: &Self, why: Refusal| this.refuse_order(e.req_id, OrderOp::Exercise, why);
        if let Some(why) = Self::beyond_the_wire(e.contract.con_id) {
            refused(self, why);
            return Step::Done;
        }
        // An exercise takes an order's number, so a number the caller states
        // is under the rules a placement's is: one the venue is working names
        // that order, and the venue refuses the exercise as a repeat of it.
        if e.stated && self.working(e.order_id) {
            refused(
                self,
                Refusal::stated(
                    DUPLICATE_ORDER_ID,
                    format!(
                        "{} is the number of an order the venue is working: an exercise takes an \
                     order's number, so pass 0 to be given one",
                        e.req_id,
                    ),
                ),
            );
            return Step::Done;
        }
        let c = &e.contract;
        let identity = api::contract_identity(
            &c.last_trade_date_or_contract_month,
            c.strike,
            &c.right,
            &c.multiplier,
            &c.currency,
        );
        let instrument = self.register_contract(
            c.con_id,
            c.symbol.clone(),
            &c.sec_type,
            &c.exchange,
            &identity,
            "",
        );
        if watch.is_none() {
            match self.position_to_exercise(e, instrument) {
                Ok(held) => e.qty = e.qty.min(crate::types::qty_from_f64(f64::from(held as i32))),
                Err(why) => {
                    refused(self, why);
                    return Step::Done;
                }
            }
        }
        // The option's standing in the money, asked for whatever the override
        // says. One already held is used at once; otherwise it is watched for,
        // with no bound, as a gateway watches, until the venue states one that
        // says whether the option is in the money.
        let figure = self.shared.market.stated_figures(instrument, IN_THE_MONEY);
        let stated = match (*watch, figure.as_slice()) {
            (None, [value, attribute, ..]) => Some((*value, *attribute)),
            (Some(_), [value, attribute, ..]) if *attribute != 0.0 => Some((*value, *attribute)),
            (Some(_), _) => None,
            (None, _) => {
                // A stop does not wait for one: none is bounded, so the
                // exercise is refused with the stop's own words.
                if self.finishing.is_some() {
                    refused(
                        self,
                        Refusal::not_connected(
                            "the engine stopped before this order reached the venue, so it was \
                         never placed",
                        ),
                    );
                    return Step::Done;
                }
                *watch = Some(self.watch_in_the_money(e));
                None
            }
        };
        let Some((value, attribute)) = stated else { return Step::Waits };
        if let Some(open) = watch.take() {
            self.withdraw_mkt_data(open);
        }
        if let Some(why) = against_the_natural_action(e.action, e.override_, value, attribute) {
            refused(self, why);
            return Step::Done;
        }
        self.context.pending_orders.push(ClientCore::build_exercise_request(
            e.order_id,
            instrument,
            e.action,
            e.qty,
            e.account.clone(),
            e.states.clone(),
        ));
        Step::Done
    }

    /// The account must hold a positive position in the option.
    fn position_to_exercise(&self, e: &Exercise, instrument: InstrumentId) -> Result<f64, Refusal> {
        let account = if e.account.is_empty() { &self.account_id } else { &e.account };
        let portfolio = self.shared.portfolio_for(account);
        let held = portfolio
            .position_info(e.contract.con_id)
            .map_or_else(|| portfolio.position(instrument), |row| row.position);
        if held > 0.0 {
            return Ok(held);
        }
        Err(Refusal::stated(
            crate::error_codes::REQUEST_NOT_PROCESSED,
            format!(
                "Error processing request:No unlapsed position exists in this option in \
                 account {account}.",
            ),
        ))
    }

    /// Watch an option's in-the-money figure for an exercise, under a number
    /// of the engine's own, which nothing is said of to a caller.
    fn watch_in_the_money(&mut self, e: &Exercise) -> i64 {
        self.own_watches += 1;
        let req_id = OWN_WATCHES + self.own_watches;
        self.take_subscription(ControlCommand::Subscribe {
            req_id,
            contract: (&e.contract).into(),
            filters: e.contract.lookup_filters(),
            mode_9887: 0,
            delayed_mode: None,
            regulatory_snapshot: false,
            snapshot: false,
            generic_ticks: vec![IN_THE_MONEY],
            news: None,
            spread_scan: None,
            calculation: None,
        });
        req_id
    }

    /// Refuse, with the stop's words, every exercise still watching for its
    /// option's in-the-money figure: a gateway sets no bound on that wait, so
    /// a stop does not wait for it.
    pub(crate) fn refuse_exercises_watching(&mut self, why: &str) {
        let (watching, rest): (VecDeque<Pending>, VecDeque<Pending>) =
            std::mem::take(&mut self.intake.waiting)
                .into_iter()
                .partition(|p| matches!(p.cmd, ControlCommand::Exercise(_)) && p.watch.is_some());
        self.intake.waiting = rest;
        for pending in watching {
            if let Some(open) = pending.watch {
                self.withdraw_mkt_data(open);
            }
            self.refuse_order_command(&pending.cmd, why);
        }
    }

    fn take_bracket(&mut self, b: &Bracket) -> Step {
        let c = &b.contract;
        if let Some(why) = Self::beyond_the_wire(c.con_id) {
            self.refuse_order(b.parent_id as i64, OrderOp::Place, why);
            return Step::Done;
        }
        let identity = api::contract_identity(
            &c.last_trade_date_or_contract_month,
            c.strike,
            &c.right,
            &c.multiplier,
            &c.currency,
        );
        let instrument = self.register_contract(
            c.con_id,
            c.symbol.clone(),
            &c.sec_type,
            &c.exchange,
            &identity,
            "",
        );
        let parent_id = b.parent_id as i64;
        let (tp_id, sl_id) = (parent_id + 1, parent_id + 2);
        let (action, exit_action) = match b.side {
            crate::types::Side::Buy => ("BUY", "SELL"),
            crate::types::Side::Sell => ("SELL", "BUY"),
            crate::types::Side::ShortSell => ("SSHORT", "BUY"),
        };
        let oca_group = format!("OCA_{parent_id}");
        // Each leg recorded as the wire states it: the entry lives a day and
        // stands alone; each exit is good till cancelled, in the group, and
        // reduces the other on a fill.
        let leg = |order_id: i64,
                   action: &str,
                   order_type: &str,
                   lmt_price: f64,
                   aux_price: f64,
                   parent: i64| {
            let exit = parent != 0;
            api::Order {
                order_id,
                action: action.into(),
                total_quantity: b.quantity,
                order_type: order_type.into(),
                lmt_price,
                aux_price,
                tif: if exit { "GTC" } else { "DAY" }.into(),
                parent_id: parent,
                oca_group: if exit { oca_group.clone() } else { String::new() },
                oca_type: if exit { 3 } else { 0 },
                transmit: true,
                ..Default::default()
            }
        };
        for order in [
            leg(parent_id, action, "LMT", b.entry, 0.0, 0),
            leg(tp_id, exit_action, "LMT", b.take_profit, 0.0, parent_id),
            leg(sl_id, exit_action, "STP", 0.0, b.stop_loss, parent_id),
        ] {
            let order_id = order.order_id as u64;
            self.shared.orders.number_placed_again(order_id);
            self.shared.push_call_record(Record::OrderBook(OrderBook::Taken(Box::new(
                TakenOrder {
                    order_id,
                    contract: c.clone(),
                    order: order.clone(),
                    instrument,
                    restated: false,
                },
            ))));
            self.intake.placed.insert(order_id, Placed { order, instrument });
        }
        let scaled = crate::types::price_from_f64;
        self.context.pending_orders.push(OrderRequest::SubmitBracket {
            con_id: c.con_id,
            parent_id: b.parent_id,
            tp_id: tp_id as u64,
            sl_id: sl_id as u64,
            instrument,
            side: b.side,
            qty: crate::types::qty_from_f64(b.quantity),
            entry_price: scaled(b.entry),
            take_profit: scaled(b.take_profit),
            stop_loss: scaled(b.stop_loss),
        });
        Step::Done
    }

    /// Withdraw a placement still waiting — for its contract's name, or behind
    /// an earlier command — with the changes behind it and what hangs from it,
    /// and say whether there was one. A change waiting to restate an order the
    /// venue is working goes too, and the order it would have changed is
    /// still there to withdraw.
    fn withdraw_waiting_placement(&mut self, order_id: u64) -> bool {
        if self.working(order_id) {
            self.intake
                .waiting
                .retain(|w| !matches!(&w.cmd, ControlCommand::Place(p) if p.order_id == order_id));
            return false;
        }
        let places =
            |w: &Pending, id: u64| matches!(&w.cmd, ControlCommand::Place(p) if p.order_id == id);
        if !self.intake.waiting.iter().any(|w| places(w, order_id)) {
            return false;
        }
        let mut gone = vec![order_id];
        while let Some(parent) = gone.pop() {
            let children: Vec<u64> = self
                .intake
                .waiting
                .iter()
                .filter_map(|w| match &w.cmd {
                    ControlCommand::Place(p) if p.order.parent_id == parent as i64 => {
                        Some(p.order_id)
                    }
                    _ => None,
                })
                .collect();
            self.intake.waiting.retain(|w| !places(w, parent));
            gone.extend(children);
        }
        true
    }

    /// Forget a placement that was kept and never sent, with what hangs from
    /// it, and say whether the order is now gone entirely. A revision kept
    /// under the number goes too, and the order it revises stays live.
    fn withdraw_kept_placement(&mut self, order_id: u64) -> bool {
        let placement = self.intake.keeps_a_placement(order_id);
        let Some(kept) = self.intake.release(order_id) else { return false };
        if !placement {
            // A revision that never left this process is not a revision the
            // record may state.
            let _ = kept;
            self.shared.push_call_record(Record::OrderBook(OrderBook::RevisionForgotten(order_id)));
            return false;
        }
        let mut parents = vec![order_id];
        while let Some(parent) = parents.pop() {
            self.intake.placed.remove(&parent);
            self.shared.push_call_record(Record::OrderBook(OrderBook::Forgotten(parent)));
            let children: Vec<u64> = self
                .intake
                .kept
                .iter()
                .filter(|k| k.parent_id == parent as i64 && k.places_the_order())
                .map(|k| k.order_id)
                .collect();
            for child in children {
                self.intake.release(child);
                parents.push(child);
            }
        }
        true
    }

    /// Withdraw everything the engine holds and has not sent: the orders
    /// kept for a later transmit, and the placements waiting for their
    /// contract's name. A kept revision goes and the order it revises stays.
    fn withdraw_everything_held(&mut self) {
        for kept in std::mem::take(&mut self.intake.kept) {
            let entry = if kept.places_the_order() {
                self.intake.placed.remove(&kept.order_id);
                OrderBook::Forgotten(kept.order_id)
            } else {
                OrderBook::RevisionForgotten(kept.order_id)
            };
            self.shared.push_call_record(Record::OrderBook(entry));
        }
        self.intake
            .waiting
            .retain(|w| !matches!(w.cmd, ControlCommand::Place(_) | ControlCommand::Bracket(_)));
    }

    /// Refuse every order command still waiting, with the words a stop
    /// says them in.
    pub(crate) fn refuse_held_order_commands(&mut self, why: &str) {
        self.intake.waiting_ids.clear();
        for pending in std::mem::take(&mut self.intake.waiting) {
            self.refuse_order_command(&pending.cmd, why);
        }
    }

    /// Refuse an order command the loop will not carry, under what it names,
    /// in the words the order buffer's own refusal says them in.
    pub(crate) fn refuse_order_command(&self, cmd: &ControlCommand, why: &str) {
        let never_placed =
            format!("{why} before this order reached the venue, so it was never placed");
        let stands = |what: &str| {
            format!(
                "{why} before this order's {what} reached the venue, so the order stands as it was"
            )
        };
        let inactive = |order_id: u64, op: OrderOp, what: String| {
            self.shared.orders.push_order_inactive(order_id, op, Refusal::NOT_CONNECTED, what);
        };
        match cmd {
            ControlCommand::Place(p) if self.working(p.order_id) => {
                inactive(p.order_id, OrderOp::Modify, stands("change"))
            }
            ControlCommand::Place(p) => inactive(p.order_id, OrderOp::Place, never_placed),
            ControlCommand::CancelOrder { order_id, .. } => {
                inactive(*order_id, OrderOp::Cancel, stands("cancellation"))
            }
            // An exercise is refused under the number its caller gave it, as
            // every refusal of one is.
            ControlCommand::Exercise(e) => self.shared.push_refused(
                ErrorOrigin::Order { id: e.req_id, op: OrderOp::Exercise },
                i64::from(Refusal::NOT_CONNECTED),
                never_placed,
            ),
            ControlCommand::Bracket(b) => {
                for order_id in [b.parent_id, b.parent_id + 1, b.parent_id + 2] {
                    inactive(order_id, OrderOp::Place, never_placed.clone());
                }
            }
            // Names no order of its own, so it is said under none.
            ControlCommand::CancelOrderByPermId { .. } => self.shared.push_refused(
                ErrorOrigin::Session,
                i64::from(Refusal::NOT_CONNECTED),
                stands("cancellation"),
            ),
            ControlCommand::GlobalCancel { .. } => self.shared.push_refused(
                ErrorOrigin::Session,
                i64::from(Refusal::NOT_CONNECTED),
                format!(
                    "{why} before the withdrawal of every order reached the venue, so \
                     whatever the account was working still is",
                ),
            ),
            _ => unreachable!("intake takes order commands"),
        }
    }

    /// Say that a withdrawal's time does not travel, for a withdrawal that
    /// happens: the order goes without it.
    fn say_the_time_did_not_travel(&self, order_id: u64, stated: &api::OrderCancel) {
        let time = &stated.manual_order_cancel_time;
        if time.is_empty() {
            return;
        }
        self.shared.orders.push_order_inactive(
            order_id,
            OrderOp::Cancel,
            Refusal::VALIDATION,
            format!(
                "a withdrawal states a time, and this client does not send it: a gateway sends one \
                 only where the venue has turned that record on for the login, and this client does \
                 not read whether it has, so the order is withdrawn without it. State it where the \
                 order was placed to have it recorded. (stated: {time})",
            ),
        );
    }
}
