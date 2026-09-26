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
use crate::client_core::{ClientCore, cash_quantity};
use crate::control::contracts::ContractDefinition;
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
            Self::Place(p) => [Some(p.order_id),
                (p.order.pt_order_id != crate::client_core::attached_checks::UNSET_ID).then_some(p.order.pt_order_id as u64),
                (p.order.sl_order_id != crate::client_core::attached_checks::UNSET_ID).then_some(p.order.sl_order_id as u64)],
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
    contract: api::Contract,
    /// The margin its size was worked out from an amount at, where it was.
    sized_at: Option<f64>,
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
    loading: Option<super::attachments::Loading>,
    wire_id: Option<u64>,
    /// Whether this placement has asked the venue for its list of algorithm
    /// definitions, which a gateway asks once for each order that waits on it.
    asked_algorithms: bool,
    deadline: std::time::Instant,
}

impl Pending {
    fn new(cmd: ControlCommand) -> Self {
        Self { cmd, lookup: None, watch: None, loading: None, wire_id: None, asked_algorithms: false,
            deadline: std::time::Instant::now() + super::ccp::CcpState::NAMING_TIMEOUT }
    }
}

/// The series a gateway reads an option's standing in the money off: its
/// intrinsic value, whether it is in the money, and its time value.
const IN_THE_MONEY: u32 = 493;

/// What a gateway warns of, once for each connection, where an order's
/// algorithm is handed a moment without a zone.
const ZONELESS_WARNING: &str = "Warning: You submitted request with date-time attributes without explicit time zone. \
     Please switch to use yyyymmdd-hh:mm:ss in UTC or use instrument time zone, like US/Eastern. Implied time zone \
     functionality will be removed in the next API release";

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
    attached: crate::client_core::attached_orders::AttachedState,
    /// Generated members share the placement that admitted their parent.
    generated: HashMap<u64, u64>,
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
    /// The numbers of orders a gateway leaves taken without placing them: an
    /// order through an algorithm on a contract it puts none on.
    stuck: HashSet<i64>,
    /// Whether this session has warned of a moment stated without a zone,
    /// which a gateway warns of once for each connection.
    warned_zoneless: bool,
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

    /// Whether what is kept under this number would place the order.
    fn keeps_a_placement(&self, order_id: u64) -> bool {
        self.kept.iter().any(|k| k.order_id == order_id && k.places_the_order())
    }

    /// Keep an order back until one that transmits releases it.
    fn keep(&mut self, order_id: u64, parent_id: i64, command: OrderRequest) {
        if let Some(held) = self.kept.iter_mut().find(|k| k.order_id == order_id) {
            held.parent_id = parent_id;
            held.command = command;
        } else {
            self.kept.push(Kept { order_id, parent_id, command });
        }
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
        self.placed.insert(order_id, Placed { order, instrument, contract: api::Contract::default(), sized_at: None });
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
    /// Count a placement once while any of the children it generated remain
    /// unsent. A later explicit amendment is an admission of its own.
    pub(super) fn built_order_commands_held(&self) -> usize {
        let requests = || self.intake.kept.iter().map(|k| &k.command)
            .chain(self.context.pending_orders.iter())
            .chain(self.attached_quote_orders.iter().flat_map(|f| f.orders.iter()));
        let parents: HashSet<_> = requests()
            .filter_map(|request| self.intake.generated.get(&request.order_id()).copied()).collect();
        let mut families = HashSet::new();
        let mut count = 0;
        for request in requests() {
            let id = request.order_id();
            if matches!(request, OrderRequest::SubmitEx { .. })
                && (self.intake.generated.contains_key(&id) || parents.contains(&id))
            {
                families.insert(self.intake.generated.get(&id).copied().unwrap_or(id));
            } else { count += 1; }
        }
        count + families.len()
    }

    pub(super) fn forget_waiting_attachment(&mut self, order_id: u64) {
        self.intake.placed.remove(&order_id);
        self.intake.attached.discard_local_order(order_id);
        self.shared.orders.forget_local_api_order(order_id);
    }

    /// Take an order command a caller admitted.
    pub(crate) fn take_order_command(&mut self, cmd: ControlCommand) {
        let cmd = match cmd {
            ControlCommand::CancelOrder { order_id, stated } => {
                let order_id = self.shared.orders.wire_order_id(order_id as i64).unwrap_or(order_id);
                if self.cancel_waiting_attached(Some(order_id), None) { return; }
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
                self.cancel_waiting_attached(None, None);
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
        // Remove terms only when the venue has finished with an order. A
        // revision then has nothing to revise; a placement kept unsent stays,
        // because the venue has never been given it.
        let orders = &self.shared.orders;
        if orders.take_numbers_changed() {
            self.intake.placed.retain(|id, _| !orders.number_finished(*id));
            self.intake.kept.retain(|k| k.places_the_order() || !orders.number_finished(k.order_id));
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
            ControlCommand::Place(p) => self.take_placement(p, &mut pending.lookup, &mut pending.loading, &mut pending.wire_id, &mut pending.asked_algorithms, pending.deadline),
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
            && !orders.number_finished(order_id);
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

    /// The venue's definitions of the algorithms a contract's orders can go
    /// through, as a gateway gathers them: the list of documents, asked for
    /// once by each order waiting on it; then each document the list names
    /// for the contract's provider group and security type, asked for by the
    /// first order that needs it; then put together. `None` while any is
    /// awaited; `Some(None)` where they do not make a set, as where the
    /// contract names no group.
    fn algorithm_definitions(
        &mut self,
        definition: &ContractDefinition,
        asked: &mut bool,
    ) -> Option<Option<crate::control::algorithms::Algorithms>> {
        use crate::control::algorithms;
        if definition.algo_group.is_empty() {
            return Some(None);
        }
        if !self.shared.reference.algorithms_stated() {
            if !*asked {
                *asked = true;
                if let Err(error) = self.ccp.send_user_message(algorithms::list_request(), &mut self.ccp_conn, &mut self.hb) {
                    log::warn!("the algorithm list was not asked for: {error}");
                }
            }
            return None;
        }
        if let Some(held) = self.shared.reference.algorithm_set(definition) {
            return Some(held);
        }
        let names = self.shared.reference.algorithm_names(definition);
        for name in &names {
            if self.shared.reference.ask_algorithm_document(name)
                && let Err(error) =
                    self.ccp.send_user_message(algorithms::document_request(name), &mut self.ccp_conn, &mut self.hb)
            {
                log::warn!("algorithm document {name} was not asked for: {error}");
            }
        }
        None
    }

    /// The size a gateway works out for an order stated by an amount, from
    /// its record of the contract's market and the account's exchange rates.
    fn cash_estimate(&self, order: &api::Order, contract: &api::Contract, definition: &ContractDefinition, margin: f64) -> f64 {
        let ledger: Vec<(String, f64)> = self
            .shared
            .portfolio_for(&order.account)
            .stated_account_values()
            .into_iter()
            .filter(|(ledger, key, ..)| *ledger && key == "ExchangeRate")
            .filter_map(|(_, _, rate, currency)| Some((currency, rate.parse().ok()?)))
            .collect();
        let cross = cash_quantity::cross_rate(&ledger, &self.shared.reference.money_orders().fixed_rates, definition);
        let market = self.cash_market(order, contract, definition);
        cash_quantity::estimate(&self.shared, order, &contract.exchange, definition, &market, cross, margin)
    }

    /// A gateway's record of a contract's market, as this session holds it:
    /// the instrument it reads the contract under, the account's holding of
    /// it and the contract's rule.
    fn cash_market(&self, order: &api::Order, contract: &api::Contract, definition: &ContractDefinition) -> cash_quantity::Market {
        let con_id = contract.con_id;
        let instrument = self
            .context
            .market
            .instrument_by_con_id(con_id)
            .or_else(|| self.shared.market.attached_quote_instrument(con_id, &contract.exchange));
        let negative = definition
            .market_rule_id
            .and_then(|id| self.shared.reference.market_rule(id as i32))
            .is_some_and(|rule| rule.negative_prices);
        let holding = self
            .shared
            .portfolio_for(&order.account)
            .position_info(con_id)
            .filter(|held| held.market_price_stated)
            .map(|held| held.market_price as f64 / crate::types::PRICE_SCALE as f64);
        match instrument {
            Some(instrument) => cash_quantity::Market::held(
                &self.shared.market.pricing_quote_views(instrument),
                self.context.quote(instrument).halted == 1,
                holding,
                negative,
            ),
            None => cash_quantity::Market { holding, negative, ..Default::default() },
        }
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
    pub(super) fn name_order_contract(
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

    fn take_placement(&mut self, p: &mut Placement, lookup: &mut Option<u32>, loading: &mut Option<super::attachments::Loading>, wire_id: &mut Option<u64>, asked_algorithms: &mut bool, deadline: std::time::Instant) -> Step {
        use crate::client_core::{attached_checks, attached_orders};
        let api_id = p.order_id as i64;
        p.order.order_id = api_id;
        p.order.client_id = self.shared.orders.api_client_id();
        let order_id = self.shared.orders.wire_order_id(api_id).unwrap_or(p.order_id);
        self.intake.attached.learn(&self.shared, order_id, p.order.client_id);
        let existing = self.intake.keeps_a_placement(order_id) || self.working(order_id);
        let op = if existing { OrderOp::Modify } else { OrderOp::Place };
        if self.intake.stuck.contains(&api_id) {
            self.refuse_order(api_id, op, Refusal::stated(2102, "Unable to modify this order as its still being processed."));
            return Step::Done;
        }
        let order_id = if let Some(wire) = *wire_id { wire } else {
        let reusable = std::cell::Cell::new(false);
        // Counted from the highest number used this session, saved before it
        // or stated by the venue for a working order this client placed, as a
        // gateway counts the next id it states. A preview raises none of them,
        // and is judged by this session's numbers alone.
        let highest = if p.order.what_if {
            self.intake.attached.highest
        } else {
            self.intake.attached.highest.max(self.shared.orders.highest_used() as i64)
        };
        if let Err(why) = attached_checks::check_ids(
            api_id, &p.order, highest,
            |id| self.shared.orders.wire_order_id(id).is_some_and(|wire| {
                self.intake.keeps_a_placement(wire) || self.working(wire)
            }),
            |id| self.intake.attached.wire_order_id(id).is_some_and(|wire| {
                let allowed = self.shared.orders.take_order_id_reuse(wire);
                if id == api_id { reusable.set(allowed); }
                allowed
            }),
        ) {
            self.refuse_order(api_id, op, why);
            return Step::Done;
        }
        if !existing && self.shared.orders.number_finished(order_id) && !reusable.get() {
            self.refuse_order(api_id, op, Refusal::stated(DUPLICATE_ORDER_ID, format!("Duplicate order id: {api_id}")));
            return Step::Done;
        }
        if order_id > crate::bridge::MAX_ORDER_ID {
            self.refuse_order(api_id, op, Refusal::validation(format!("place_order: order_id {api_id} is past the highest this client can carry an order under ({})", crate::bridge::MAX_ORDER_ID)));
            return Step::Done;
        }
        let order_id = match self.intake.attached.place_order_id(&self.shared, api_id, &p.allocator) {
            Some(wire) => wire,
            None => { self.refuse_order(api_id, op, Refusal::validation("No venue order identifier is available")); return Step::Done; }
        };
        *wire_id = Some(order_id);
        order_id
        };
        let attaching = attached_checks::requested(&p.order);
        let smart_combo = (attaching || self.intake.attached.family_keys.contains_key(&order_id)) && p.contract.exchange == "SMART"
            && matches!(p.contract.sec_type.as_str(), "BAG" | "COMB" | "COMBO");
        if p.contract.con_id == 0 && !p.contract.symbol.is_empty() && !smart_combo {
            match self.name_order_contract(&mut p.contract, lookup) {
                Ok(true) => {}
                Ok(false) => return Step::Waits,
                Err(why) => {
                    self.refuse_order(api_id, op, why);
                    return Step::Done;
                }
            }
        }
        // An order through an algorithm is held to the venue's own definition
        // of it, as a gateway holds it before anything else about the order:
        // the definitions are asked for the first time an order needs them.
        let mut algorithms = None;
        if !existing && p.contract.con_id != 0 && !p.order.algo_strategy.is_empty() {
            // The contract's definition, and for an order on an overnight venue
            // the definition on the smart route, which names its algorithms.
            let overnight = matches!(p.contract.exchange.as_str(), "OVERNIGHT" | "IBEOS");
            let smart = api::Contract { exchange: "SMART".into(), ..p.contract.clone() };
            for needed in [Some(&p.contract), overnight.then_some(&smart)].into_iter().flatten() {
                if attached_orders::contract_definition(&self.shared, needed).is_none() {
                    match self.name_order_contract(&mut needed.clone(), lookup) {
                        Ok(true) => {}
                        Ok(false) => return Step::Waits,
                        Err(why) => {
                            self.refuse_order(api_id, op, why);
                            return Step::Done;
                        }
                    }
                }
            }
            let definition = attached_orders::contract_definition(&self.shared, &p.contract).unwrap_or_default();
            // Named in the refusal by its symbol, without the slashes a gateway drops.
            if let Some(why) = cash_quantity::algorithm_gate(&definition, &definition.symbol.replace('/', "")) {
                self.intake.stuck.insert(api_id);
                self.refuse_order(api_id, op, why);
                return Step::Done;
            }
            if !p.order.algo_strategy.eq_ignore_ascii_case("AD") {
                let named = if overnight {
                    attached_orders::contract_definition(&self.shared, &smart).unwrap_or_default()
                } else {
                    definition.clone()
                };
                let held = match self.algorithm_definitions(&named, asked_algorithms) {
                    Some(held) => held,
                    None if std::time::Instant::now() < deadline => return Step::Waits,
                    None => {
                        self.refuse_order(api_id, op, Refusal::no_answer("Algorithm definitions timed out"));
                        return Step::Done;
                    }
                };
                let mut zoneless = false;
                let refused = cash_quantity::algorithm_refusal(
                    held.as_ref(), &p.contract, &definition, &p.order, &mut || zoneless = true,
                );
                if zoneless && !self.intake.warned_zoneless {
                    self.intake.warned_zoneless = true;
                    self.shared.orders.push_order_notice(order_id, op, 2174, ZONELESS_WARNING.to_string());
                }
                if let Err(why) = refused {
                    self.refuse_order(api_id, op, why);
                    return Step::Done;
                }
                algorithms = held;
            }
        }
        if !existing
            && let Some(why) = cash_quantity::fractional_refusal(
                &self.shared, &p.contract.sec_type, false, &[p.order.total_quantity],
            )
        {
            self.refuse_order(api_id, op, why);
            return Step::Done;
        }
        // Conditions stated to include the overnight session are refused, when
        // the order is placed, unless the logon enables them and the contract
        // trades on an overnight venue. A gateway asks this of the contract's
        // definition, so one not yet held is asked for. A replace is not asked.
        if !existing && !p.order.conditions.is_empty() && p.order.conditions_include_overnight {
            let enabled = self.shared.reference.enables("CONDINCOVN");
            if enabled && p.contract.con_id != 0
                && attached_orders::contract_definition(&self.shared, &p.contract).is_none()
            {
                match self.name_order_contract(&mut p.contract.clone(), lookup) {
                    Ok(true) => {}
                    Ok(false) => return Step::Waits,
                    Err(why) => {
                        self.refuse_order(api_id, op, why);
                        return Step::Done;
                    }
                }
            }
            let overnight = attached_orders::contract_definition(&self.shared, &p.contract).is_some_and(|definition| {
                definition.valid_exchanges.iter().any(|venue| venue == "OVERNIGHT" || venue == "IBEOS")
            });
            if !(enabled && overnight) {
                self.refuse_order(api_id, op, Refusal::stated(10371, "Conditions include overnight is not supported for this instrument or is not enabled for this account."));
                return Step::Done;
            }
        }
        // An order stated by the cash it spends, or one for a fund, is checked
        // as a gateway checks it before sending it. The contract's definition
        // decides, so one not yet held is asked for.
        if !existing
            && p.contract.con_id != 0
            && (cash_quantity::stated_amount(&p.order).is_some() || p.contract.sec_type == "FUND")
        {
            if attached_orders::contract_definition(&self.shared, &p.contract).is_none() {
                match self.name_order_contract(&mut p.contract.clone(), lookup) {
                    Ok(true) => {}
                    Ok(false) => return Step::Waits,
                    Err(why) => {
                        self.refuse_order(api_id, op, why);
                        return Step::Done;
                    }
                }
            }
            let refused = attached_orders::contract_definition(&self.shared, &p.contract)
                .map(|definition| cash_quantity::refusals(&self.shared, &p.order, &definition, algorithms.as_ref()))
                .unwrap_or_default();
            if !refused.is_empty() {
                for why in refused {
                    self.refuse_order(api_id, op, why);
                }
                return Step::Done;
            }
        }
        let loaded = if attaching && !existing {
            if loading.is_none() {
                let (logon_accounts, advisor) = self.shared.reference.login();
                *loading = Some(super::attachments::Loading::new(self.shared.clone(), p.contract.clone(), crate::client_core::OrderSession {
                    account: self.account_id.clone(), accounts: logon_accounts.clone(), logon_accounts,
                    advisor, features: self.shared.reference.enabled_features(),
                }, deadline));
            }
            match loading.as_mut().unwrap().poll(self) {
                std::task::Poll::Pending => return Step::Waits,
                std::task::Poll::Ready(Err(why)) => {
                    self.refuse_order(api_id, op, why);
                    return Step::Done;
                }
                std::task::Poll::Ready(Ok(loaded)) => {
                    *loading = None;
                    p.order.total_quantity = ClientCore::attached_creation_quantity(&self.shared, &p.contract, &p.order, true);
                    Some(loaded)
                }
            }
        } else { None };
        let mut sized_at = None;
        // An order stated by the cash it spends is given the size a gateway
        // works out for it: its size until the venue states one, and what tag
        // 38 states where the venue does not size it. There the size is
        // raised by the account preset's margin, which is loaded first.
        if !existing
            && let Some(definition) = attached_orders::contract_definition(&self.shared, &p.contract)
            && cash_quantity::works_out_size(&self.shared, &p.order, &definition)
        {
            let margin = if cash_quantity::sized_by_the_venue(&self.shared, &p.contract.sec_type, true) {
                1.0
            } else if let Some((preset, _)) = &loaded {
                cash_quantity::margin(preset.cash_estimate_percent)
            } else {
                let preset = loading.get_or_insert_with(|| {
                    super::attachments::Loading::preset(self.shared.clone(), p.contract.clone(), deadline)
                });
                match preset.poll(self) {
                    std::task::Poll::Pending => return Step::Waits,
                    std::task::Poll::Ready(Err(why)) => {
                        *loading = None;
                        self.refuse_order(api_id, op, why);
                        return Step::Done;
                    }
                    std::task::Poll::Ready(Ok((preset, _))) => {
                        *loading = None;
                        cash_quantity::margin(preset.cash_estimate_percent)
                    }
                }
            };
            p.order.total_quantity = self.cash_estimate(&p.order, &p.contract, &definition, margin);
            sized_at = Some(margin);
        }
        if let Some(why) = Self::beyond_the_wire(p.contract.con_id) {
            self.refuse_order(api_id, op, why);
            return Step::Done;
        }

        let replacing = self.working(order_id) && !self.shared.orders.is_waiting_attached(order_id);
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
        let identity = crate::client_core::attached_combos::registration_identity(&p.contract);
        let instrument = match placed_on {
            Some(placed_on) if p.contract.con_id != 0 && !matches!(p.contract.sec_type.as_str(), "BAG" | "COMB" | "COMBO") => {
                if self.context.market.instrument_by_con_id(p.contract.con_id) != Some(placed_on) {
                    self.refuse_order(
                        api_id,
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
                        api_id,
                        OrderOp::Modify,
                        wrong_contract(&p.contract.symbol),
                    );
                    return Step::Done;
                }
                self.register_contract(
                    if matches!(p.contract.sec_type.as_str(), "BAG" | "COMB" | "COMBO") { 0 } else { p.contract.con_id },
                    p.contract.symbol.clone(),
                    &p.contract.sec_type,
                    &p.contract.exchange,
                    &identity,
                    "",
                )
            }
        };

        // Attached exits share the parent's group, including children placed
        // individually. A scale child retains the group its scale order gave it.
        if !replacing && p.order.parent_id != 0 {
            let parent_id = self.intake.attached.wire_order_id(p.order.parent_id)
                .unwrap_or(p.order.parent_id as u64);
            let scale_parent = self.intake.placed.get(&parent_id)
                .map(|parent| crate::client_core::attached_children::is_scale_order(&parent.order))
                .or_else(|| self.shared.orders.get_order_info(parent_id)
                    .map(|parent| crate::client_core::attached_children::is_scale_order(&parent.order)));
            if let Some(scale_parent) = scale_parent
                && !(scale_parent && p.order.scale_profit_offset != f64::MAX
                    && p.order.scale_profit_offset > 0.0)
            {
                p.order.oca_group = parent_id.to_string();
                if p.order.oca_type == 0 { p.order.oca_type = 3; }
            }
        }

        let mut command = if replacing {
            if placed_on.is_some_and(|placed_on| placed_on != instrument) {
                self.refuse_order(
                    api_id,
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
            let working_size = resting.as_ref().map_or(0.0, |working| working.total_quantity);
            if let Some(refusal) = cash_quantity::modify_refusal(
                &self.shared, p.contract.sec_type == "FUND", resting.as_ref(), &p.order,
            )
            .or_else(|| {
                cash_quantity::fractional_refusal(
                    &self.shared, &p.contract.sec_type, true, &[working_size, p.order.total_quantity],
                )
            })
            .or_else(|| ClientCore::modify_refusal_of(resting, &p.order, Some(&self.shared)))
            {
                self.refuse_order(api_id, OrderOp::Modify, refusal);
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
                    self.refuse_order(api_id, OrderOp::Modify, why);
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
                    self.refuse_order(api_id, OrderOp::Place, why);
                    return Step::Done;
                }
            }
        };

        let mut prepared = match loaded {
            Some((preset, regular)) => {
                let parent_is_scale_child = self.intake.attached.wire_order_id(p.order.parent_id)
                    .and_then(|id| self.intake.placed.get(&id))
                    .is_some_and(|p| crate::client_core::attached_children::is_scale_order(&p.order));
                match self.intake.attached.build(&self.shared, &preset, order_id, &p.contract, &p.order,
                    instrument, &p.allocator, regular, parent_is_scale_child) {
                    Ok(prepared) => prepared,
                    Err(why) => { self.refuse_order(api_id, op, why); return Step::Done; }
                }
            }
            None => None,
        };
        // A held member retains the family's terms and its original position.
        if let Some(before) = self.intake.kept.iter().find(|k| k.order_id == order_id)
            && let (Some(old), Some(attrs)) = (before.command.attrs(), command.attrs_mut())
        {
            attrs.attached.clone_from(&old.attached);
            attrs.parent_id = old.parent_id;
        }
        if prepared.is_some() || self.intake.attached.family_keys.contains_key(&order_id) {
            let definition = attached_orders::contract_definition(&self.shared, &p.contract);
            let legs = attached_orders::confirmed_legs(&self.shared, &p.contract);
            let combo = crate::client_core::attached_combos::attached_combo(&self.shared, &p.contract).and_then(|combo| combo.frame);
            if let Some(attrs) = command.attrs_mut() {
                if let Some(legs) = &legs { attrs.combo_legs.clone_from(legs); }
                let attached = attrs.attached_mut();
                attached.contract_id = definition.as_ref().map(|d| i64::from(d.con_id));
                attached.combo.clone_from(&combo);
            }
            if let (Some(prepared), Some(attrs)) = (&prepared, command.attrs_mut()) {
                attrs.attached_mut().family_key.clone_from(&prepared.parent_family_key);
            }
        }
        if let Some(attrs) = command.attrs_mut() {
            attrs.parent_id = self.intake.attached.wire_order_id(p.order.parent_id).unwrap_or(p.order.parent_id as u64);
            // The program's order and client, which a gateway states on every
            // order a program places.
            attrs.attached_mut().api_identity = Some((api_id, p.order.client_id));
        }
        // A replace goes out with the caller's size. Where the working order's
        // size was worked out from its amount and the replace changes the
        // amount, a gateway then resizes its own record of the order.
        if replacing
            && let Some(margin) = self.intake.placed.get(&order_id).and_then(|working| {
                let cents = |cash: f64| (cash * 1e4).round();
                working.sized_at.filter(|_| cents(working.order.cash_qty) != cents(p.order.cash_qty))
            })
        {
            sized_at = Some(margin);
            if let Some(definition) = attached_orders::contract_definition(&self.shared, &p.contract) {
                let filled = self.context.order(order_id).map_or(0.0, |held| crate::types::qty_to_f64(held.filled));
                let market = self.cash_market(&p.order, &p.contract, &definition);
                if let Some(size) = cash_quantity::resized(
                    &self.shared, &p.order, &p.contract.exchange, &definition, &market, margin, filled,
                ) {
                    p.order.total_quantity = size;
                }
            }
        } else if replacing {
            sized_at = self.intake.placed.get(&order_id).and_then(|working| working.sized_at);
        }
        self.remember_attachment(order_id, api_id, p.order.client_id, &command, p.order.what_if);
        // The caller's side records the order before anything the venue says
        // about it: the record stands ahead of the order in the session's
        // order, and the venue's answer to it after.
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
            self.intake.placed.insert(order_id, Placed { order, instrument, contract: p.contract.clone(), sized_at });
        }

        if self.shared.orders.is_waiting_attached(order_id) {
            if let OrderRequest::SubmitEx { kind, attrs, .. } = command {
                let change = OrderRequest::Modify {
                    order_id, price: ClientCore::replace_price(&p.order),
                    qty: crate::types::qty_from_f64(p.order.total_quantity), outside_rth: p.order.outside_rth,
                    ord_type: p.order.ord_type_byte(), tif: p.order.tif_byte(), stop_price: ClientCore::replace_trigger(&p.order),
                    spec: Some(Box::new(crate::types::OrderSpec { kind, attrs })),
                };
                self.shared.orders.amend_waiting_attached(&change, &[]);
            }
            return Step::Done;
        }
        self.intake.generated.remove(&order_id);
        // Keep every member before releasing the family, so a parent restated
        // after its children still occupies the first position.
        let parent_id = self.intake.attached.wire_order_id(p.order.parent_id).unwrap_or(p.order.parent_id as u64) as i64;
        self.intake.keep(order_id, parent_id, command);
        if let Some(prepared) = prepared.take() {
            for child in prepared.children {
                let ControlCommand::Order(mut request) = child.command else { unreachable!() };
                if self.intake.attached.sent_api_ids.contains(&child.order.order_id) { continue; }
                if let Some(attrs) = request.attrs_mut() {
                    if let Some(legs) = attached_orders::confirmed_legs(&self.shared, &p.contract) { attrs.combo_legs = legs; }
                    let attached = attrs.attached_mut();
                    attached.contract_id = attached_orders::contract_definition(&self.shared, &p.contract).map(|d| i64::from(d.con_id));
                    attached.combo = crate::client_core::attached_combos::attached_combo(&self.shared, &p.contract).and_then(|combo| combo.frame);
                }
                self.intake.generated.insert(child.wire_id, order_id);
                self.remember_attachment(child.wire_id, child.order.order_id, p.order.client_id, &request, p.order.what_if);
                self.shared.push_call_record(Record::OrderBook(OrderBook::Taken(Box::new(TakenOrder {
                    order_id: child.wire_id, contract: p.contract.clone(), order: child.order.clone(), instrument, restated: false,
                }))));
                self.intake.placed.insert(child.wire_id, Placed { order: child.order, instrument, contract: p.contract.clone(), sized_at: None });
                self.intake.keep(child.wire_id, order_id as i64, request);
            }
        }
        if !p.order.what_if { self.intake.attached.highest = self.intake.attached.highest.max(api_id); }
        if p.order.transmit { self.transmit_order_family(order_id, parent_id, replacing); }
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

    fn remember_attachment(&mut self, wire: u64, api: i64, client_id: i32, request: &OrderRequest, preview: bool) {
        self.intake.attached.place(wire, api);
        let attached = request.attrs().and_then(|attrs| attrs.attached.as_ref());
        let key = attached.map(|attrs| attrs.family_key.clone()).filter(|key| !key.is_empty());
        if let Some(key) = &key { self.intake.attached.family_keys.insert(wire, key.clone()); }
        self.shared.orders.note_attached_order_metadata(wire, crate::bridge::AttachedOrderMetadata {
            family_key: key, api_order_id: Some(api), api_client_id: Some(client_id),
            ..Default::default()
        });
        if !preview {
            self.intake.attached.highest = self.intake.attached.highest.max(api);
        }
    }

    fn transmit_order_family(&mut self, order_id: u64, parent_id: i64, replacing: bool) {
        let own_at = self.intake.kept.iter().position(|k| k.order_id == order_id).unwrap();
        let before: HashSet<_> = self.intake.kept[..own_at].iter().map(|k| k.order_id).collect();
        let own = self.intake.release(order_id).unwrap();
        let mut family = if replacing { Vec::new() } else { self.intake.family_of(order_id, parent_id) };
        let at = family.iter().position(|k| !before.contains(&k.order_id)).unwrap_or(family.len());
        family.insert(at, own);
        let root = family[0].order_id;
        let attached = family.iter().any(|k| self.intake.attached.family_keys.contains_key(&k.order_id));
        let mut ids = HashSet::new();
        let mut dropped = Vec::new();
        family.retain(|member| {
            let api = self.intake.attached.api_order_id(member.order_id);
            let keep = !member.places_the_order() || (ids.insert(api)
                && (member.order_id == order_id || !self.intake.attached.sent_api_ids.contains(&api)));
            if !keep { dropped.push(member.order_id); }
            keep
        });
        for wire in dropped {
            self.forget_waiting_attachment(wire);
            self.shared.push_call_record(Record::OrderBook(OrderBook::Forgotten(wire)));
        }
        for member in &family {
            if member.places_the_order() {
                self.intake.attached.sent_api_ids.insert(self.intake.attached.api_order_id(member.order_id));
            }
        }
        if attached {
            let placed = &self.intake.placed[&root];
            let (contract, parent, instrument) = (placed.contract.clone(), placed.order.clone(), placed.instrument);
            let mut orders: Vec<_> = family.into_iter().map(|k| k.command).collect();
            if let Some(factor) = crate::client_core::attached_combos::attached_combo(&self.shared, &contract)
                .and_then(|c| c.legs).map(|(_, factor)| factor).filter(|factor| *factor > 1.0)
            {
                for request in &mut orders {
                    if (request.order_id() == root || request.order_id() == order_id)
                        && let Some(attrs) = request.attrs_mut()
                    { attrs.attached_mut().ratio_factor = Some(factor); }
                }
            }
            self.shared.orders.stage_waiting_attached(&orders);
            self.accept_attached_orders(orders, contract, parent, instrument);
        } else {
            for member in family { self.queue_order_during_quote_wait(member.command); }
        }
    }

    fn take_cancel(&mut self, order_id: u64, stated: &api::OrderCancel) -> Step {
        if self.cancel_waiting_attached(Some(order_id), None) { return Step::Done; }
        if self.withdraw_kept_placement(order_id) {
            self.say_the_time_did_not_travel(order_id, stated);
            return Step::Done;
        }
        let api_id = self.shared.orders.attached_order_metadata(order_id)
            .and_then(|held| held.api_order_id).unwrap_or(order_id as i64);
        // An order this session saw finish is not one it has never heard of:
        // the venue's own answer for it is that it is no longer cancellable.
        if self.shared.orders.number_finished(order_id) {
            self.refuse_order(api_id, OrderOp::Cancel, Refusal::stated(
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
                api_id,
                OrderOp::Cancel,
                Refusal::stated(NO_SUCH_ORDER, format!("no order is working under {api_id}")),
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
        let carrying: Vec<u64> = self
            .shared
            .orders
            .drain_open_orders()
            .into_iter()
            .filter(|(_, info)| info.order.perm_id == perm_id)
            .map(|(order_id, _)| order_id)
            .collect();
        // One order under a number, as a gateway holds its orders. Where more
        // than one record carries the number, it names the order a report
        // under that number reaches: the one held under the number itself, or
        // the one the venue's name for it was learned for. Taken as the first
        // record the book happened to yield, the withdrawal reached either.
        // And of those, the one the engine holds: a record can outlast the
        // order it was kept for.
        let number = perm_id as u64;
        let named: Vec<u64> = [Some(number), self.ccp.the_order_named(number)]
            .into_iter()
            .flatten()
            .filter(|order_id| carrying.contains(order_id))
            .chain(carrying.iter().copied())
            .collect();
        let found = named.iter().copied()
            .find(|order_id| self.context.order(*order_id).is_some())
            .or_else(|| named.first().copied());
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
            match self.shared.orders.number_in_memory(allocator) {
                Ok(id) => {
                    e.order_id = id;
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
            frozen: false,
            delayed_frozen: false,
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
        let oca_group = parent_id.to_string();
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
            self.shared.push_call_record(Record::OrderBook(OrderBook::Taken(Box::new(
                TakenOrder {
                    order_id,
                    contract: c.clone(),
                    order: order.clone(),
                    instrument,
                    restated: false,
                },
            ))));
            self.intake.placed.insert(order_id, Placed { order, instrument, contract: c.clone(), sized_at: None });
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
            self.intake.attached.discard_local_order(parent);
            self.shared.orders.forget_local_api_order(parent);
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
                self.intake.attached.discard_local_order(kept.order_id);
                self.shared.orders.forget_local_api_order(kept.order_id);
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

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::{Arc, atomic::AtomicU64, mpsc};
    use std::time::Duration;

    use crate::bridge::SharedState;
    use crate::control::contracts::{ContractDefinition, SecurityType};
    use crate::protocol::connection::Connection;
    use crate::types::model::{Contract, Order};
    use crate::types::{ControlCommand, OrderCondition, Placement};

    use super::HotLoop;

    /// The algorithm provider's document and its share algorithms, as the
    /// venue answered them on a paper session.
    const ALGORITHM_PROVIDER: &str = include_str!("../../control/fixtures/algorithms_ibalgo_ae.xml");
    const SHARE_ALGORITHMS: &str = include_str!("../../control/fixtures/algorithms_ibalgo_al_stk.xml");

    /// Conditions that count the overnight session are placed only where the
    /// logon enables them and the contract trades on an overnight venue;
    /// anywhere else a gateway refuses them under 10371 and sends nothing. A
    /// contract whose definition is not held is asked for first. A replace is
    /// not asked, and the flag on an order with no conditions states nothing.
    #[test]
    fn conditions_count_the_overnight_session_only_where_a_gateway_takes_them() {
        let refused = Some(10371);
        for (features, venues, held, conditioned, replaced, expected) in [
            (&[][..], &["SMART", "OVERNIGHT"][..], true, true, false, refused),
            (&["CONDINCOVN"][..], &["SMART", "ARCA"][..], true, true, false, refused),
            (&["CONDINCOVN"][..], &["SMART", "OVERNIGHT"][..], true, true, false, None),
            (&["CONDINCOVN"][..], &["SMART", "IBEOS"][..], true, true, false, None),
            (&["CONDINCOVN"][..], &["SMART", "OVERNIGHT"][..], false, true, false, None),
            (&[][..], &["SMART"][..], true, false, false, None),
            (&[][..], &["SMART", "ARCA"][..], true, true, true, None),
        ] {
            let shared = Arc::new(SharedState::new());
            shared.orders.set_replay_done();
            shared.reference.set_enabled_features(features.iter().map(|f| f.to_string()).collect());
            let definition = ContractDefinition {
                con_id: 756733,
                exchange: "SMART".into(),
                valid_exchanges: venues.iter().map(|v| v.to_string()).collect(),
                ..Default::default()
            };
            if held {
                shared.reference.cache_contract_definition(definition.clone());
            }
            let mut engine = HotLoop::new(shared.clone(), None, None);
            let (connection, mut peer) = Connection::for_test();
            peer.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            engine.ccp_conn = Some(connection);
            engine.set_account_id("DU1".into());
            let (send, receive) = mpsc::channel();
            engine.set_control_rx(receive);
            let mut sent = || {
                let mut wire = String::new();
                let mut bytes = [0; 16384];
                while let Ok(n) = peer.read(&mut bytes) {
                    if n == 0 { break; }
                    wire.push_str(&String::from_utf8_lossy(&bytes[..n]));
                }
                wire.replace('\x01', "|")
            };

            let place = |overnight: bool| {
                let mut order = Order::limit("BUY", 1.0, 100.0);
                if conditioned {
                    order.conditions.push(OrderCondition::Time {
                        time: "20260925-20:30:00".into(), is_more: true, is_conjunction_connection: false,
                    });
                }
                order.conditions_include_overnight = overnight;
                shared.admit(&send, ControlCommand::Place(Box::new(Placement {
                    order_id: 10,
                    allocator: Arc::new(AtomicU64::new(11)),
                    contract: Contract {
                        con_id: 756733, symbol: "SPY".into(), sec_type: "STK".into(),
                        exchange: "SMART".into(), currency: "USD".into(), ..Default::default()
                    },
                    order,
                    warnings: Vec::new(),
                }))).unwrap();
            };
            let row = (features, venues, held, conditioned, replaced);
            if replaced {
                place(false);
                (0..3).for_each(|_| engine.poll_once());
                assert!(sent().contains("35=D|"), "{row:?}: working before it is replaced");
            }
            place(true);
            engine.poll_once();
            if !held {
                let asked = sent();
                assert!(asked.contains("35=c|") && !asked.contains("35=D|"), "{row:?}: {asked}");
                // The venue's answer, cached as every definition it states is.
                let (lookup, _) = engine.ccp.order_naming.remove(0);
                shared.reference.cache_contract_definition(definition.clone());
                engine.ccp.orders_named.push((lookup, super::OrderNamed::Contract(Box::new(definition))));
            }
            engine.poll_once();
            engine.poll_once();

            let wire = sent();
            let codes: Vec<_> = shared.drain_refused().into_iter().map(|(_, code, _)| code).collect();
            assert_eq!(codes, expected.into_iter().map(i64::from).collect::<Vec<_>>(), "{row:?}");
            let sent_as = if replaced { "35=G|" } else { "35=D|" };
            assert_eq!(wire.contains(sent_as), expected.is_none(), "{row:?}: {wire}");
            assert_eq!(wire.contains("|8612=1|"), expected.is_none() && conditioned, "{row:?}: {wire}");
        }
    }

    /// An order stated by the cash it spends, and one for a fund, is checked,
    /// sized and sent as a gateway does. A share takes an amount only through
    /// an algorithm on a type the logon takes amounts on, a currency pair
    /// where its order types list `CASHQTY`, a crypto except as a stop, a
    /// limit or a sale at market, a fund only to buy. A pair's size is whole
    /// and an amount a multiple of its currency's least amount, or of no more
    /// than two places where that is waived; a fund bought by an amount keeps
    /// to its own least amount where the logon holds it to one. A fund is
    /// bought by an amount alone, stating a size of one, and sold by a
    /// quantity alone. The size a gateway works out for an amount is the
    /// order's size — at the market's price, the order's own or the preset's
    /// margin — and is stated on tag 38 only where the venue does not work
    /// it out; a size the caller states on a pair passes through. A
    /// definition not held is asked for first.
    #[test]
    fn a_cash_order_is_checked_and_sent_as_a_gateway_sends_it() {
        #[derive(Clone)]
        struct Row {
            what: &'static str,
            sec_type: SecurityType,
            currency: &'static str,
            features: &'static [&'static str],
            action: &'static str,
            order_type: &'static str,
            limit: f64,
            algo: &'static str,
            /// The algorithm's parameters the order states.
            params: &'static [(&'static str, &'static str)],
            total: f64,
            cash: f64,
            held: bool,
            /// The finest size the contract's rule deals in.
            finest: Option<f64>,
            /// The most places the rule shows a size to.
            size_places: Option<i32>,
            /// The live bid, ask and last trade, each with a size.
            market: Option<(f64, f64, f64)>,
            /// The fund's least amount, where it states one.
            tick: Option<&'static str>,
            /// The margin percentage the account's preset states, where one
            /// is seeded.
            percent: Option<&'static str>,
            /// A model the order is allocated to.
            model: &'static str,
            /// The logon's list of the order types it takes amounts on, where
            /// a row states its own.
            order_types: Option<&'static str>,
            refused: &'static [i64],
            text: &'static str,
            size: Option<&'static str>,
            /// The size the order is held at.
            held_at: Option<f64>,
        }
        let share = Row {
            what: "a share through an algorithm, the estimate off",
            sec_type: SecurityType::Stock,
            currency: "USD",
            features: &["DISABLECASHQTYOVEREST"],
            action: "BUY",
            order_type: "LMT",
            limit: 1.0,
            algo: "Adaptive",
            params: &[],
            total: 0.0,
            cash: 50.0,
            held: true,
            finest: None,
            size_places: None,
            market: None,
            tick: None,
            percent: None,
            model: "",
            order_types: None,
            refused: &[],
            text: "",
            size: None,
            // No market: a buy converts at its limit.
            held_at: Some(50.0),
        };
        let pair = Row {
            what: "a pair listing CASHQTY, the estimate off",
            sec_type: SecurityType::Forex,
            features: &["DISABLEFXCASHQTYOVEREST"],
            algo: "",
            cash: 1000.5,
            held_at: Some(1001.0),
            ..share.clone()
        };
        let crypto = Row {
            what: "a crypto bought at market",
            sec_type: SecurityType::Crypto,
            features: &[],
            order_type: "MKT",
            algo: "",
            // No market and no limit: nothing to convert at.
            held_at: Some(0.0),
            ..share.clone()
        };
        let fund = Row {
            what: "a fund bought by an amount",
            sec_type: SecurityType::Fund,
            features: &[],
            algo: "",
            size: Some("1"),
            held_at: None,
            ..share.clone()
        };
        for row in [
            share.clone(),
            Row { what: "a share without an algorithm", algo: "", refused: &[10244], ..share.clone() },
            Row {
                what: "a share through an algorithm defined without an amount",
                algo: "AccuDistr", params: &[("componentSize", "100"), ("timeBetweenOrders", "60")], refused: &[10244],
                ..share.clone()
            },
            Row { what: "a share on a type the logon takes no amounts on", order_type: "MIT", refused: &[10244], ..share.clone() },
            Row { what: "a definition not held", held: false, ..share.clone() },
            Row {
                what: "a share with the estimate on, at the preset's margin over its limit",
                features: &[],
                limit: 2.0,
                finest: Some(0.0001),
                size: Some("31.25"),
                held_at: Some(31.25),
                ..share.clone()
            },
            Row {
                what: "a share at the preset's whole margin over its limit",
                features: &[], percent: Some("100"), limit: 2.0, finest: Some(0.0001),
                size: Some("50"), held_at: Some(50.0),
                ..share.clone()
            },
            Row {
                what: "a preset margin past a hundred takes the default",
                features: &[], percent: Some("101"), limit: 2.0, finest: Some(0.0001),
                size: Some("31.25"), held_at: Some(31.25),
                ..share.clone()
            },
            Row {
                what: "a size worked out at a tie rounds half to even",
                features: &[], limit: 20.0, finest: Some(0.01),
                size: Some("3.12"), held_at: Some(3.12),
                ..share.clone()
            },
            Row {
                what: "a tie at an odd place rounds half to even up",
                features: &[], cash: 19.76, limit: 20.0, finest: Some(0.01),
                size: Some("1.24"), held_at: Some(1.24),
                ..share.clone()
            },
            Row {
                what: "a share allocated to a model on a logon taking no allocated amounts",
                model: "MDL", order_types: Some("DAY,GTC,LMT,MKT,STP,STPLMT"),
                refused: &[10244],
                ..share.clone()
            },
            Row {
                what: "a whole-unit share at market, up to a whole unit",
                features: &[],
                order_type: "MKT",
                finest: Some(1.0),
                market: Some((29.0, 31.0, 30.0)),
                size: Some("3"),
                held_at: Some(3.0),
                ..share.clone()
            },
            pair.clone(),
            Row { what: "a pair stating a size passes it through", total: 1000.0, held_at: Some(1000.0), ..pair.clone() },
            Row { what: "a pair stating part of a unit", total: 1000.5, refused: &[10318], ..pair.clone() },
            Row {
                what: "a pair's amount finer than its currency",
                cash: 1000.505,
                refused: &[10317],
                text: "The Cash Quantity size of 1000.505 does not conform to minimum variation of 0.01 for this contract",
                ..pair.clone()
            },
            Row { what: "a future", sec_type: SecurityType::Future, algo: "", refused: &[10244], ..share.clone() },
            Row { what: "a pair in a currency not listed", currency: "XYZ", refused: &[10318], ..pair.clone() },
            Row { what: "a pair in a currency with a fixed rate", currency: "GBX", ..pair.clone() },
            crypto.clone(),
            Row {
                what: "a crypto sized at the midpoint of its quote",
                finest: Some(0.00000001),
                market: Some((100.0, 102.0, f64::MAX)),
                held_at: Some(0.4950495),
                ..crypto.clone()
            },
            Row {
                what: "a crypto's amount finer than a cent",
                cash: 50.555,
                refused: &[10317],
                text: "The Cash Quantity size of 50.555 does not conform to minimum variation of 0.01 for this contract",
                ..crypto.clone()
            },
            Row {
                what: "a crypto's amount past two places where cents are waived",
                features: &["NOCASHQTYPRECISION"],
                cash: 50.555,
                refused: &[10206],
                text: "Non-zero cash quantity cannot contain more than 2 decimals.",
                ..crypto.clone()
            },
            Row { what: "a crypto stating a size too", total: 1.0, refused: &[10293], ..crypto.clone() },
            Row {
                what: "a crypto stating a size and an amount finer than a cent",
                total: 1.0,
                cash: 50.555,
                refused: &[10317, 10293],
                ..crypto.clone()
            },
            Row { what: "a crypto limit", order_type: "LMT", refused: &[10244], ..crypto.clone() },
            Row { what: "a crypto sold at market", action: "SELL", refused: &[10244], ..crypto.clone() },
            Row { what: "a crypto stop to buy", order_type: "STP", refused: &[10292], ..crypto.clone() },
            fund.clone(),
            Row {
                what: "a fund bought finer than its least amount",
                features: &["MFCASHQTYINCR"],
                cash: 50.55,
                tick: Some("0.1"),
                refused: &[10317],
                text: "The Cash Quantity size of 50.55 does not conform to minimum variation of 0.1 for this contract",
                ..fund.clone()
            },
            Row { what: "a fund bought with a size too", total: 5.0, refused: &[10203], ..fund.clone() },
            Row { what: "a fund bought without an amount", cash: 0.0, refused: &[10203], ..fund.clone() },
            Row { what: "a fund sold by an amount", action: "SELL", refused: &[10244], ..fund.clone() },
            Row { what: "a fund sold without a size", action: "SELL", cash: 0.0, refused: &[10204], ..fund.clone() },
            Row { what: "a fund sold by a size", action: "SELL", cash: 0.0, total: 3.0, size: Some("3"), ..fund.clone() },
            Row { what: "a fund sold by a size past three places", action: "SELL", cash: 0.0, total: 3.0001, refused: &[10207], ..fund.clone() },
            Row {
                what: "a fund dealt in parts sold to the places its rule shows",
                action: "SELL", cash: 0.0, total: 3.0001, finest: Some(0.0001), size_places: Some(4), size: Some("3.0001"),
                ..fund.clone()
            },
            Row {
                what: "a fund dealt in parts sold past the places its rule shows",
                action: "SELL", cash: 0.0, total: 3.00001, finest: Some(0.00001), size_places: Some(4), refused: &[10207],
                ..fund.clone()
            },
            Row {
                what: "a fund dealt in parts whose rule shows no places",
                action: "SELL", cash: 0.0, total: 3.00001, finest: Some(0.00001), size: Some("3.00001"),
                ..fund.clone()
            },
        ] {
            let shared = Arc::new(SharedState::new());
            shared.orders.set_replay_done();
            shared.reference.set_enabled_features(row.features.iter().map(|f| f.to_string()).collect());
            shared.reference.set_money_orders(crate::bridge::MoneyOrderTerms {
                order_types: row.order_types.unwrap_or("ALLOC,DAY,GTC,LMT,MKT,STP,STPLMT").into(),
                product_defaults: "CASH,USD,25000,1000000,0.01".into(),
                fixed_rates: "GBX:0.0132,USD:1".into(),
                ..Default::default()
            });
            shared.reference.set_order_presets(Vec::new());
            if let Some(percent) = row.percent {
                shared.reference.set_order_permissions([("STK".to_string(), Vec::new())].into());
                shared.reference.set_order_presets(vec![("s=STK".into(), "a=1".into(), "1".into())]);
                let (request_key, _) = shared.reference.expect_order_preset_values("s=STK");
                shared.reference.set_order_preset_values(crate::control::order_presets::PresetValues {
                    request_key, key: "s=STK".into(), attributes: "a=1".into(), error: None,
                    fields: vec![(4014, percent.to_string())],
                });
            }
            shared.reference.set_algorithms(
                [("IBALGO/STK".to_string(), vec!["IBALGO-AE".to_string(), "IBALGO-AL-STK".to_string()])].into(),
            );
            shared.reference.note_algorithm_document("IBALGO-AE", ALGORITHM_PROVIDER);
            shared.reference.note_algorithm_document("IBALGO-AL-STK", SHARE_ALGORITHMS);
            if let Some(finest) = row.finest {
                shared.reference.push_market_rules(vec![crate::control::contracts::MarketRule {
                    rule_id: 26,
                    negative_prices: false,
                    price_magnifier: 1,
                    price_increments: Vec::new(),
                    size_increments: vec![crate::control::contracts::PriceIncrement { low_edge: 0.0, increment: finest }],
                    price_places: None,
                    size_places: row.size_places,
                }]);
            }
            let definition = ContractDefinition {
                con_id: 265598,
                sec_type: row.sec_type.clone(),
                exchange: "SMART".into(),
                currency: row.currency.into(),
                order_types: vec!["LMT".into(), "CASHQTY".into()],
                order_type_rules: vec![("LMT".into(), 1), ("CASHQTY".into(), 1)],
                market_rule_id: row.finest.map(|_| 26),
                algo_group: "IBALGO".into(),
                unnamed_fields: row.tick.map(|tick| (8482, tick.to_string())).into_iter().collect(),
                ..Default::default()
            };
            if row.held {
                shared.reference.cache_contract_definition(definition.clone());
            }
            let mut engine = HotLoop::new(shared.clone(), None, None);
            if let Some((bid, ask, last)) = row.market {
                let instrument = engine.context.market.register(265598);
                let scaled = |price: f64| if price == f64::MAX { 0 } else { (price * crate::types::PRICE_SCALE as f64) as i64 };
                let quote = crate::types::Quote {
                    bid: scaled(bid), ask: scaled(ask), last: scaled(last),
                    bid_size: crate::types::QTY_SCALE, ask_size: crate::types::QTY_SCALE, last_size: crate::types::QTY_SCALE,
                    ..Default::default()
                };
                let present = if last == f64::MAX { 0x33 } else { 0x77 };
                shared.market.push_pricing_quote(instrument, 0, &quote, present, 0);
                shared.market.note_pricing_subscription(instrument, 0, true);
            }
            let (connection, mut peer) = Connection::for_test();
            peer.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            engine.ccp_conn = Some(connection);
            engine.set_account_id("DU1".into());
            let (send, receive) = mpsc::channel();
            engine.set_control_rx(receive);
            let mut sent = || {
                let mut wire = String::new();
                let mut bytes = [0; 16384];
                while let Ok(n) = peer.read(&mut bytes) {
                    if n == 0 { break; }
                    wire.push_str(&String::from_utf8_lossy(&bytes[..n]));
                }
                wire.replace('\x01', "|")
            };
            let order = Order {
                action: row.action.into(),
                order_type: row.order_type.into(),
                lmt_price: row.limit,
                aux_price: 1.0,
                total_quantity: row.total,
                cash_qty: row.cash,
                algo_strategy: row.algo.into(),
                algo_params: row.params.iter().map(|(tag, value)| crate::types::model::TagValue {
                    tag: tag.to_string(), value: value.to_string(),
                }).collect(),
                model_code: row.model.into(),
                tif: "DAY".into(),
                ..Default::default()
            };
            shared.admit(&send, ControlCommand::Place(Box::new(Placement {
                order_id: 10,
                allocator: Arc::new(AtomicU64::new(11)),
                contract: Contract {
                    con_id: 265598, symbol: "X".into(), sec_type: row.sec_type.to_api_str().into(),
                    exchange: "SMART".into(), currency: row.currency.into(), ..Default::default()
                },
                order,
                warnings: Vec::new(),
            }))).unwrap();
            engine.poll_once();
            if !row.held {
                assert!(sent().contains("35=c|"), "{}: asked first", row.what);
                let (lookup, _) = engine.ccp.order_naming.remove(0);
                shared.reference.cache_contract_definition(definition.clone());
                engine.ccp.orders_named.push((lookup, super::OrderNamed::Contract(Box::new(definition))));
            }
            engine.poll_once();
            engine.poll_once();
            let wire = sent();
            let told = shared.drain_refused();
            let codes: Vec<_> = told.iter().map(|(_, code, _)| *code).collect();
            assert_eq!(codes, row.refused, "{}", row.what);
            if !row.text.is_empty() {
                assert_eq!(told[0].2, row.text, "{}", row.what);
            }
            let Some(submit) = wire.split("35=D|").nth(1) else {
                assert!(!row.refused.is_empty(), "{}: nothing sent: {wire}", row.what);
                continue;
            };
            assert!(row.refused.is_empty(), "{}: sent though refused: {submit}", row.what);
            let stated = submit.split('|').find_map(|field| field.strip_prefix("38="));
            assert_eq!(stated, row.size, "{}: {submit}", row.what);
            if let Some(held_at) = row.held_at {
                let placed = engine.intake.placed.values().next().expect("placed");
                assert_eq!(placed.order.total_quantity, held_at, "{}", row.what);
            }
        }
    }

    /// A replace of an order stated by an amount is refused under 10241 where
    /// the account may trade crypto or the logon takes amounts on market,
    /// limit, stop or stop-limit orders; otherwise it goes, stating no size
    /// where the venue works it out, and the order worked out from its amount
    /// is held at the size the new amount comes to, rounded up to the most
    /// places the contract's rule shows a size to; a rule showing none leaves
    /// it at the size the replace states.
    #[test]
    fn a_cash_order_is_not_replaced_where_a_gateway_refuses_it() {
        for (crypto, order_types, size_places, refused, held_at) in [
            (true, "", Some(8), Some(10241), 0.4950495),
            (false, "LMT", Some(8), Some(10241), 0.4950495),
            (false, "LMT/4", Some(8), None, 0.594059),
            (false, "LMT/4", Some(2), None, 0.6),
            (false, "LMT/4", None, None, 0.0),
        ] {
            let shared = Arc::new(SharedState::new());
            shared.orders.set_replay_done();
            if crypto {
                shared.reference.set_order_permissions([("CRYPTO".to_string(), Vec::new())].into());
            }
            shared.reference.set_money_orders(crate::bridge::MoneyOrderTerms {
                order_types: order_types.into(),
                ..Default::default()
            });
            shared.reference.push_market_rules(vec![crate::control::contracts::MarketRule {
                rule_id: 26,
                negative_prices: false,
                price_magnifier: 1,
                price_increments: Vec::new(),
                size_increments: vec![crate::control::contracts::PriceIncrement { low_edge: 0.0, increment: 0.00000001 }],
                price_places: None,
                size_places,
            }]);
            shared.reference.cache_contract_definition(ContractDefinition {
                con_id: 479624278,
                sec_type: SecurityType::Crypto,
                exchange: "PAXOS".into(),
                currency: "USD".into(),
                market_rule_id: Some(26),
                ..Default::default()
            });
            let mut engine = HotLoop::new(shared.clone(), None, None);
            let instrument = engine.context.market.register(479624278);
            let quote = crate::types::Quote {
                bid: 100 * crate::types::PRICE_SCALE, ask: 102 * crate::types::PRICE_SCALE,
                bid_size: crate::types::QTY_SCALE, ask_size: crate::types::QTY_SCALE,
                ..Default::default()
            };
            shared.market.push_pricing_quote(instrument, 0, &quote, 0x33, 0);
            let (connection, mut peer) = Connection::for_test();
            peer.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            engine.ccp_conn = Some(connection);
            engine.set_account_id("DU1".into());
            let (send, receive) = mpsc::channel();
            engine.set_control_rx(receive);
            let mut sent = || {
                let mut wire = String::new();
                let mut bytes = [0; 16384];
                while let Ok(n) = peer.read(&mut bytes) {
                    if n == 0 { break; }
                    wire.push_str(&String::from_utf8_lossy(&bytes[..n]));
                }
                wire.replace('\x01', "|")
            };
            let place = |cash: f64| {
                shared.admit(&send, ControlCommand::Place(Box::new(Placement {
                    order_id: 10,
                    allocator: Arc::new(AtomicU64::new(11)),
                    contract: Contract {
                        con_id: 479624278, symbol: "BTC".into(), sec_type: "CRYPTO".into(),
                        exchange: "PAXOS".into(), currency: "USD".into(), ..Default::default()
                    },
                    order: Order {
                        action: "BUY".into(), order_type: "MKT".into(), cash_qty: cash,
                        tif: "IOC".into(), ..Default::default()
                    },
                    warnings: Vec::new(),
                }))).unwrap();
            };
            place(50.0);
            (0..3).for_each(|_| engine.poll_once());
            assert!(sent().contains("35=D|"), "{crypto} {order_types}: working first");
            place(60.0);
            (0..3).for_each(|_| engine.poll_once());
            let wire = sent();
            let codes: Vec<_> = shared.drain_refused().into_iter().map(|(_, code, _)| code).collect();
            assert_eq!(codes, refused.into_iter().collect::<Vec<_>>(), "{crypto} {order_types}");
            let replace = wire.split("35=G|").nth(1);
            assert_eq!(replace.is_some(), refused.is_none(), "{crypto} {order_types}: {wire}");
            if let Some(replace) = replace {
                assert!(!replace.split('|').any(|field| field.starts_with("38=")), "{replace}");
            }
            let placed = engine.intake.placed.values().next().expect("placed");
            assert_eq!(placed.order.total_quantity, held_at, "{crypto} {order_types}");
        }
    }

    /// An order through an algorithm is held to the venue's definition of it,
    /// as a gateway holds it, before anything else is checked. A contract a
    /// gateway puts no algorithms on — a fund traded at its settlement, the
    /// settlement venue, a contract priced other than in currency — is refused
    /// first (10015), and its order number stays taken (2102). Otherwise the
    /// list of definitions is asked for, then each definition it names for the
    /// contract's provider group and security type, and the order waits on
    /// them. It is refused where the contract names no group or the list names
    /// nothing for it (442); where the algorithm is not defined under the name
    /// given (439); where a parameter is one the algorithm does not take (443)
    /// or is its amount (10205); where a moment is not in a form a gateway
    /// reads (10314), or a number does not read or falls outside its bounds, a
    /// required parameter is missing or the contract does not take one
    /// (441); where a text is not among its legal strings (145); and where
    /// the order trades overnight through an algorithm the session does not
    /// take (442). A moment without a zone is warned of, once (2174).
    #[test]
    fn an_algorithm_order_is_held_to_the_venues_definition() {
        let list = "IBALGO/STK:IBALGO-AE,IBALGO-AL-STK;IBALGO/CASH:IBALGO-AE,IBALGO-AL-CASH";
        #[derive(Clone)]
        struct Row {
            what: &'static str,
            group: &'static str,
            stock_type: &'static str,
            ev_rule: &'static str,
            algo: &'static str,
            params: &'static [(&'static str, &'static str)],
            overnight: bool,
            stated: &'static str,
            refused: &'static [i64],
            text: &'static str,
            warned: bool,
        }
        let defined = Row {
            what: "an algorithm the venue defines",
            group: "IBALGO",
            stock_type: "COMMON",
            ev_rule: "",
            algo: "Adaptive",
            params: &[("adaptivePriority", "Urgent")],
            overnight: false,
            stated: list,
            refused: &[],
            text: "",
            warned: false,
        };
        let rows = [
            defined.clone(),
            Row { what: "a contract naming no provider group", group: "", refused: &[442], ..defined.clone() },
            Row { what: "a list naming nothing for the contract", stated: "IBALGO/CASH:IBALGO-AE", refused: &[442], ..defined.clone() },
            Row { what: "an algorithm not defined under the name", algo: "adaptive", params: &[], refused: &[439], ..defined.clone() },
            Row {
                what: "a parameter the algorithm does not take", params: &[("speed", "1")], refused: &[443],
                text: "Order processing failed. Unknown algo attribute  :speed", ..defined.clone()
            },
            Row {
                what: "an amount as the algorithm's parameter", algo: "Vwap", params: &[("monetaryValue", "100")],
                refused: &[10205],
                text: "Cash Quantity cannot be send in monetaryValue field in Algo . Please try sending in Cash Quantity field.",
                ..defined.clone()
            },
            Row { what: "an overnight order through an algorithm the session does not take", algo: "Vwap", params: &[], overnight: true, refused: &[442], ..defined.clone() },
            Row { what: "an overnight order through one it takes", overnight: true, ..defined.clone() },
            Row { what: "a moment with a zone", algo: "Vwap", params: &[("startTime", "09:30:00 US/Eastern")], ..defined.clone() },
            Row { what: "a moment in UTC", algo: "Vwap", params: &[("startTime", "20261002-13:30:00")], ..defined.clone() },
            Row { what: "a moment without a zone", algo: "Vwap", params: &[("startTime", "09:30:00")], warned: true, ..defined.clone() },
            Row { what: "a moment without seconds", algo: "Vwap", params: &[("startTime", "09:30 US/Eastern")], refused: &[10314], ..defined.clone() },
            Row { what: "a moment in a zone never read", algo: "Vwap", params: &[("startTime", "09:30:00 CST")], refused: &[10314], ..defined.clone() },
            Row { what: "a moment in a zone across the sea", algo: "Vwap", params: &[("startTime", "20261002 13:30:00 Europe/London")], ..defined.clone() },
            Row {
                what: "a number that does not read", algo: "Vwap", params: &[("maxPctVol", "abc")], refused: &[441],
                text: "Algo attributes validation failed:maxPctVol=abc", ..defined.clone()
            },
            Row {
                what: "a number past its bound, in its units", algo: "Vwap", params: &[("maxPctVol", "0.6")], refused: &[441],
                text: "Algo attributes validation failed:\n'Max Percentage' is invalid: value is greater than maximum value 50.0.\n",
                ..defined.clone()
            },
            Row { what: "a number within its bound, in its units", algo: "Vwap", params: &[("maxPctVol", "0.1")], ..defined.clone() },
            Row {
                what: "a required parameter missing", algo: "ArrivalPx", params: &[], refused: &[441],
                text: "Algo attributes validation failed:\n'Max Percentage' is invalid: value is required.\n",
                ..defined.clone()
            },
            Row {
                what: "every failure, in the order a gateway's map reads them", algo: "PctVolPx", params: &[],
                refused: &[441],
                text: "Algo attributes validation failed:\n'Minimum Target Percentage' is invalid: value is required.\n\
                       'Target Percentage' is invalid: value is required.\n\
                       'Target Percentage Change Rate' is invalid: value is required.\n\
                       'Maximum Target Percentage' is invalid: value is required.\n",
                ..defined.clone()
            },
            Row {
                what: "a text not among its legal strings", algo: "ArrivalPx", params: &[("maxPctVol", "0.1"), ("riskAversion", "Bold")],
                refused: &[145], text: "Error in validating entry fields -Bold", ..defined.clone()
            },
            Row {
                what: "a parameter the contract does not take", algo: "Vwap", params: &[("optoutOpeningAuction", "1")], refused: &[441],
                text: "Algo attributes validation failed:\nThe usage of algorithm parameter optoutOpeningAuction is not allowed on NASDAQ",
                ..defined.clone()
            },
            Row { what: "a fund traded at its settlement", stock_type: "ETMF", refused: &[10015, 2102], ..defined.clone() },
            Row {
                what: "a contract priced other than in currency", ev_rule: "aussieBond:1", refused: &[10015, 2102],
                text: "The algo orders are not supported because product is trading on the basis other than currency price",
                ..defined.clone()
            },
            Row { what: "a contract priced by a factor", ev_rule: "factor:0.5", ..defined.clone() },
        ];
        for row in rows {
            let what = row.what;
            let shared = Arc::new(SharedState::new());
            shared.orders.set_replay_done();
            shared.reference.cache_contract_definition(ContractDefinition {
                con_id: 265598,
                sec_type: SecurityType::Stock,
                exchange: "SMART".into(),
                primary_exchange: "NASDAQ".into(),
                currency: "USD".into(),
                algo_group: row.group.into(),
                stock_type: row.stock_type.into(),
                ev_rule: row.ev_rule.into(),
                order_type_rules: vec![("LMT".into(), 1), ("ALGO".into(), 1)],
                ..Default::default()
            });
            let mut engine = HotLoop::new(shared.clone(), None, None);
            let (connection, mut peer) = Connection::for_test();
            peer.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            engine.ccp_conn = Some(connection);
            engine.set_account_id("DU1".into());
            let (send, receive) = mpsc::channel();
            engine.set_control_rx(receive);
            let mut sent = || {
                let mut wire = String::new();
                let mut bytes = [0; 16384];
                while let Ok(n) = peer.read(&mut bytes) {
                    if n == 0 { break; }
                    wire.push_str(&String::from_utf8_lossy(&bytes[..n]));
                }
                wire.replace('\x01', "|")
            };
            let place = |engine: &mut HotLoop| {
                shared.admit(&send, ControlCommand::Place(Box::new(Placement {
                    order_id: 10,
                    allocator: Arc::new(AtomicU64::new(11)),
                    contract: Contract {
                        con_id: 265598, symbol: "AAPL".into(), sec_type: "STK".into(),
                        exchange: "SMART".into(), currency: "USD".into(), ..Default::default()
                    },
                    order: Order {
                        action: "BUY".into(), order_type: "LMT".into(), lmt_price: 1.0, total_quantity: 1.0,
                        tif: "DAY".into(), algo_strategy: row.algo.into(), include_overnight: row.overnight,
                        algo_params: row.params.iter().map(|(tag, value)| crate::types::model::TagValue {
                            tag: tag.to_string(), value: value.to_string(),
                        }).collect(),
                        ..Default::default()
                    },
                    warnings: Vec::new(),
                }))).unwrap();
                engine.poll_once();
            };
            place(&mut engine);
            let asked = sent();
            let answer = |engine: &mut HotLoop, frame: String| {
                engine.ccp.process_ccp_message(
                    frame.as_bytes(), &mut None, &mut engine.context, &shared, &None, &mut engine.hb, "DU1",
                );
            };
            let gated = !row.stock_type.eq("COMMON") || row.ev_rule.starts_with("aussie");
            if !row.group.is_empty() && !gated {
                assert_eq!(asked.matches("|6040=80|").count(), 1, "{what}: the list asked for once: {asked}");
                answer(&mut engine, format!("35=U\x016040=81\x016597={}\x01", row.stated));
                engine.poll_once();
                let asked = sent();
                let names: &[&str] = if row.stated == list { &["IBALGO-AE", "IBALGO-AL-STK"] } else { &[] };
                for name in names {
                    assert_eq!(asked.matches(&format!("|6040=53|6364={name}|")).count(), 1, "{what}: {name}: {asked}");
                }
                answer(&mut engine, format!("35=U\x016040=54\x016364=IBALGO-AE\x016118={ALGORITHM_PROVIDER}\x01"));
                answer(&mut engine, format!("35=U\x016040=54\x016364=IBALGO-AL-STK\x016118={SHARE_ALGORITHMS}\x01"));
            } else {
                assert!(!asked.contains("|6040=80|"), "{what}: nothing asked: {asked}");
            }
            engine.poll_once();
            engine.poll_once();
            if gated {
                // The same number placed again.
                place(&mut engine);
                engine.poll_once();
            }
            let wire = sent();
            let told = shared.drain_refused();
            assert_eq!(told.iter().map(|(_, code, _)| *code).collect::<Vec<_>>(), row.refused, "{what}");
            if !row.text.is_empty() {
                assert_eq!(told[0].2, row.text, "{what}");
            }
            if row.refused == [10314] {
                assert!(told[0].2.starts_with("startTime: The date, time, or time-zone entered is invalid."), "{what}: {}", told[0].2);
            }
            let warned: Vec<_> = shared.orders.drain_order_notices().into_iter().map(|(_, code, _)| code).collect();
            assert_eq!(warned, if row.warned { vec![2174] } else { vec![] }, "{what}");
            assert_eq!(wire.contains("35=D|"), row.refused.is_empty(), "{what}: {wire}");
        }
    }

    /// A moment stated without a zone is warned of once for each connection:
    /// the first such order of the session is read after the warning under
    /// 2174, and no later one is warned of again.
    #[test]
    fn a_moment_without_a_zone_is_warned_of_once_for_the_connection() {
        let shared = Arc::new(SharedState::new());
        shared.orders.set_replay_done();
        shared.reference.cache_contract_definition(ContractDefinition {
            con_id: 265598,
            sec_type: SecurityType::Stock,
            exchange: "SMART".into(),
            primary_exchange: "NASDAQ".into(),
            currency: "USD".into(),
            algo_group: "IBALGO".into(),
            order_type_rules: vec![("LMT".into(), 1), ("ALGO".into(), 1)],
            ..Default::default()
        });
        let mut engine = HotLoop::new(shared.clone(), None, None);
        let (connection, mut peer) = Connection::for_test();
        peer.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        engine.ccp_conn = Some(connection);
        engine.set_account_id("DU1".into());
        let (send, receive) = mpsc::channel();
        engine.set_control_rx(receive);
        let mut sent = || {
            let mut wire = String::new();
            let mut bytes = [0; 16384];
            while let Ok(n) = peer.read(&mut bytes) {
                if n == 0 { break; }
                wire.push_str(&String::from_utf8_lossy(&bytes[..n]));
            }
            wire.replace('\x01', "|")
        };
        let place = |engine: &mut HotLoop, order_id: u64, allocator: u64| {
            shared.admit(&send, ControlCommand::Place(Box::new(Placement {
                order_id,
                allocator: Arc::new(AtomicU64::new(allocator)),
                contract: Contract {
                    con_id: 265598, symbol: "AAPL".into(), sec_type: "STK".into(),
                    exchange: "SMART".into(), currency: "USD".into(), ..Default::default()
                },
                order: Order {
                    action: "BUY".into(), order_type: "LMT".into(), lmt_price: 1.0,
                    total_quantity: 1.0, tif: "DAY".into(), algo_strategy: "Vwap".into(),
                    algo_params: vec![crate::types::model::TagValue {
                        tag: "startTime".into(), value: "09:30:00".into(),
                    }],
                    ..Default::default()
                },
                warnings: Vec::new(),
            }))).unwrap();
            engine.poll_once();
        };
        let answer = |engine: &mut HotLoop, frame: String| {
            engine.ccp.process_ccp_message(
                frame.as_bytes(), &mut None, &mut engine.context, &shared, &None, &mut engine.hb, "DU1",
            );
        };
        place(&mut engine, 10, 11);
        let asked = sent();
        assert_eq!(asked.matches("|6040=80|").count(), 1, "the list asked for once: {asked}");
        answer(&mut engine, "35=U\x016040=81\x016597=IBALGO/STK:IBALGO-AE,IBALGO-AL-STK\x01".into());
        engine.poll_once();
        let asked = sent();
        assert!(asked.contains("|6040=53|6364=IBALGO-AE|"), "{asked}");
        answer(&mut engine, format!("35=U\x016040=54\x016364=IBALGO-AE\x016118={ALGORITHM_PROVIDER}\x01"));
        answer(&mut engine, format!("35=U\x016040=54\x016364=IBALGO-AL-STK\x016118={SHARE_ALGORITHMS}\x01"));
        engine.poll_once();
        engine.poll_once();
        let wire = sent();
        assert!(wire.contains("35=D|"), "the first order goes: {wire}");
        let warned: Vec<_> = shared.orders.drain_order_notices().into_iter().map(|(_, code, _)| code).collect();
        assert_eq!(warned, [2174], "the first zone-less moment is warned of");
        place(&mut engine, 12, 13);
        engine.poll_once();
        engine.poll_once();
        let wire = sent();
        assert!(wire.contains("35=D|"), "the second order goes: {wire}");
        let warned: Vec<_> = shared.orders.drain_order_notices().into_iter().map(|(_, code, _)| code).collect();
        assert!(warned.is_empty(), "and no later one of the session is");
        assert!(shared.drain_refused().is_empty(), "neither order is refused");
    }

    /// An order for part of a share or a warrant is refused through the API
    /// on a login that may trade parts of a unit or crypto, as a gateway
    /// refuses it: placed (10243), or replaced where the order working or the
    /// replace states part of a unit (10242). On any other login, and on any
    /// other contract, it goes.
    #[test]
    fn an_order_for_part_of_a_share_is_refused_through_the_api() {
        // What the row is, the contract's type, whether the login takes parts
        // of a unit, the size placed, the size a replace states, and what is
        // refused.
        type Row = (&'static str, &'static str, bool, f64, Option<f64>, &'static [i64]);
        let rows: [Row; 6] = [
            ("part of a share", "STK", true, 1.5, None, &[10243]),
            ("part of a warrant", "WAR", true, 0.5, None, &[10243]),
            ("a whole share", "STK", true, 2.0, None, &[]),
            ("part of a share on a login that takes no parts", "STK", false, 1.5, None, &[]),
            ("a replace for part of a share", "STK", true, 2.0, Some(1.5), &[10242]),
            ("a whole replace", "STK", true, 2.0, Some(3.0), &[]),
        ];
        for (what, sec_type, parts, placed, replaced, refused) in rows {
            let shared = Arc::new(SharedState::new());
            shared.orders.set_replay_done();
            if parts {
                shared.reference.set_money_orders(crate::bridge::MoneyOrderTerms {
                    types: "DAY,GTC,LMT,MKT".into(),
                    account: true,
                    ..Default::default()
                });
            }
            let mut engine = HotLoop::new(shared.clone(), None, None);
            let (connection, mut peer) = Connection::for_test();
            peer.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            engine.ccp_conn = Some(connection);
            engine.set_account_id("DU1".into());
            let (send, receive) = mpsc::channel();
            engine.set_control_rx(receive);
            let mut sent = || {
                let mut wire = String::new();
                let mut bytes = [0; 16384];
                while let Ok(n) = peer.read(&mut bytes) {
                    if n == 0 { break; }
                    wire.push_str(&String::from_utf8_lossy(&bytes[..n]));
                }
                wire.replace('\x01', "|")
            };
            let mut place = |quantity: f64| {
                shared.admit(&send, ControlCommand::Place(Box::new(Placement {
                    order_id: 10,
                    allocator: Arc::new(AtomicU64::new(11)),
                    contract: Contract {
                        con_id: 265598, symbol: "X".into(), sec_type: sec_type.into(),
                        exchange: "SMART".into(), currency: "USD".into(), ..Default::default()
                    },
                    order: Order {
                        action: "BUY".into(), order_type: "LMT".into(), lmt_price: 1.0,
                        total_quantity: quantity, tif: "DAY".into(), ..Default::default()
                    },
                    warnings: Vec::new(),
                }))).unwrap();
                (0..3).for_each(|_| engine.poll_once());
            };
            place(placed);
            let first = sent();
            let told: Vec<_> = shared.drain_refused().into_iter().map(|(_, code, _)| code).collect();
            let Some(replaced) = replaced else {
                assert_eq!(told, refused, "{what}");
                assert_eq!(first.contains("35=D|"), refused.is_empty(), "{what}: {first}");
                continue;
            };
            assert!(told.is_empty() && first.contains("35=D|"), "{what}: working first");
            place(replaced);
            let wire = sent();
            let told: Vec<_> = shared.drain_refused().into_iter().map(|(_, code, _)| code).collect();
            assert_eq!(told, refused, "{what}");
            assert_eq!(wire.contains("35=G|"), refused.is_empty(), "{what}: {wire}");
        }
    }
}
