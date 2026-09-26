//! Preparing and submitting the family described by one place-order request.

use super::attached_checks;
use super::attached_children::{
    ChildContext, ChildDefaults, build_children, is_scale_order, market_orders_regular_only,
};
use super::attached_combos::attached_combo;
use super::attached_prices::{
    IndicativePrices, OrderPrice, PriceContext, PriceSide, cached_quote_views,
    resolve_order_price,
};
use super::{
    ApiContract, ApiOrder, ClientCore, ControlCommand, InstrumentId, OrderKind, OrderRequest,
    Refusal, SharedState,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

#[derive(Default)]
pub(crate) struct AttachedState {
    pub(crate) highest: i64,
    /// Venue number to API number, for each order stated under an API number
    /// other than its venue number. What callbacks report it by.
    aliases: HashMap<u64, i64>,
    /// API number to venue number, for the same orders where they are this
    /// client's: what a caller's API number addresses.
    placed: HashMap<i64, u64>,
    /// The API client this client places under, which reports stating another
    /// client's number do not address.
    client_id: i32,
    pub(crate) family_keys: HashMap<u64, String>,
    pub(crate) sent_api_ids: HashSet<i64>,
}

impl AttachedState {
    pub(crate) fn api_order_id(&self, wire: u64) -> i64 {
        self.aliases.get(&wire).copied().unwrap_or(wire as i64)
    }

    pub(crate) fn learn(&mut self, shared: &SharedState, wire: u64, client_id: i32) {
        let Some(stated) = shared.orders.attached_order_metadata(wire) else { return };
        if let Some(api) = stated.api_order_id {
            self.aliases.insert(wire, api);
            if stated.api_client_id == Some(client_id) {
                self.placed.insert(api, wire);
            }
        }
        if let Some(key) = stated.family_key {
            self.family_keys.insert(wire, key);
        }
    }

    pub(crate) fn place_order_id(
        &mut self,
        shared: &SharedState,
        api: i64,
        next_id: &AtomicU64,
    ) -> Option<u64> {
        if let Some(wire) = shared.orders.wire_order_id(api) {
            return Some(wire);
        }
        loop {
            let wire = shared.orders.number_in_memory(next_id).ok()?;
            if wire != 0 && !self.aliases.contains_key(&wire) {
                self.place(wire, api);
                return Some(wire);
            }
        }
    }

    pub(crate) fn wire_order_id(&self, api: i64) -> Option<u64> {
        self.placed.get(&api).copied().or_else(|| {
            u64::try_from(api).ok().filter(|id| *id != 0 && !self.aliases.contains_key(id))
        })
    }

    /// The first order placed under an API number is the one it addresses
    /// while that order stands.
    pub(crate) fn place(&mut self, wire: u64, api: i64) {
        self.aliases.insert(wire, api);
        self.placed.entry(api).or_insert(wire);
    }

    /// What a new session starts from. The API client orders are placed
    /// under is the caller's, and stays.
    pub(super) fn reset(&mut self) {
        *self = Self { client_id: self.client_id, ..Self::default() };
    }

    pub(crate) fn discard_local_order(&mut self, wire: u64) {
        let api = self.aliases.get(&wire).copied().unwrap_or(wire as i64);
        if self.wire_order_id(api) == Some(wire) {
            self.sent_api_ids.remove(&api);
        }
        if self.placed.get(&api) == Some(&wire) {
            self.placed.remove(&api);
        }
        self.family_keys.remove(&wire);
    }
}

pub(crate) struct PreparedChild {
    pub wire_id: u64,
    pub order: ApiOrder,
    pub command: ControlCommand,
}

pub(crate) struct PreparedAttached {
    pub children: Vec<PreparedChild>,
    pub parent_family_key: String,
}

pub(crate) fn contract_definition(
    shared: &SharedState,
    contract: &ApiContract,
) -> Option<crate::control::contracts::ContractDefinition> {
    if matches!(contract.sec_type.as_str(), "BAG" | "COMB")
        && let Some(definition) =
            attached_combo(shared, contract).and_then(|combo| combo.definition)
    {
        return Some(definition);
    }
    shared.reference.contract_definition(contract.con_id as u32, &contract.exchange).or_else(|| {
        matches!(contract.sec_type.as_str(), "BAG" | "COMB")
            .then(|| {
                shared.reference.combo_definition(
                    &contract.symbol,
                    &contract.exchange,
                    &contract.currency,
                )
            })
            .flatten()
    })
}

impl ClientCore {
    pub(crate) fn attached_creation_quantity(
        shared: &SharedState,
        contract: &ApiContract,
        order: &ApiOrder,
        new_order: bool,
    ) -> f64 {
        let quantity = order.total_quantity;
        let allocated_quantity = shared.reference.advisor()
            && !order.fa_group.trim_matches(|c: char| c <= '\u{20}').is_empty()
            && matches!(order.fa_method.as_str(), "PctChange" | "PctRoll")
            && (!order.fa_percentage.is_empty() ^ (quantity.trunc() as i64 as i32 != 0));
        if allocated_quantity
            && new_order
            && order.model_code.is_empty()
            && order.fa_percentage.is_empty()
        {
            return f64::from(quantity.trunc() as i64 as i32);
        }
        if allocated_quantity
            || !new_order
            || contract.exchange == "AVGCOST"
            || !matches!(contract.sec_type.as_str(), "BAG" | "COMB")
        {
            return quantity;
        }
        attached_combo(shared, contract)
            .and_then(|combo| combo.legs)
            .map_or(quantity, |(_, factor)| quantity * factor.max(1.0))
    }

    /// What the venue states about an order's API identity and family. Any
    /// report names the API number callbacks give the order; only one stated
    /// under this client's own API client can be addressed by that number.
    pub(crate) fn learn_order_identity(&self, shared: &SharedState, wire: u64) {
        let Some(stated) = shared.orders.attached_order_metadata(wire) else { return };
        let mut state = self.attached_orders.lock().unwrap();
        if let Some(api) = stated.api_order_id.filter(|api| *api != wire as i64) {
            state.aliases.insert(wire, api);
            if stated.api_client_id == Some(state.client_id)
                && shared.orders.wire_order_id(api) == Some(wire)
            {
                state.placed.insert(api, wire);
            }
        }
        if let Some(key) = stated.family_key {
            state.family_keys.insert(wire, key);
        }
    }

    /// The API client this client's orders are placed under, the one either
    /// surface names at connect.
    pub(crate) fn set_api_client_id(&self, client_id: i32) {
        self.attached_orders.lock().unwrap().client_id = client_id;
    }

    pub(crate) fn api_order_id(&self, wire: u64) -> i64 {
        self.attached_orders.lock().unwrap().aliases.get(&wire).copied().unwrap_or(wire as i64)
    }

    pub(crate) fn wire_order_id(&self, api: i64) -> Option<u64> {
        self.attached_orders.lock().unwrap().wire_order_id(api)
    }

    pub(crate) fn wire_parent_id(&self, api: i64) -> i64 {
        if api > 0 { self.wire_order_id(api).map_or(api, |wire| wire as i64) } else { api }
    }
}

impl AttachedState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build(
        &mut self,
        shared: &SharedState,
        preset: &crate::control::attached_presets::AttachedPreset,
        parent: u64,
        contract: &ApiContract,
        order: &ApiOrder,
        instrument: InstrumentId,
        next_id: &AtomicU64,
        combo_regular_hours: bool,
        parent_is_scale_child: bool,
    ) -> Result<Option<PreparedAttached>, Refusal> {
        if !attached_checks::requested(order) {
            return Ok(None);
        }
        self.learn(shared, parent, order.client_id);
        let definition = contract_definition(shared, contract).ok_or_else(|| {
            Refusal::no_answer("The contract definition needed for attached orders is unavailable")
        })?;
        attached_checks::check_presets(
            order,
            preset.auto_attach_profit_taker,
            preset.auto_attach_stop_loss,
        )?;
        let rule = if matches!(contract.sec_type.as_str(), "BAG" | "COMB") {
            attached_combo(shared, contract).and_then(|combo| combo.price_rule)
        } else {
            None
        }
        .or_else(|| {
            definition.market_rule_id.and_then(|id| shared.reference.market_rule(id as i32))
        });
        let indication = super::attached_quote_contract::cached(shared, contract);
        let proxy = indication.as_ref().filter(|proxy| {
            (proxy.con_id, &proxy.exchange, &proxy.sec_type)
                != (contract.con_id, &contract.exchange, &contract.sec_type)
        });
        let proxy_definition = proxy.and_then(|proxy| contract_definition(shared, proxy));
        let proxy_rule = proxy_definition
            .as_ref()
            .and_then(|definition| definition.market_rule_id)
            .and_then(|id| shared.reference.market_rule(id as i32));
        let proxy_prices = proxy.map(|proxy| {
            let instrument = shared.market.attached_quote_instrument(proxy.con_id, &proxy.exchange);
            price_context(shared, proxy, proxy_definition.as_ref(), instrument, proxy_rule.as_ref())
        });
        let mut parent_price = order_price(order, contract);
        let mut prices = PriceContext {
            side: parent_price.side,
            pricing_order: parent_price,
            loan_fee_sides: (contract.sec_type == "SLB")
                .then(|| shared.market.pricing_loan_sides(instrument)),
            indicative: match &indication {
                Some(_) => proxy_prices
                    .as_ref()
                    .map_or(IndicativePrices::Original, IndicativePrices::Available),
                None => IndicativePrices::Unavailable,
            },
            ..price_context(shared, contract, Some(&definition), Some(instrument), rule.as_ref())
        };
        // A gateway reads no limit off an order of a type that takes none. It
        // prices one as it creates it, from the preset's primary limit, and a
        // market parent's children are priced from that.
        if !parent_price.uses_limit {
            parent_price.limit =
                resolve_order_price(&preset.primary_limit, preset.primary_reverse_bid_ask, &prices);
            prices.pricing_order = parent_price;
        }
        let underlying = shared.reference.contract_definition(definition.under_con_id, "");
        let numeric_parent = parent.to_string();
        let parent_execution = shared.orders.get_order_info(parent);
        let context = ChildContext {
            exchange: &definition.exchange,
            parent_api_id: self.api_order_id(parent),
            parent_price,
            prices,
            oca_group: &numeric_parent,
            parent_is_scale_or_scale_child: is_scale_order(order) || parent_is_scale_child,
            parent_finished: parent_execution.as_ref().is_some_and(|info| {
                matches!(info.order_state.status.as_str(), "Filled" | "Cancelled")
            }),
            parent_trade_price: parent_execution.as_ref().map(|info| {
                if info.last_exec.avg_price == 0.0 {
                    info.last_exec.price
                } else {
                    info.last_exec.avg_price
                }
            }),
            supports_order_type: &|kind| {
                shared.reference.supports_attached_order_type(&definition, kind)
            },
            defaults: ChildDefaults {
                percent_prices_restricted: !definition.ev_rule.is_empty()
                    && !matches!(definition.ev_rule.split(':').next(),
                        Some(kind) if kind.eq_ignore_ascii_case("factor") || kind.eq_ignore_ascii_case("etmf")),
                regular_hours: shared.reference.supports_attached_order_type(&definition, "RTH"),
                late_hours: shared.reference.supports_attached_order_type(&definition, "LTH"),
                market_orders_regular_only: shared
                    .reference
                    .supports_attached_order_type(&definition, "RTH4MKT")
                    || market_orders_regular_only(
                        &contract.sec_type,
                        &contract.currency,
                        &definition.market_classification,
                        &definition.under_sec_type,
                        underlying
                            .as_ref()
                            .map_or("", |under| under.market_classification.as_str()),
                    ),
                is_combo: matches!(contract.sec_type.as_str(), "BAG" | "COMB"),
                combo_regular_hours,
                cancel_parent_enabled: shared.reference.enables("CHILDCANCELPARENT"),
                opening_at_arca: contract.exchange == "ARCA",
            },
        };
        let children = build_children(order, preset, &context);
        let held_key = self.family_keys.get(&parent).cloned();
        let key = if children.is_empty() {
            String::new()
        } else {
            held_key.clone().unwrap_or_else(new_family_key)
        };
        // A new family starts its members at one; one the parent already heads
        // continues after the members it has.
        let last_family_index = held_key
            .map(|held| {
                let state = &self;
                state
                    .family_keys
                    .values()
                    .filter_map(|member| family_index_in(member, &held))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let floor = children
            .iter()
            .map(|child| child.order.order_id)
            .filter(|id| *id > 0)
            .max()
            .unwrap_or(parent as i64)
            .max(parent as i64) as u64
            + 1;
        next_id.fetch_max(floor, Ordering::AcqRel);
        let trail_as_t = shared.reference.enables("TRAILSENDT");
        let mut prepared = Vec::with_capacity(children.len());
        for (index, child) in children.into_iter().enumerate() {
            let api_id = child.order.order_id;
            let wire = if api_id > 0 {
                self.place_order_id(shared, api_id, next_id)
                    .ok_or_else(|| Refusal::validation("No venue order identifier is available"))?
            } else {
                shared.orders.number_in_memory(next_id)?
            };
            let kind = OrderKind::Attached {
                ord_type: child.prices.wire_order_type(trail_as_t).into(),
                exec_inst: child.prices.exec_inst(trail_as_t).into(),
                prices: child.prices.wire_fields(),
            };
            let mut command = ClientCore::build_order_request_with_kind(
                &child.order,
                wire,
                instrument,
                Some(contract),
                Some(kind),
            )?;
            if let ControlCommand::Order(OrderRequest::SubmitEx { attrs, tif, .. }) = &mut command {
                attrs.parent_id = parent;
                let attached = attrs.attached_mut();
                attached.api_identity = Some((api_id, order.client_id));
                attached.family_key = child_family_key(&key, last_family_index + index + 1);
                attached.use_parent_price = child.use_parent_trade_price;
                if child.profit_offset.is_some() {
                    attached.profit_offset = child.profit_offset;
                }
                if let Some(value) = child.tif_override {
                    *tif = value;
                }
            }
            prepared.push(PreparedChild { wire_id: wire, order: child.order, command });
        }
        Ok(Some(PreparedAttached { children: prepared, parent_family_key: key }))
    }
}

pub(crate) fn confirmed_legs(
    shared: &SharedState,
    contract: &ApiContract,
) -> Option<Vec<crate::types::ComboLegSpec>> {
    let (legs, _) = attached_combo(shared, contract)?.legs?;
    Some(legs.iter().map(super::leg_spec).collect())
}

static FAMILY_GROUP: AtomicI32 = AtomicI32::new(0);

/// A family key the venue states, read whole, moves the group numbering past
/// its group: a family numbered afterwards is not taken for a recovered one.
pub(crate) fn note_family_key(key: &str) {
    let mut parts = key.split('/').filter(|part| !part.is_empty());
    let group = parts.next().and_then(|part| part.parse::<i32>().ok());
    let whole = parts.next().is_some_and(|part| part.parse::<i32>().is_ok())
        && parts.next().is_some_and(|part| part.parse::<i32>().is_ok());
    if let Some(group) = group.filter(|_| whole) {
        FAMILY_GROUP.fetch_max(group, Ordering::Relaxed);
    }
}

fn new_family_key() -> String {
    let group = FAMILY_GROUP.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    let rgb = 0xff00_0000_u32
        | (rand::random_range(100..255_u32) << 16)
        | (rand::random_range(100..255_u32) << 8)
        | rand::random_range(100..255_u32);
    format!("{group}/0/{}", rgb as i32)
}

fn family_index_in(key: &str, family: &str) -> Option<usize> {
    let mut key = key.split('/');
    let mut family = family.split('/');
    let same_group = key.next()? == family.next()?;
    let index = key.next()?.parse().ok()?;
    let _ = family.next()?;
    (same_group && key.next()? == family.next()?).then_some(index)
}

fn child_family_key(parent: &str, index: usize) -> String {
    let mut parts = parent.split('/');
    let group = parts.next().unwrap_or_default();
    let _ = parts.next();
    format!("{group}/{index}/{}", parts.next().unwrap_or_default())
}

/// What prices on one contract are resolved from: its rule, the quotes held
/// for its slot and the portfolio's mark for it.
fn price_context<'a>(
    shared: &SharedState,
    contract: &ApiContract,
    definition: Option<&crate::control::contracts::ContractDefinition>,
    instrument: Option<InstrumentId>,
    rule: Option<&'a crate::control::contracts::MarketRule>,
) -> PriceContext<'a> {
    let settles = definition.is_some_and(|definition| definition.stock_type == "ETMF");
    PriceContext {
        rule,
        quotes: instrument
            .map(|instrument| {
                cached_quote_views(
                    &shared.market,
                    instrument,
                    rule.is_some_and(|rule| rule.negative_prices),
                    contract.sec_type == "IND",
                    matches!(contract.sec_type.as_str(), "BAG" | "COMB"),
                    contract.sec_type == "FUT"
                        && !shared.reference.enables("NO_NEGATIVE_CLOSE_FOR_FUT"),
                    contract.sec_type == "SLB"
                        && definition.is_some_and(|definition| {
                            definition.order_types.iter().any(|kind| kind == "IBLENDER")
                        }),
                )
            })
            .unwrap_or_default(),
        trades_at_settlement: settles,
        allow_portfolio_fallback: !settles && contract.exchange != "CFETAS",
        portfolio_price: shared.portfolio.position_info(contract.con_id).and_then(|position| {
            position
                .market_price_stated
                .then_some(position.market_price as f64 / crate::types::PRICE_SCALE as f64)
        }),
        ..PriceContext::default()
    }
}

/// A parent's prices as a gateway reads them off the placement. The limit of
/// a type that takes none is not read, and is priced once the prices are.
fn order_price(order: &ApiOrder, contract: &ApiContract) -> OrderPrice {
    let kind = order.order_type_named().unwrap_or_default();
    let uses_limit = matches!(
        kind,
        "LMT"
            | "STP LMT"
            | "TRAIL LIMIT"
            | "LIT"
            | "LOC"
            | "REL"
            | "PASSV REL"
            | "PEG MKT"
            | "PEG MID"
            | "PEG BEST"
            | "MIDPRICE"
    );
    OrderPrice {
        side: if order.side() == Ok(crate::types::Side::Buy) {
            PriceSide::Buy
        } else {
            PriceSide::Sell
        },
        limit: if uses_limit {
            ClientCore::stated_limit(order, Some(contract))
        } else {
            super::attached_prices::UNSET_PRICE
        },
        stop: if matches!(kind, "TRAIL" | "TRAIL LIMIT") {
            order.trail_stop_price
        } else {
            order.aux_price
        },
        touched_trigger: order.aux_price,
        uses_limit,
        uses_stop: matches!(kind, "STP" | "STP LMT" | "TRAIL LIMIT" | "STP PRT"),
        is_market: kind == "MKT",
        is_touched: matches!(kind, "MIT" | "LIT"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::contracts::{ContractDefinition, MarketRule, PriceIncrement};
    use crate::control::order_presets::PresetValues;

    fn prepared() -> (AttachedState, SharedState, ApiContract, ApiOrder) {
        let state = AttachedState::default();
        let shared = SharedState::new();
        let contract = ApiContract {
            con_id: 17,
            symbol: "ABC".into(),
            sec_type: "STK".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            ..ApiContract::default()
        };
        shared.reference.cache_contract_definition(ContractDefinition {
            con_id: 17,
            exchange: "SMART".into(),
            market_rule_id: Some(5),
            order_type_key: "STK".into(),
            order_type_rules: vec![("STP".into(), 1), ("LMT".into(), 1), ("OCA".into(), 1)],
            order_types: vec!["STP".into(), "LMT".into(), "OCA".into()],
            ..ContractDefinition::default()
        });
        shared.reference.push_market_rules(vec![MarketRule {
            rule_id: 5,
            negative_prices: false,
            price_magnifier: 0,
            price_increments: vec![PriceIncrement { low_edge: 0.0, increment: 0.01 }],
            size_increments: vec![],
            price_places: None,
        }]);
        shared.reference.set_order_presets(vec![("s=STK".into(), "a=1".into(), "1".into())]);
        let (request_key, _answer) = shared.reference.expect_order_preset_values("s=STK");
        shared.reference.set_order_preset_values(PresetValues {
            request_key,
            key: "s=STK".into(),
            attributes: "a=1".into(),

            error: None,
            fields: vec![
                (4074, "1".into()),
                (4075, "1".into()),
                (4076, "7".into()),
                (4083, "2".into()),
            ],
        });
        let order = ApiOrder {
            order_id: 10,
            client_id: 23,
            action: "BUY".into(),
            total_quantity: 100.0,
            order_type: "LMT".into(),
            lmt_price: 100.0,
            sl_order_id: 11,
            sl_order_type: "PRESET".into(),
            pt_order_id: 12,
            pt_order_type: "PRESET".into(),
            ..ApiOrder::default()
        };
        (state, shared, contract, order)
    }

    #[test]
    fn a_combination_without_a_conid_keeps_its_named_contract_rules() {
        let shared = SharedState::new();
        let contract = ApiContract {
            symbol: "ABC".into(),
            sec_type: "BAG".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            ..ApiContract::default()
        };
        shared.reference.cache_contract_definition(ContractDefinition {
            symbol: "ABC".into(),
            sec_type: crate::control::contracts::SecurityType::Combo,
            exchange: "SMART".into(),
            currency: "USD".into(),
            market_rule_id: Some(17),
            order_type_key: "COMB".into(),
            order_type_rules: vec![("LMT".into(), 1)],
            ..ContractDefinition::default()
        });
        let definition = contract_definition(&shared, &contract).unwrap();
        assert_eq!(definition.market_rule_id, Some(17));
        assert!(shared.reference.supports_attached_order_type(&definition, "LMT"));
        assert!(!shared.reference.supports_attached_order_type(&definition, "STP"));
        assert!(
            contract_definition(&shared, &ApiContract { currency: "EUR".into(), ..contract })
                .is_none()
        );
    }

    #[test]
    fn initial_children_use_a_cached_indication_and_its_market_rule() {
        let (mut state, shared, mut contract, order) = prepared();
        contract.sec_type = "CFD".into();
        let mut definition = shared.reference.contract_definition(17, "SMART").unwrap();
        definition.sec_type = crate::control::contracts::SecurityType::Cfd;
        definition.under_con_id = 18;
        definition.under_sec_type = "STK".into();
        definition.order_type_rules.push(("USESTKMD".into(), 0));
        shared.reference.cache_contract_definition(definition);
        shared.reference.set_enabled_features(vec!["USESTKMD".into()]);
        shared.reference.set_order_presets(vec![("s=CFD".into(), "a=1".into(), "1".into())]);
        let (request_key, _) = shared.reference.expect_order_preset_values("s=CFD");
        shared.reference.set_order_preset_values(PresetValues {
            request_key,
            key: "s=CFD".into(),
            attributes: "a=1".into(),

            error: None,
            fields: vec![
                (4074, "1".into()),
                (4075, "1".into()),
                (4076, "7".into()),
                (4083, "2".into()),
                (4084, "4077".into()),
                (4085, "0".into()),
                (4088, "2".into()),
                (4084, "4078".into()),
                (4085, "0".into()),
                (4088, "2".into()),
            ],
        });
        let mut prepare = || {
            let preset = crate::control::attached_presets::AttachedPreset::read(
                &shared.reference.current_order_preset_values("s=CFD").unwrap(),
            )
            .unwrap();
            state
                .build(&shared, &preset, 10, &contract, &order, 0, &AtomicU64::new(1), false, false)
                .unwrap()
                .unwrap()
        };
        let unresolved = prepare();
        assert_eq!(unresolved.children[0].order.aux_price, f64::MAX);
        assert_eq!(unresolved.children[1].order.lmt_price, f64::MAX);
        let proxy = ApiContract { con_id: 18, ..contract.clone() };
        let proxy = ApiContract { sec_type: "STK".into(), ..proxy };
        shared.reference.cache_contract_definition(ContractDefinition {
            con_id: 18,
            exchange: "SMART".into(),
            market_rule_id: Some(6),
            ..Default::default()
        });
        shared.reference.push_market_rules(vec![MarketRule {
            rule_id: 6,
            negative_prices: false,
            price_magnifier: 1,
            price_increments: vec![PriceIncrement { low_edge: 0.0, increment: 0.25 }],
            size_increments: vec![],
            price_places: None,
        }]);
        shared.reference.cache_attached_quote_contract(
            super::super::attached_quote_contract::key(&contract),
            super::super::attached_quote_contract::CachedSelection::Proxy(proxy),
        );
        shared.market.note_attached_quote_instrument(18, "SMART", 1);
        shared.market.push_pricing_quote(
            1,
            0,
            &crate::types::Quote {
                last: crate::types::price_from_f64(50.12),
                ..Default::default()
            },
            crate::bridge::PRICING_LAST,
            0,
        );
        let resolved = prepare();
        assert_eq!(resolved.children[0].order.aux_price, 50.0);
        assert_eq!(resolved.children[1].order.lmt_price, 50.25);
    }

    #[test]
    fn combination_creation_quantity_uses_the_normalized_ratio_factor() {
        let (_, shared, mut contract, _) = prepared();
        contract.sec_type = "BAG".into();
        let cache = |contract: &ApiContract, factor| {
            shared.reference.update_attached_combo(&format!("{contract:?}"), |combo| {
                combo.legs = Some((vec![], factor))
            });
        };
        cache(&contract, 2.0);
        let order = ApiOrder { total_quantity: 1.25, ..Default::default() };
        assert_eq!(ClientCore::attached_creation_quantity(&shared, &contract, &order, true), 2.5);
        assert_eq!(ClientCore::attached_creation_quantity(&shared, &contract, &order, false), 1.25);
        cache(&contract, 0.5);
        assert_eq!(ClientCore::attached_creation_quantity(&shared, &contract, &order, true), 1.25);
        contract.exchange = "AVGCOST".into();
        cache(&contract, 2.0);
        assert_eq!(ClientCore::attached_creation_quantity(&shared, &contract, &order, true), 1.25);
    }

    #[test]
    fn an_explicit_allocation_total_is_integer_and_not_scaled() {
        let (_, shared, mut contract, _) = prepared();
        contract.sec_type = "BAG".into();
        shared.reference.update_attached_combo(&format!("{contract:?}"), |combo| {
            combo.legs = Some((vec![], 2.0))
        });
        shared.reference.set_advisor(true);
        for method in ["PctChange", "PctRoll"] {
            for (quantity, percentage, expected) in
                [(1.25, "", 1.0), (1.25, "10", 2.5), (0.25, "", 0.5), (0.0, "", 0.0)]
            {
                let order = ApiOrder {
                    total_quantity: quantity,
                    fa_group: " Group ".into(),
                    fa_method: method.into(),
                    fa_percentage: percentage.into(),
                    ..Default::default()
                };
                assert_eq!(
                    ClientCore::attached_creation_quantity(&shared, &contract, &order, true),
                    expected
                );
                assert_eq!(
                    ClientCore::attached_creation_quantity(&shared, &contract, &order, false),
                    quantity,
                );
            }
        }
        let mut order = ApiOrder {
            total_quantity: 1.25,
            fa_group: " ".into(),
            fa_method: "PctChange".into(),
            ..Default::default()
        };
        assert_eq!(ClientCore::attached_creation_quantity(&shared, &contract, &order, true), 2.5);
        order.fa_group = "Group".into();
        order.fa_method = "EqualQuantity".into();
        assert_eq!(ClientCore::attached_creation_quantity(&shared, &contract, &order, true), 2.5);
        order.fa_method = "PctChange".into();
        shared.reference.set_advisor(false);
        assert_eq!(ClientCore::attached_creation_quantity(&shared, &contract, &order, true), 2.5);
    }

    /// Children relative to their parent's price take the price a gateway
    /// reads off the parent: its own limit where its type takes one, nought
    /// being none but on a combination that is not a relative order, and
    /// where its type takes none, as a market order's does not, the preset's
    /// primary limit on the parent's side of the quote, the other side for a
    /// sale unless the preset says otherwise. A trailing parent has no fixed
    /// price, and a parent with no price leaves its children unpriced.
    #[test]
    fn children_are_priced_from_the_price_a_gateway_reads_off_the_parent() {
        let unset = f64::MAX;
        let quoted = crate::types::Quote {
            bid: crate::types::price_from_f64(99.0),
            ask: crate::types::price_from_f64(100.5),
            bid_size: crate::types::QTY_SCALE,
            ask_size: crate::types::QTY_SCALE,
            ..Default::default()
        };
        // The parent's type, what else it and its contract state, what its
        // preset states beside the defaults, whether there is a quote, and
        // the children's stop and limit.
        type Row =
            (&'static str, fn(&mut ApiOrder, &mut ApiContract), &'static [(u32, &'static str)], bool, (f64, f64));
        // The bid, a quarter above it, and a sale left on that side.
        let bid_for_both = &[(4050, "0"), (4084, "4058"), (4085, "0.25"), (4088, "0")];
        let rows: [Row; 9] = [
            ("TRAIL", |o, _| { o.trail_stop_price = 98.0; o.aux_price = 2.0 }, &[], true, (unset, unset)),
            ("MKT", |_, _| {}, &[], true, (99.5, 101.5)),
            ("MKT", |o, _| o.action = "SELL".into(), &[], true, (100.0, 98.0)),
            ("MKT", |o, _| o.action = "SELL".into(), bid_for_both, true, (100.25, 98.25)),
            ("MKT", |_, _| {}, &[], false, (unset, unset)),
            ("MKT", |o, _| o.lmt_price = 50.0, &[], false, (unset, unset)),
            ("LMT", |o, _| o.lmt_price = 0.0, &[], true, (unset, unset)),
            ("REL", |o, c| { o.lmt_price = 0.0; c.sec_type = "BAG".into() }, &[], false, (unset, unset)),
            ("LMT", |_, _| {}, &[], true, (99.0, 101.0)),
        ];
        for (order_type, set, stated, quotes, (stop, limit)) in rows {
            let (mut state, shared, mut contract, mut order) = prepared();
            order.order_type = order_type.into();
            set(&mut order, &mut contract);
            if quotes {
                use crate::bridge::{PRICING_ASK, PRICING_ASK_SIZE, PRICING_BID, PRICING_BID_SIZE};
                shared.market.push_pricing_quote(
                    0, 0, &quoted, PRICING_BID | PRICING_ASK | PRICING_BID_SIZE | PRICING_ASK_SIZE, 0,
                );
            }
            let mut values = shared.reference.current_order_preset_values("s=STK").unwrap();
            values.fields.extend(stated.iter().map(|(tag, value)| (*tag, value.to_string())));
            let preset = crate::control::attached_presets::AttachedPreset::read(&values).unwrap();
            let family = state
                .build(&shared, &preset, 10, &contract, &order, 0, &AtomicU64::new(1), false, false)
                .unwrap()
                .unwrap();
            let priced = (family.children[0].order.aux_price, family.children[1].order.lmt_price);
            assert_eq!(priced, (stop, limit), "{order_type} {} {:?}", order.action, order.lmt_price);
        }
    }

    #[test]
    fn parent_trade_offsets_use_completed_fills_and_fall_back_to_the_last_price() {
        for (status, avg, expected) in [
            ("Submitted", 95.0, [1.0, 1.0]),
            ("Filled", 95.0, [4.0, 6.0]),
            ("Cancelled", 0.0, [3.0, 5.0]),
        ] {
            let (mut state, shared, contract, order) = prepared();
            let (request_key, _) = shared.reference.expect_order_preset_values("s=STK");
            shared.reference.set_order_preset_values(PresetValues {
                request_key,
                key: "s=STK".into(),
                attributes: "a=1".into(),

                error: None,
                fields: vec![
                    (4074, "1".into()),
                    (4075, "1".into()),
                    (4076, "7".into()),
                    (4083, "2".into()),
                    (4084, "4077".into()),
                    (4087, "1".into()),
                    (4084, "4078".into()),
                    (4087, "1".into()),
                ],
            });
            shared.orders.push_order_info(
                10,
                crate::bridge::RichOrderInfo {
                    contract: contract.clone(),
                    order: ApiOrder { filled_quantity: 10.0, ..order.clone() },
                    order_state: crate::types::model::OrderState {
                        status: status.into(),
                        ..Default::default()
                    },
                    last_exec: crate::types::model::Execution {
                        avg_price: avg,
                        price: 96.0,
                        ..Default::default()
                    },
                },
            );
            let family = {
                let preset = crate::control::attached_presets::AttachedPreset::read(
                    &shared.reference.current_order_preset_values("s=STK").unwrap(),
                )
                .unwrap();
                state.build(
                    &shared,
                    &preset,
                    10,
                    &contract,
                    &order,
                    0,
                    &AtomicU64::new(1),
                    false,
                    false,
                )
            }
            .unwrap()
            .unwrap();
            for (child, offset) in family.children.iter().zip(expected) {
                let ControlCommand::Order(OrderRequest::SubmitEx { attrs, .. }) = &child.command
                else {
                    panic!()
                };
                assert_eq!(
                    attrs.attached.as_ref().and_then(|held| held.profit_offset),
                    Some(offset),
                    "{status}"
                );
            }
        }
    }

    #[test]
    fn the_contract_rule_controls_percentage_trailing_children() {
        for (rule, count) in [("", 2), ("factor:100", 2), ("ETMF", 2), ("aussieBond", 1)] {
            let (mut state, shared, contract, order) = prepared();
            let mut definition = shared.reference.contract_definition(17, "SMART").unwrap();
            definition.ev_rule = rule.into();
            definition.order_type_rules.push(("TRAIL".into(), 1));
            definition.order_types.push("TRAIL".into());
            shared.reference.cache_contract_definition(definition);
            let (request_key, _) = shared.reference.expect_order_preset_values("s=STK");
            shared.reference.set_order_preset_values(PresetValues {
                request_key,
                key: "s=STK".into(),
                attributes: "a=1".into(),

                error: None,
                fields: vec![
                    (4074, "1".into()),
                    (4075, "1".into()),
                    (4076, "11".into()),
                    (4083, "2".into()),
                    (4070, "2".into()),
                    (4071, "100".into()),
                ],
            });
            let family = {
                let preset = crate::control::attached_presets::AttachedPreset::read(
                    &shared.reference.current_order_preset_values("s=STK").unwrap(),
                )
                .unwrap();
                state.build(
                    &shared,
                    &preset,
                    10,
                    &contract,
                    &order,
                    0,
                    &AtomicU64::new(1),
                    false,
                    false,
                )
            }
            .unwrap()
            .unwrap();
            assert_eq!(family.children.len(), count, "{rule}");
        }
    }

    #[test]
    fn a_family_key_read_from_the_venue_moves_the_numbering_past_it() {
        let group = |key: &str| key.split('/').next().unwrap().parse::<i32>().unwrap();
        for incomplete in ["1000000/1", "1000000", "x/1/2", "1000000/1/x"] {
            note_family_key(incomplete);
            assert!(group(&new_family_key()) < 1_000_000, "{incomplete}");
        }
        note_family_key("1000000/2/-9934747");
        assert!(group(&new_family_key()) > 1_000_000);
    }
}
