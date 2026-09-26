use super::attached_loading::Request;
use super::{OrderSession, holds_several_accounts};
use crate::bridge::SharedState;
use crate::control::contracts::{ContractDefinition, SecurityType};
use crate::error_codes::Refusal;
use crate::types::model::Contract;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// The key what attaching orders to a combination settles is kept under: the
/// combination as the caller stated it, legs and hedge included, since each
/// reaches the confirmed legs and terms kept for it.
pub(crate) fn combo_key(contract: &Contract) -> String {
    format!("{contract:?}")
}

/// What attaching orders to this combination has settled so far.
pub(crate) fn attached_combo(
    shared: &SharedState,
    contract: &Contract,
) -> Option<crate::bridge::AttachedCombo> {
    shared.reference.attached_combo(&combo_key(contract))
}

pub(crate) fn registration_identity(contract: &Contract) -> String {
    let identity = crate::types::model::contract_identity(
        &contract.last_trade_date_or_contract_month,
        contract.strike,
        &contract.right,
        &contract.multiplier,
        &contract.currency,
    );
    if !matches!(contract.sec_type.as_str(), "BAG" | "COMB" | "COMBO") {
        return identity;
    }
    let legs: std::collections::BTreeMap<_, _> = contract
        .combo_legs
        .iter()
        .map(|leg| (leg.con_id, (leg.ratio, leg.action == "BUY", &leg.exchange)))
        .collect();
    let prefix = if identity.is_empty() { "||||||" } else { &identity };
    format!("{prefix}|{legs:?}|{:?}", contract.delta_neutral_contract)
}

fn allows_rth(entries: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    entries
        .into_iter()
        .find_map(|entry| {
            let entry = entry.as_ref();
            let (kind, simulation) = entry.split_once('/').unwrap_or((entry, "0"));
            (kind == "RTH").then(|| simulation.parse::<i32>().unwrap_or(0) != 4)
        })
        .unwrap_or(false)
}

fn definition_allows_rth(definition: &ContractDefinition) -> bool {
    definition
        .order_type_rules
        .iter()
        .find(|(kind, _)| kind == "RTH")
        .is_some_and(|(_, simulation)| *simulation != 4)
}

fn eligible_smart_legs(legs: &[ContractDefinition], session: &OrderSession) -> bool {
    !session.enables("REJGNONAT")
        && !(session.account.starts_with('T')
            && !holds_several_accounts(&session.logon_accounts, &session.features))
        && legs.iter().all(|leg| {
            leg.currency == "USD"
                && matches!(leg.sec_type, SecurityType::Stock | SecurityType::Option)
        })
        && !(legs.len() > 1 && legs.iter().all(|leg| leg.sec_type == SecurityType::Stock))
}

fn common_exchanges(legs: &[ContractDefinition], exclusions: &str) -> Vec<String> {
    let excluded = |exchange: &str, sec_type: &str| {
        exclusions
            .split(',')
            .filter_map(|entry| {
                let mut parts = entry.split('/');
                let pair = (parts.next()?, parts.next()?);
                parts.next().is_none().then_some(pair)
            })
            .any(|(named, kind)| named == exchange && (kind == sec_type || kind == "*"))
    };
    let Some(first) = legs.first() else {
        return Vec::new();
    };
    first
        .valid_exchanges
        .iter()
        .filter(|exchange| {
            exchange.as_str() != "SMART"
                && legs.iter().all(|leg| {
                    leg.valid_exchanges.contains(exchange)
                        && !excluded(exchange, leg.sec_type.to_api_str())
                })
        })
        .fold(Vec::new(), |mut exchanges, exchange| {
            if !exchanges.contains(exchange) {
                exchanges.push(exchange.clone());
            }
            exchanges
        })
}

/// Resolve the session restriction before constructing a combination's children.
pub(crate) async fn resolve(
    shared: &SharedState,
    contract: &Contract,
    session: &OrderSession,
    control: &Sender<Request>,
    deadline: Instant,
    mut lookup: impl FnMut(
        &Contract,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), Refusal>> + Send>,
    >,
) -> Result<bool, Refusal> {
    if !matches!(contract.sec_type.as_str(), "BAG" | "COMB" | "COMBO") {
        return Ok(false);
    }
    let permissions = shared.reference.permitted_order_types("COMB").unwrap_or_default();
    let key = combo_key(contract);
    if let Some(answer) =
        shared.reference.attached_combo(&key).and_then(|combo| combo.regular_hours)
    {
        return Ok(answer);
    }
    let mut legs = Vec::with_capacity(contract.combo_legs.len());
    for leg in &contract.combo_legs {
        let Some(definition) =
            definition_or_lookup(shared, leg.con_id, &leg.exchange, &mut lookup).await?
        else {
            return Ok(false);
        };
        request_rules(shared, control, &definition.exchange)?;
        legs.push(definition);
    }
    let mut combination = contract.clone();
    if let Some(neutral) = &contract.delta_neutral_contract
        && !combination.combo_legs.iter().any(|leg| leg.con_id == neutral.con_id)
        && let Some(underlying) =
            definition_or_lookup(shared, neutral.con_id, "", &mut lookup).await?
    {
        add_neutral_leg(&mut combination, &underlying);
        request_rules(shared, control, &underlying.exchange)?;
        legs.push(underlying);
    }
    let ratio_factor = simplify_legs(&mut combination);
    let definition = if contract.exchange == "SMART" {
        let con_id = shared.reference.smart_combo_conid(&contract.currency).unwrap_or(0);
        definition_or_lookup(shared, i64::from(con_id), "SMART", &mut lookup).await?
    } else {
        // Named directly, not through the attached family's cache this call is
        // about to fill, and for a combination under any of its names.
        shared.reference.contract_definition(contract.con_id as u32, &contract.exchange).or_else(
            || {
                shared.reference.combo_definition(
                    &contract.symbol,
                    &contract.exchange,
                    &contract.currency,
                )
            },
        )
    };
    let definition = definition
        .ok_or_else(|| Refusal::no_answer("The combination definition is unavailable"))?;
    shared
        .reference
        .update_attached_combo(&key, |combo| combo.definition = Some(definition.clone()));
    let synthetic = eligible_smart_legs(&legs, session);
    let kind =
        crate::control::attached_combos::combo_type(legs.iter().map(|leg| leg.sec_type.clone()));
    let needs_multiplier = combination_needs_multiplier(shared, &combination, &legs);
    let include_exchanges = combination.combo_legs.iter().zip(&legs).any(|(leg, definition)| {
        !shared.reference.carries_smart_combo_leg(&leg.exchange, definition.sec_type.to_api_str())
    });
    let mut native = false;
    let mut delta_neutral =
        contract.exchange == "SMART" && separate_delta_neutral(&combination, &legs);
    let wire_combination = with_neutral_price_per_magnifier(shared, &combination, &legs);
    let mut price_mode = crate::control::attached_combos::price_mode(&combination, &legs);
    let mut combo_multiplier = 1.0;
    let mut confirmation = None;
    if contract.exchange == "SMART" {
        let multiplier_answer = request_combo(shared, control, deadline, |request_key| {
            let mut fields = crate::control::attached_combos::confirmation_request(
                request_key,
                "SMART",
                &wire_combination,
                delta_neutral,
                &legs,
                Some(1.0),
                include_exchanges,
            );
            fields[0].1 = "36".into();
            fields
        })
        .await?;
        let multiplier = field(&multiplier_answer, 231)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| *value != f64::MAX)
            .unwrap_or(0.0);
        combo_multiplier = multiplier;
        if session.enables("REJGNONAT") {
            native = resolve_native(
                shared,
                &combination,
                session,
                &legs,
                &definition,
                control,
                deadline,
                delta_neutral,
                multiplier,
                price_mode,
                &mut lookup,
            )
            .await?;
        }
        let answer = request_combo(shared, control, deadline, |request_key| {
            let mut fields = crate::control::attached_combos::confirmation_request(
                request_key,
                "SMART",
                &wire_combination,
                delta_neutral,
                &legs,
                Some(multiplier),
                include_exchanges,
            );
            let guaranteed = if session.enables("REJGNONAT") { native } else { synthetic };
            if combination.combo_legs.is_empty() || !guaranteed {
                fields.insert(4, (6248, "1".into()));
            }
            fields
        })
        .await?;
        if let Some(key) = field(&answer, 6085).filter(|key| !key.is_empty()) {
            price_mode =
                key.split_once('|').and_then(|(_, value)| value.parse::<i32>().ok()).unwrap_or(0);
        }
        delta_neutral &= !matches!(price_mode, 3 | 4);
        confirmation = Some(answer);
        if !session.enables("REJGNONAT") {
            native = resolve_native(
                shared,
                &combination,
                session,
                &legs,
                &definition,
                control,
                deadline,
                delta_neutral,
                multiplier,
                price_mode,
                &mut lookup,
            )
            .await?;
        }
    } else if contract.delta_neutral_contract.is_none() {
        let answer = request_combo(shared, control, deadline, |request_key| {
            crate::control::attached_combos::confirmation_request(
                request_key,
                &contract.exchange,
                &combination,
                false,
                &legs,
                needs_multiplier.then_some(1.0),
                false,
            )
        })
        .await?;
        if ratio_factor == 1.0 || !needs_multiplier {
            price_mode = apply_confirmation(&mut combination, &legs, &answer)?;
        }
        confirmation = Some(answer);
    }
    let frame = crate::types::AttachedComboFrame {
        multiplier: (needs_multiplier && !matches!(kind, 2 | 3 | 4 | 5 | 7 | 8 | 10))
            .then_some(combo_multiplier),
        price_mode: if matches!(price_mode, 1 | 2) { 2 } else { price_mode },
        combo_type: kind,
        market_data_generic: needs_multiplier,
        separate_delta_neutral: delta_neutral,
        include_leg_exchanges: include_exchanges,
        delta_neutral_contract: wire_combination.delta_neutral_contract.as_ref().map(|neutral| {
            crate::types::DeltaNeutralContractSpec {
                con_id: neutral.con_id,
                delta: neutral.delta,
                price: neutral.price,
            }
        }),
    };
    shared.reference.update_attached_combo(&key, |combo| combo.frame = Some(frame));
    combination.combo_legs.sort_by_key(|leg| leg.con_id);
    shared.reference.update_attached_combo(&key, |combo| {
        combo.legs = Some((
            combination.combo_legs.clone(),
            if session.enables("CMAG") { ratio_factor } else { 1.0 },
        ))
    });
    if let Some(base) =
        definition.market_rule_id.and_then(|id| shared.reference.market_rule(id as i32))
    {
        let leg_rules: Vec<_> = legs
            .iter()
            .map(|leg| {
                (
                    leg.sec_type.clone(),
                    leg.market_rule_id.and_then(|id| shared.reference.market_rule(id as i32)),
                )
            })
            .collect();
        let rule = super::attached_combo_rules::effective_rule(
            &base,
            &leg_rules,
            super::attached_combo_rules::ComboRuleTerms {
                smart: contract.exchange == "SMART",
                delta_neutral,
                uses_confirmation_rounding: contract.delta_neutral_contract.is_some()
                    || !two_leg_neutral_strategy(&legs),
                negative_prices: base.negative_prices || matches!(kind, 1 | 3 | 5 | 11),
            },
            confirmation.as_deref(),
        );
        shared.reference.update_attached_combo(&key, |combo| combo.price_rule = Some(rule));
    }
    let enabled = if !allows_rth(&permissions) {
        false
    } else if contract.exchange != "SMART" {
        definition_allows_rth(&definition) && legs.iter().all(definition_allows_rth)
    } else if !synthetic && !native {
        false
    } else {
        let first_symbol = legs.first().map(|leg| leg.symbol.as_str()).unwrap_or("");
        let common_symbol =
            !first_symbol.is_empty() && legs.iter().all(|leg| leg.symbol == first_symbol);
        let symbol = if common_symbol { first_symbol } else { definition.symbol.as_str() };
        let currency = definition.currency.as_str();
        let mut enabled = false;
        if !symbol.is_empty() {
            for exchange in common_exchanges(&legs, &shared.reference.combo_excluded_exchanges()) {
                let answer =
                    exchange_definition(shared, symbol, &exchange, currency, &mut lookup).await?;
                if answer.as_ref().is_some_and(definition_allows_rth) {
                    enabled = true;
                    break;
                }
            }
        }
        enabled
    };
    shared.reference.update_attached_combo(&key, |combo| combo.regular_hours = Some(enabled));
    Ok(enabled)
}

fn simplify_legs(contract: &mut Contract) -> f64 {
    if contract.combo_legs.len() < 2 {
        return 1.0;
    }
    let divisor = crate::control::attached_combos::legs_divisor(&contract.combo_legs);
    if divisor > 1 {
        for leg in &mut contract.combo_legs {
            leg.ratio = (i64::from(leg.ratio) / i64::from(divisor)) as i32;
        }
    }
    f64::from(divisor)
}

/// A contract's definition, asked for once where none is held.
async fn definition_or_lookup(
    shared: &SharedState,
    con_id: i64,
    exchange: &str,
    lookup: &mut impl FnMut(
        &Contract,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), Refusal>> + Send>,
    >,
) -> Result<Option<ContractDefinition>, Refusal> {
    if let Some(definition) = shared.reference.contract_definition(con_id as u32, exchange) {
        return Ok(Some(definition));
    }
    lookup(&Contract { con_id, exchange: exchange.into(), ..Contract::default() }).await?;
    Ok(shared.reference.contract_definition(con_id as u32, exchange))
}

/// A combination's definition on one exchange, asked for once where none is
/// held. Nothing where the venue names none there.
async fn exchange_definition(
    shared: &SharedState,
    symbol: &str,
    exchange: &str,
    currency: &str,
    lookup: &mut impl FnMut(
        &Contract,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), Refusal>> + Send>,
    >,
) -> Result<Option<ContractDefinition>, Refusal> {
    if let Some(answer) = shared.reference.combo_definition(symbol, exchange, currency) {
        return Ok(Some(answer));
    }
    match lookup(&Contract {
        symbol: symbol.into(),
        sec_type: "BAG".into(),
        exchange: exchange.into(),
        currency: currency.into(),
        ..Contract::default()
    })
    .await
    {
        Ok(()) => Ok(shared.reference.combo_definition(symbol, exchange, currency)),
        Err(error) if error.code == 200 => Ok(None),
        Err(error) => Err(error),
    }
}

/// The combination with its hedge's price stated per unit of the hedge's
/// price magnifier, as a confirmation states it.
fn with_neutral_price_per_magnifier(
    shared: &SharedState,
    contract: &Contract,
    legs: &[ContractDefinition],
) -> Contract {
    let mut stated = contract.clone();
    if let Some(neutral) = &mut stated.delta_neutral_contract
        && let Some(underlying) = legs.iter().find(|leg| i64::from(leg.con_id) == neutral.con_id)
        && let Some(rule) =
            underlying.market_rule_id.and_then(|id| shared.reference.market_rule(id as i32))
    {
        neutral.price /= f64::from(rule.price_magnifier);
    }
    stated
}

fn two_leg_neutral_strategy(legs: &[ContractDefinition]) -> bool {
    legs.len() == 2
        && legs[0].symbol == legs[1].symbol
        && match crate::control::attached_combos::combo_type(
            legs.iter().map(|leg| leg.sec_type.clone()),
        ) {
            8 => legs[0].multiplier == legs[1].multiplier,
            11 => true,
            _ => false,
        }
}

fn separate_delta_neutral(contract: &Contract, legs: &[ContractDefinition]) -> bool {
    two_leg_neutral_strategy(legs)
        && contract.delta_neutral_contract.as_ref().is_some_and(|neutral| {
            neutral.delta != f64::MAX && neutral.delta != 0.0 && neutral.delta != f64::from_bits(1)
        })
}

fn apply_confirmation(
    contract: &mut Contract,
    legs: &[ContractDefinition],
    answer: &[u8],
) -> Result<i32, Refusal> {
    let expected_mode = crate::control::attached_combos::price_mode(contract, legs);
    let confirmed_key = field(answer, 6085)
        .ok_or_else(|| Refusal::no_answer("The combination answer has no definition"))?;
    let (key, mode) = confirmed_key.split_once('|').unwrap_or((&confirmed_key, "0"));
    let mode = mode.trim().parse::<i32>().unwrap_or(0);
    if !(mode == expected_mode || matches!((mode, expected_mode), (1, 2) | (2, 1))) {
        return Err(Refusal::no_answer("The combination answer did not confirm its price type"));
    }
    let mut stock_or_cash = None;
    let mut multiplier = f64::MAX;
    for entry in key.split(',') {
        let mut fields = entry.split('/');
        let Some(con_id) = fields.next().and_then(|value| value.parse::<i64>().ok()) else {
            continue;
        };
        let Some(ratio) = fields.next().and_then(|value| value.parse::<i32>().ok()) else {
            continue;
        };
        let Some(index) = contract.combo_legs.iter().position(|leg| leg.con_id == con_id) else {
            continue;
        };
        contract.combo_legs[index].ratio = ratio.wrapping_abs();
        let definition = &legs[index];
        if matches!(definition.sec_type, SecurityType::Stock | SecurityType::Forex) {
            stock_or_cash = Some(index);
        } else if multiplier > definition.multiplier {
            multiplier = definition.multiplier;
        }
    }
    if let Some(index) = stock_or_cash
        && !matches!(mode, 1 | 2)
        && multiplier != f64::MAX
    {
        contract.combo_legs[index].ratio =
            (f64::from(contract.combo_legs[index].ratio) * multiplier) as i32;
    }
    Ok(mode)
}

fn field(message: &[u8], wanted: u32) -> Option<String> {
    crate::control::contracts::tag_sequence(message)
        .into_iter()
        .find(|(tag, _)| *tag == wanted)
        .map(|(_, value)| value)
}

async fn request_combo(
    shared: &SharedState,
    control: &Sender<Request>,
    deadline: Instant,
    fields: impl FnOnce(&str) -> Vec<(u32, String)>,
) -> Result<Vec<u8>, Refusal> {
    static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
    let key = format!("AC.{}", NEXT_REQUEST.fetch_add(1, Ordering::Relaxed));
    let answer = shared.reference.expect_attached_combo_confirmation(key.clone());
    struct Waiting<'a>(&'a SharedState, String);
    impl Drop for Waiting<'_> {
        fn drop(&mut self) {
            self.0.reference.stop_waiting_for_combo_confirmation(&self.1);
        }
    }
    let _waiting = Waiting(shared, key.clone());
    control
        .send(Request::ConfirmAttachedCombo { request_key: key.clone(), fields: fields(&key) })
        .map_err(|error| Refusal::not_connected(format!("Engine stopped: {error}")))?;
    super::attached_loading::answer(answer, deadline, "Combination request timed out").await
}

fn add_neutral_leg(contract: &mut Contract, underlying: &ContractDefinition) {
    let Some(neutral) = &contract.delta_neutral_contract else {
        return;
    };
    let delta = if neutral.delta == 0.0 { 1.0 } else { neutral.delta };
    let mut factor = 1;
    if underlying.sec_type == SecurityType::Future && delta.fract() != 0.0 {
        let mut divisor = (delta * 100.0).abs() as i32;
        for leg in &contract.combo_legs {
            divisor = crate::control::attached_combos::gcd(
                u64::from(divisor.unsigned_abs()),
                u64::from(leg.ratio.wrapping_mul(100).unsigned_abs()),
            ) as i32;
        }
        if divisor != 0 {
            factor = 100 / divisor;
        }
    }
    let ratio = if matches!(underlying.sec_type, SecurityType::Stock | SecurityType::Forex) {
        let multiplier =
            if underlying.multiplier_stated { underlying.multiplier as i32 } else { 100 };
        (f64::from(multiplier) * delta) as i32
    } else {
        (f64::from(factor) * delta) as i32
    };
    for leg in &mut contract.combo_legs {
        leg.ratio = leg.ratio.wrapping_mul(factor);
    }
    contract.combo_legs.push(crate::types::model::ComboLeg {
        con_id: neutral.con_id,
        ratio: ratio.wrapping_abs(),
        action: if delta > 0.0 { "SELL" } else { "BUY" }.into(),
        exchange: underlying.exchange.clone(),
        ..Default::default()
    });
}

fn request_rules(
    shared: &SharedState,
    control: &Sender<Request>,
    exchange: &str,
) -> Result<(), Refusal> {
    let exchange = crate::control::attached_combos::rules_exchange(exchange);
    if !exchange.is_empty()
        && shared.reference.request_attached_combo_rules(exchange)
        && let Err(error) =
            control.send(Request::FetchAttachedComboRules { exchange: exchange.into() })
    {
        shared.reference.forget_attached_combo_rule_request(exchange);
        return Err(Refusal::not_connected(format!("Engine stopped: {error}")));
    }
    Ok(())
}

async fn exchange_rules(
    shared: &SharedState,
    control: &Sender<Request>,
    exchange: &str,
    deadline: Instant,
) -> Result<Option<crate::control::attached_combos::ComboRules>, Refusal> {
    let exchange = crate::control::attached_combos::rules_exchange(exchange);
    if let Some(rules) = shared.reference.attached_combo_rules(exchange) {
        return Ok(Some(rules));
    }
    request_rules(shared, control, exchange)?;
    let until = deadline.min(Instant::now() + Duration::from_secs(5));
    Ok(std::future::poll_fn(|_| {
        let rules = shared.reference.attached_combo_rules(exchange);
        if rules.is_some() || Instant::now() >= until {
            std::task::Poll::Ready(rules)
        } else {
            std::task::Poll::Pending
        }
    })
    .await)
}

fn combination_needs_multiplier(
    shared: &SharedState,
    contract: &Contract,
    legs: &[ContractDefinition],
) -> bool {
    let same_symbol =
        legs.first().is_none_or(|first| legs.iter().all(|leg| leg.symbol == first.symbol));
    if contract.exchange == "SMART" || !same_symbol {
        return true;
    }
    let kind =
        crate::control::attached_combos::combo_type(legs.iter().map(|leg| leg.sec_type.clone()));
    let mut stocks = 0;
    let mut exchange = None;
    for (leg, definition) in contract.combo_legs.iter().zip(legs) {
        match definition.sec_type {
            SecurityType::Stock => {
                stocks += 1;
                if stocks > 1 {
                    return true;
                }
            }
            SecurityType::Forex => {}
            _ => {
                if leg.exchange == "SMART"
                    || exchange.is_some_and(|previous| previous != leg.exchange)
                {
                    return true;
                }
                exchange = Some(leg.exchange.as_str());
                let rules = shared.reference.attached_combo_rules(&leg.exchange);
                if let Some(rules) = &rules {
                    let code = crate::control::attached_combos::type_code(
                        crate::control::attached_combos::combo_type([definition.sec_type.clone()]),
                    );
                    if !rules.allowed.starts_with(|first: char| first.is_ascii_lowercase())
                        || !rules.allowed.contains(code)
                    {
                        return true;
                    }
                }
                if matches!(kind, 1 | 3 | 5)
                    && (definition.sec_type != SecurityType::Option || rules.is_none())
                {
                    return true;
                }
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
async fn resolve_native(
    shared: &SharedState,
    contract: &Contract,
    session: &OrderSession,
    legs: &[ContractDefinition],
    definition: &ContractDefinition,
    control: &Sender<Request>,
    deadline: Instant,
    delta_neutral: bool,
    multiplier: f64,
    price_mode: i32,
    lookup: &mut impl FnMut(
        &Contract,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), Refusal>> + Send>,
    >,
) -> Result<bool, Refusal> {
    let key = combo_key(contract);
    let kind =
        crate::control::attached_combos::combo_type(legs.iter().map(|leg| leg.sec_type.clone()));
    let has_ev_rule = legs.iter().any(|leg| !leg.ev_rule.is_empty());
    if !has_ev_rule
        && let Some(answer) = shared.reference.attached_combo(&key).and_then(|combo| combo.native)
    {
        return Ok(answer);
    }
    if !has_ev_rule
        && ((legs.len() == 2 && kind == 9)
            || (!session.enables("NATGCOMB") && !session.enables("REJGNONAT")))
    {
        shared.reference.update_attached_combo(&key, |combo| combo.native = Some(false));
        return native_result(shared, definition, legs, false);
    }
    if eligible_smart_legs(legs, session) && !session.enables("REJGNONAT") {
        shared.reference.update_attached_combo(&key, |combo| combo.native = Some(false));
        return native_result(shared, definition, legs, false);
    }
    // One symbol on every leg, or no native combination is looked for.
    let Some(symbol) = legs
        .first()
        .map(|first| first.symbol.as_str())
        .filter(|symbol| !symbol.is_empty() && legs.iter().all(|leg| leg.symbol == *symbol))
    else {
        shared.reference.update_attached_combo(&key, |combo| combo.native = Some(false));
        return native_result(shared, definition, legs, false);
    };
    let currency = definition.currency.as_str();
    let confirmation = if delta_neutral {
        with_neutral_price_per_magnifier(shared, contract, legs)
    } else {
        contract.clone()
    };
    let mut native = false;
    for exchange in common_exchanges(legs, &shared.reference.combo_excluded_exchanges()) {
        if exchange_definition(shared, symbol, &exchange, currency, lookup).await?.is_none() {
            continue;
        }
        let include_multiplier = combination_needs_multiplier(shared, &confirmation, legs);
        let include_exchanges = exchange == "SMART"
            && confirmation.combo_legs.iter().zip(legs).any(|(leg, definition)| {
                !shared
                    .reference
                    .carries_smart_combo_leg(&leg.exchange, definition.sec_type.to_api_str())
            });
        let result = request_combo(
            shared,
            control,
            deadline.min(Instant::now() + Duration::from_secs(5)),
            |request_key| {
                let mut fields = crate::control::attached_combos::confirmation_request(
                    request_key,
                    &exchange,
                    &confirmation,
                    delta_neutral,
                    legs,
                    include_multiplier.then_some(multiplier),
                    include_exchanges,
                );
                if let Some((_, mode)) = fields.iter_mut().find(|(tag, _)| *tag == 6175) {
                    *mode = if matches!(price_mode, 1 | 2) { 2 } else { price_mode }.to_string();
                }
                fields
            },
        )
        .await;
        let combo_key = match result {
            Ok(answer) => answer,
            Err(error) if error.code == Refusal::NO_ANSWER => continue,
            Err(error) => return Err(error),
        };
        if !crate::control::contracts::tag_sequence(&combo_key)
            .iter()
            .any(|(tag, value)| *tag == 6085 && !value.is_empty())
        {
            continue;
        }
        if !delta_neutral {
            let rules = exchange_rules(shared, control, &exchange, deadline).await?;
            if rules.is_none_or(|rules| {
                rules.dn_only.contains(crate::control::attached_combos::type_code(kind))
            }) {
                continue;
            }
        }
        native = true;
        break;
    }
    shared.reference.update_attached_combo(&key, |combo| combo.native = Some(native));
    native_result(shared, definition, legs, native)
}

fn native_result(
    shared: &SharedState,
    definition: &ContractDefinition,
    legs: &[ContractDefinition],
    native: bool,
) -> Result<bool, Refusal> {
    if !legs.iter().any(|leg| !leg.ev_rule.is_empty()) {
        return Ok(native);
    }
    let error = if !shared.reference.supports_attached_order_type(definition, "EVRULE") {
        Some(
            "Combos are not supported for products trading on the basis other than currency price.",
        )
    } else if legs.first().is_some_and(|first| {
        legs.iter().any(|leg| economic_rule(&leg.ev_rule) != economic_rule(&first.ev_rule))
    }) {
        Some("Combos with legs for products with different economic rules are not supported")
    } else if !native {
        Some(
            "Combos for products trading on the basis other than currency price could not be Non-Guaranteed",
        )
    } else {
        None
    };
    if let Some(error) = error {
        return Err(Refusal::no_definition(format!(
            "No security definition has been found for the request:{error}"
        )));
    }
    Ok(native)
}

fn economic_rule(rule: &str) -> (&str, Option<&str>) {
    let fields: Vec<_> = rule.split(':').collect();
    let last = fields.iter().rposition(|field| !field.is_empty()).unwrap_or(0);
    (fields[0], (last > 0).then(|| fields[1]))
}

#[cfg(test)]
mod tests {
    fn drive<T>(
        future: impl std::future::Future<Output = T>,
        requests: &std::sync::mpsc::Receiver<Request>,
        control: &Sender<Request>,
    ) -> T {
        let mut future = std::pin::pin!(future);
        super::super::attached_loading::tests::block_on(std::future::poll_fn(|cx| {
            let result = future.as_mut().poll(cx);
            for request in requests.try_iter() {
                control.send(request).unwrap();
            }
            result
        }))
    }

    fn resolve(
        shared: &SharedState,
        contract: &Contract,
        session: &OrderSession,
        control: &Sender<Request>,
        deadline: Instant,
        mut lookup: impl FnMut(&Contract) -> Result<(), Refusal>,
    ) -> Result<bool, Refusal> {
        let (send, receive) = std::sync::mpsc::channel();
        drive(
            super::resolve(shared, contract, session, &send, deadline, |contract| {
                Box::pin(std::future::ready(lookup(contract)))
            }),
            &receive,
            control,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_native(
        shared: &SharedState,
        contract: &Contract,
        session: &OrderSession,
        legs: &[ContractDefinition],
        definition: &ContractDefinition,
        control: &Sender<Request>,
        deadline: Instant,
        delta_neutral: bool,
        multiplier: f64,
        price_mode: i32,
        lookup: &mut impl FnMut(&Contract) -> Result<(), Refusal>,
    ) -> Result<bool, Refusal> {
        let (send, receive) = std::sync::mpsc::channel();
        drive(
            super::resolve_native(
                shared,
                contract,
                session,
                legs,
                definition,
                &send,
                deadline,
                delta_neutral,
                multiplier,
                price_mode,
                &mut |contract| Box::pin(std::future::ready(lookup(contract))),
            ),
            &receive,
            control,
        )
    }

    use super::*;
    use crate::types::model::ComboLeg;

    #[test]
    fn attached_combo_registration_keeps_distinct_legs_on_distinct_slots() {
        let mut market = crate::engine::market_state::MarketState::new();
        let mut contract = Contract {
            sec_type: "BAG".into(),
            symbol: "ABC".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            combo_legs: vec![
                ComboLeg {
                    con_id: 1,
                    ratio: 1,
                    action: "BUY".into(),
                    exchange: "SMART".into(),
                    ..Default::default()
                },
                ComboLeg {
                    con_id: 2,
                    ratio: 2,
                    action: "SELL".into(),
                    exchange: "SMART".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut register = |contract: &Contract| {
            market.register_described(
                0,
                &contract.symbol,
                &contract.sec_type,
                &contract.exchange,
                &registration_identity(contract),
                "",
            )
        };
        let first = register(&contract);
        contract.combo_legs.reverse();
        assert_eq!(register(&contract), first);
        contract.combo_legs[0].con_id = 3;
        let second = register(&contract);
        assert_ne!(second, first);
        contract.combo_legs[0].ratio = 3;
        assert_ne!(register(&contract), second);
        contract.delta_neutral_contract =
            Some(crate::types::model::DeltaNeutralContract { con_id: 4, delta: 0.5, price: 20.0 });
        let hedged = register(&contract);
        contract.delta_neutral_contract.as_mut().unwrap().delta = -0.5;
        assert_ne!(register(&contract), hedged);
        let identity = market.order_identity(first).unwrap();
        assert!(identity.expiry.is_empty());
        assert!(identity.strike.is_empty());
        assert!(identity.right.is_empty());
        assert!(identity.multiplier.is_empty());
        assert!(identity.trading_class.is_empty());
        assert!(identity.local_symbol.is_empty());
        assert!(identity.currency.is_empty());
        contract.sec_type = "FUT".into();
        contract.last_trade_date_or_contract_month = "202612".into();
        contract.multiplier = "50".into();
        assert_eq!(registration_identity(&contract), "202612|0||50|||USD");
    }

    fn definition(
        con_id: u32,
        kind: SecurityType,
        exchange: &str,
        regular: bool,
    ) -> ContractDefinition {
        ContractDefinition {
            con_id,
            symbol: "ABC".into(),
            sec_type: kind,
            exchange: exchange.into(),
            currency: "USD".into(),
            valid_exchanges: vec!["SMART".into(), "CBOE".into(), "ISE".into()],
            order_type_key: "x".into(),
            order_type_rules: if regular { vec![("RTH".into(), 0)] } else { Vec::new() },
            ..ContractDefinition::default()
        }
    }

    #[test]
    fn smart_combinations_resolve_missing_legs_and_common_exchange_definitions() {
        let shared = SharedState::new();
        shared.reference.set_attached_combo_rules("IBCX".into(), Default::default());
        shared.reference.set_order_permissions(std::collections::HashMap::from([(
            "COMB".into(),
            vec!["RTH".into()],
        )]));
        shared.reference.set_combo_excluded_exchanges("ISE/OPT".into());
        let contract = Contract {
            symbol: "ABC".into(),
            sec_type: "BAG".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            combo_legs: vec![
                ComboLeg { con_id: 1, ratio: 1, exchange: "SMART".into(), ..Default::default() },
                ComboLeg { con_id: 2, ratio: 1, exchange: "SMART".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let mut requests = Vec::new();
        let (control, receive) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let shared = &shared;
            scope.spawn(move || {
                for (subtype, reply) in
                    [("36", b"231=100\x01".as_slice()), ("7", b"6085=1/1,2/-1\x01".as_slice())]
                {
                    let Request::ConfirmAttachedCombo { request_key, fields } =
                        receive.recv_timeout(Duration::from_secs(1)).unwrap()
                    else {
                        panic!("combination request");
                    };
                    assert!(fields.contains(&(6040, subtype.into())));
                    if subtype == "7" {
                        assert!(fields.contains(&(231, "100".into())));
                    }
                    shared
                        .reference
                        .answer_attached_combo_confirmation(&request_key, reply.to_vec());
                }
            });
            assert!(
                resolve(
                    shared,
                    &contract,
                    &OrderSession::single("U1"),
                    &control,
                    Instant::now() + Duration::from_secs(1),
                    |request| {
                        requests.push(request.clone());
                        let kind = if request.con_id == 0
                            || request.con_id
                                == i64::from(shared.reference.smart_combo_conid("USD").unwrap_or(0))
                        {
                            SecurityType::Combo
                        } else {
                            SecurityType::Option
                        };
                        shared.reference.cache_contract_definition(definition(
                            request.con_id as u32,
                            kind,
                            &request.exchange,
                            true,
                        ));
                        Ok(())
                    }
                )
                .unwrap()
            );
        });
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[2].con_id,
            i64::from(shared.reference.smart_combo_conid("USD").unwrap_or(0))
        );
        assert_eq!(requests[3].exchange, "CBOE");
        assert!(
            resolve(
                &shared,
                &contract,
                &OrderSession::single("U1"),
                &control,
                Instant::now(),
                |_| panic!("cached resolution")
            )
            .unwrap()
        );
    }

    #[test]
    fn direct_combinations_require_the_combo_and_every_leg_to_allow_regular_hours() {
        let shared = SharedState::new();
        shared.reference.set_attached_combo_rules("IBCX".into(), Default::default());
        shared.reference.set_attached_combo_rules("CBOE".into(), Default::default());
        shared.reference.set_order_permissions(std::collections::HashMap::from([(
            "COMB".into(),
            vec!["RTH".into()],
        )]));
        shared.reference.cache_contract_definition(definition(
            20,
            SecurityType::Combo,
            "CBOE",
            true,
        ));
        shared.reference.cache_contract_definition(definition(
            1,
            SecurityType::Option,
            "CBOE",
            false,
        ));
        let contract = Contract {
            con_id: 20,
            sec_type: "BAG".into(),
            exchange: "CBOE".into(),
            combo_legs: vec![ComboLeg {
                con_id: 1,
                exchange: "CBOE".into(),
                ..ComboLeg::default()
            }],
            ..Contract::default()
        };
        let (control, receive) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let shared = &shared;
            scope.spawn(move || {
                let Request::ConfirmAttachedCombo { request_key, fields } =
                    receive.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("confirmation request");
                };
                assert!(fields.contains(&(6040, "7".into())));
                assert!(fields.contains(&(207, "CBOE".into())));
                shared
                    .reference
                    .answer_attached_combo_confirmation(&request_key, b"6085=1/1\x01".to_vec());
            });
            assert!(
                !resolve(
                    shared,
                    &contract,
                    &OrderSession::single("U1"),
                    &control,
                    Instant::now() + Duration::from_secs(1),
                    |_| panic!("cached definitions")
                )
                .unwrap()
            );
        });
    }

    #[test]
    fn smart_eligibility_and_exclusions_preserve_the_stated_gates() {
        let option = definition(1, SecurityType::Option, "SMART", true);
        let stock = definition(2, SecurityType::Stock, "SMART", true);
        assert!(eligible_smart_legs(&[option.clone(), stock.clone()], &OrderSession::single("U1")));
        assert!(!eligible_smart_legs(&[stock.clone(), stock], &OrderSession::single("U1")));
        assert!(!eligible_smart_legs(std::slice::from_ref(&option), &OrderSession::single("T1")));
        let mut dynamic = OrderSession::single("T1");
        dynamic.features.push("DYNACCTADD".into());
        assert!(eligible_smart_legs(std::slice::from_ref(&option), &dynamic));
        assert_eq!(common_exchanges(&[option], "CBOE/*,ISE/STK,bad"), vec!["ISE"]);
        assert!(!allows_rth(["RTH/4", "RTH/0"]));
        assert!(allows_rth(["RTH/1"]));
    }

    #[test]
    fn native_smart_combinations_confirm_legs_before_resolving_regular_hours() {
        let shared = SharedState::new();
        shared.reference.set_order_permissions(std::collections::HashMap::from([(
            "COMB".into(),
            vec!["RTH".into()],
        )]));
        let mut session = OrderSession::single("U1");
        session.features.push("REJGNONAT".into());
        let contract = Contract {
            sec_type: "BAG".into(),
            symbol: "ABC".into(),
            currency: "USD".into(),
            exchange: "SMART".into(),
            combo_legs: vec![
                ComboLeg {
                    con_id: 1,
                    exchange: "SMART".into(),
                    ratio: 1,
                    action: "BUY".into(),
                    ..Default::default()
                },
                ComboLeg {
                    con_id: 2,
                    exchange: "SMART".into(),
                    ratio: 1,
                    action: "SELL".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        for con_id in [1, 2] {
            shared.reference.cache_contract_definition(definition(
                con_id,
                SecurityType::Future,
                "SMART",
                true,
            ));
        }
        shared.reference.cache_contract_definition(definition(
            0,
            SecurityType::Combo,
            "CBOE",
            true,
        ));
        shared.reference.cache_contract_definition(definition(
            shared.reference.smart_combo_conid("USD").unwrap_or(0) as u32,
            SecurityType::Combo,
            "SMART",
            true,
        ));
        let (control, receive) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let shared = &shared;
            scope.spawn(move || {
                let Request::FetchAttachedComboRules { exchange } =
                    receive.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("leg rules request");
                };
                assert_eq!(exchange, "IBCX");
                shared.reference.set_attached_combo_rules(exchange, Default::default());
                let Request::ConfirmAttachedCombo { request_key, fields } =
                    receive.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("multiplier request");
                };
                assert!(fields.contains(&(6040, "36".into())));
                shared
                    .reference
                    .answer_attached_combo_confirmation(&request_key, b"231=50\x01".to_vec());
                let Request::ConfirmAttachedCombo { request_key, fields } =
                    receive.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("confirmation request");
                };
                assert!(fields.contains(&(207, "CBOE".into())));
                assert!(fields.contains(&(6134, "2".into())));
                shared
                    .reference
                    .answer_attached_combo_confirmation("unrelated", b"6085=wrong\x01".to_vec());
                shared.reference.answer_attached_combo_confirmation(
                    &request_key,
                    b"6085=1/1,2/-1\x01".to_vec(),
                );
                let Request::FetchAttachedComboRules { exchange } =
                    receive.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("rules request");
                };
                assert_eq!(exchange, "CBOE");
                shared.reference.set_attached_combo_rules(
                    exchange,
                    crate::control::attached_combos::ComboRules::default(),
                );
                let Request::ConfirmAttachedCombo { request_key, fields } =
                    receive.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("smart confirmation request");
                };
                assert!(fields.contains(&(207, "BEST".into())));
                assert!(!fields.iter().any(|(tag, _)| *tag == 6248));
                shared.reference.answer_attached_combo_confirmation(
                    &request_key,
                    b"6085=1/1,2/-1\x01".to_vec(),
                );
            });
            assert!(
                resolve(
                    shared,
                    &contract,
                    &session,
                    &control,
                    Instant::now() + Duration::from_secs(1),
                    |_| panic!("cached definitions")
                )
                .unwrap()
            );
        });
        assert!(
            shared
                .reference
                .attached_combo(&format!("{contract:?}"))
                .and_then(|combo| combo.native)
                .unwrap()
        );
        assert!(
            resolve(&shared, &contract, &session, &control, Instant::now(), |_| panic!(
                "cached result"
            ))
            .unwrap()
        );
    }

    #[test]
    fn delta_neutral_only_exchange_does_not_make_plain_combinations_native() {
        let shared = SharedState::new();
        let mut session = OrderSession::single("U1");
        session.features.push("NATGCOMB".into());
        let mut future = definition(1, SecurityType::Future, "SMART", true);
        future.valid_exchanges = vec!["CBOE".into()];
        shared.reference.cache_contract_definition(definition(
            0,
            SecurityType::Combo,
            "CBOE",
            true,
        ));
        let (control, receive) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let shared = &shared;
            scope.spawn(move || {
                let Request::ConfirmAttachedCombo { request_key, .. } =
                    receive.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("confirmation request");
                };
                shared.reference.answer_attached_combo_confirmation(
                    &request_key,
                    b"6085=accepted\x01".to_vec(),
                );
                let Request::FetchAttachedComboRules { exchange } =
                    receive.recv_timeout(Duration::from_secs(1)).unwrap()
                else {
                    panic!("rules request");
                };
                shared.reference.set_attached_combo_rules(
                    exchange,
                    crate::control::attached_combos::ComboRules {
                        dn_only: "f".into(),
                        ..Default::default()
                    },
                );
            });
            assert!(
                !resolve_native(
                    shared,
                    &Contract::default(),
                    &session,
                    &[future],
                    &definition(0, SecurityType::Combo, "SMART", true),
                    &control,
                    Instant::now() + Duration::from_secs(1),
                    false,
                    1.0,
                    0,
                    &mut |_| panic!("cached definition")
                )
                .unwrap()
            );
        });
    }
    #[test]
    fn attached_combo_delta_neutral_legs_use_the_stated_multiplier_and_scale_futures() {
        let make = |delta| Contract {
            delta_neutral_contract: Some(crate::types::model::DeltaNeutralContract {
                con_id: 3,
                delta,
                price: 40.0,
            }),
            combo_legs: vec![ComboLeg {
                con_id: 1,
                ratio: 2,
                action: "BUY".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut stock = definition(3, SecurityType::Stock, "SMART", true);
        let mut contract = make(-0.25);
        add_neutral_leg(&mut contract, &stock);
        assert_eq!(contract.combo_legs[1].ratio, 25);
        assert_eq!(contract.combo_legs[1].action, "BUY");
        stock.multiplier_stated = true;
        stock.multiplier = 10.0;
        let mut contract = make(0.25);
        add_neutral_leg(&mut contract, &stock);
        assert_eq!(contract.combo_legs[1].ratio, 2);
        assert_eq!(contract.combo_legs[1].action, "SELL");
        let mut contract = make(0.25);
        add_neutral_leg(&mut contract, &definition(3, SecurityType::Future, "CME", true));
        assert_eq!(contract.combo_legs.iter().map(|leg| leg.ratio).collect::<Vec<_>>(), [8, 1]);
        let mut contract = make(0.0);
        add_neutral_leg(&mut contract, &stock);
        assert_eq!(contract.combo_legs[1].ratio, 10);
        assert_eq!(contract.delta_neutral_contract.unwrap().delta, 0.0);
    }

    #[test]
    fn attached_combo_multiplier_depends_on_leg_routes_and_exchange_rules() {
        let shared = SharedState::new();
        let mut contract = Contract {
            combo_legs: vec![ComboLeg {
                con_id: 1,
                ratio: 1,
                exchange: "CBOE".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let option = definition(1, SecurityType::Option, "CBOE", true);
        assert!(!combination_needs_multiplier(&shared, &contract, std::slice::from_ref(&option)));
        contract.combo_legs[0].exchange = "SMART".into();
        assert!(combination_needs_multiplier(&shared, &contract, std::slice::from_ref(&option)));
        contract.combo_legs[0].exchange = "CBOE".into();
        shared.reference.set_attached_combo_rules("CBOE".into(), Default::default());
        assert!(combination_needs_multiplier(&shared, &contract, std::slice::from_ref(&option)));
        shared.reference.set_attached_combo_rules(
            "CBOE".into(),
            crate::control::attached_combos::ComboRules {
                allowed: "o".into(),
                ..Default::default()
            },
        );
        assert!(!combination_needs_multiplier(&shared, &contract, std::slice::from_ref(&option)));
        contract.delta_neutral_contract =
            Some(crate::types::model::DeltaNeutralContract { con_id: 1, delta: 0.5, price: 10.0 });
        assert!(!combination_needs_multiplier(&shared, &contract, &[option]));
    }

    #[test]
    fn attached_combo_confirmation_updates_ratios_and_keeps_sides() {
        let mut contract = Contract {
            combo_legs: vec![
                ComboLeg { con_id: 1, ratio: 4, action: "SELL".into(), ..Default::default() },
                ComboLeg { con_id: 2, ratio: 8, action: "BUY".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let mut option = definition(2, SecurityType::Option, "CBOE", true);
        option.multiplier = 100.0;
        let legs = [definition(1, SecurityType::Stock, "SMART", true), option];
        simplify_legs(&mut contract);
        assert_eq!(contract.combo_legs.iter().map(|leg| leg.ratio).collect::<Vec<_>>(), [1, 2]);
        apply_confirmation(&mut contract, &legs, b"6085=1/-3,2/2\x01").unwrap();
        assert_eq!(contract.combo_legs.iter().map(|leg| leg.ratio).collect::<Vec<_>>(), [300, 2]);
        assert_eq!(contract.combo_legs[0].action, "SELL");
        assert_eq!(contract.combo_legs[1].action, "BUY");
        assert!(apply_confirmation(&mut contract, &legs, b"6085=1/1,2/-1|2\x01").is_err());
        contract.combo_legs[1].exchange = "ISE".into();
        apply_confirmation(&mut contract, &legs, b"6085=1/3,2/-2|1\x01").unwrap();
        assert_eq!(contract.combo_legs[0].ratio, 3);
    }

    #[test]
    fn attached_combo_delta_neutral_strategy_requires_compatible_two_leg_terms() {
        let make = |delta| Contract {
            delta_neutral_contract: Some(crate::types::model::DeltaNeutralContract {
                con_id: 1,
                delta,
                price: 40.0,
            }),
            ..Default::default()
        };
        let mut legs = [
            definition(1, SecurityType::Future, "CME", true),
            definition(2, SecurityType::FutureOption, "CME", true),
        ];
        assert!(separate_delta_neutral(&make(0.5), &legs));
        for delta in [0.0, f64::MAX, f64::from_bits(1)] {
            assert!(!separate_delta_neutral(&make(delta), &legs));
        }
        assert!(separate_delta_neutral(&make(f64::NAN), &legs));
        legs[1].multiplier = 50.0;
        assert!(!separate_delta_neutral(&make(0.5), &legs));
        legs[0].sec_type = SecurityType::Forex;
        legs[1].sec_type = SecurityType::Option;
        assert!(separate_delta_neutral(&make(0.5), &legs));
        legs[1].symbol = "OTHER".into();
        assert!(!separate_delta_neutral(&make(0.5), &legs));
    }

    #[test]
    fn attached_combo_smart_confirmation_precedes_optional_native_probe_and_keeps_price_terms() {
        use crate::control::contracts::{MarketRule, PriceIncrement};
        let shared = SharedState::new();
        for exchange in ["IBCX", "CBOE"] {
            shared.reference.set_attached_combo_rules(exchange.into(), Default::default());
        }
        let contract = Contract {
            sec_type: "BAG".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            symbol: "ABC".into(),
            combo_legs: vec![
                ComboLeg {
                    con_id: 1,
                    ratio: 2,
                    action: "BUY".into(),
                    exchange: "SMART".into(),
                    ..Default::default()
                },
                ComboLeg {
                    con_id: 2,
                    ratio: 4,
                    action: "SELL".into(),
                    exchange: "SMART".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        for con_id in [1, 2] {
            shared.reference.cache_contract_definition(definition(
                con_id,
                SecurityType::Future,
                "SMART",
                false,
            ));
        }
        let mut combo = definition(
            shared.reference.smart_combo_conid("USD").unwrap_or(0) as u32,
            SecurityType::Combo,
            "SMART",
            false,
        );
        combo.market_rule_id = Some(9);
        shared.reference.cache_contract_definition(combo);
        shared.reference.cache_contract_definition(definition(
            0,
            SecurityType::Combo,
            "CBOE",
            false,
        ));
        shared.reference.push_market_rules(vec![MarketRule {
            rule_id: 9,
            negative_prices: false,
            price_magnifier: 1,
            price_increments: vec![PriceIncrement { low_edge: 0.0, increment: 1.0 }],
            size_increments: Vec::new(),
            price_places: None,
            size_places: None,
        }]);
        let mut session = OrderSession::single("U1");
        session.features.extend(["NATGCOMB".into(), "CMAG".into()]);
        let (control, receive) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let shared = &shared;
            scope.spawn(move || {
                for (subtype, exchange, reply) in [
                    ("36", "BEST", b"231=50\x01".as_slice()),
                    ("7", "BEST", b"6085=1/3,2/-5\x016871=0.125\x01".as_slice()),
                    ("7", "CBOE", b"6085=1/3,2/-5\x01".as_slice()),
                ] {
                    let Request::ConfirmAttachedCombo { request_key, fields } =
                        receive.recv_timeout(Duration::from_secs(1)).unwrap()
                    else {
                        panic!("combination request");
                    };
                    assert!(fields.contains(&(6040, subtype.into())));
                    assert!(fields.contains(&(207, exchange.into())));
                    if subtype == "36" {
                        assert!(fields.contains(&(6081, "2".into())));
                    } else if exchange == "BEST" {
                        assert!(fields.contains(&(6248, "1".into())));
                    } else {
                        assert!(fields.contains(&(6081, "1".into())));
                        assert!(fields.contains(&(6081, "2".into())));
                    }
                    shared
                        .reference
                        .answer_attached_combo_confirmation(&request_key, reply.to_vec());
                }
            });
            assert!(
                !resolve(
                    shared,
                    &contract,
                    &session,
                    &control,
                    Instant::now() + Duration::from_secs(1),
                    |_| panic!("cached definitions")
                )
                .unwrap()
            );
        });
        let key = format!("{contract:?}");
        assert_eq!(
            shared
                .reference
                .attached_combo(&key)
                .and_then(|combo| combo.definition)
                .unwrap()
                .con_id,
            shared.reference.smart_combo_conid("USD").unwrap_or(0) as u32
        );
        let rule =
            shared.reference.attached_combo(&key).and_then(|combo| combo.price_rule).unwrap();
        assert_eq!(rule.price_increments[0].increment, 0.125);
        assert!(rule.negative_prices);
        let legs = shared
            .reference
            .attached_combo(&key)
            .and_then(|combo| combo.legs)
            .map(|(legs, _)| legs)
            .unwrap();
        assert_eq!(legs.iter().map(|leg| leg.ratio).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(
            shared
                .reference
                .attached_combo(&key)
                .and_then(|combo| combo.legs)
                .map(|(_, factor)| factor),
            Some(2.0)
        );
        let frame = shared.reference.attached_combo(&key).and_then(|combo| combo.frame).unwrap();
        assert_eq!(frame.combo_type, 2);
        assert!(frame.market_data_generic);
        assert_eq!(frame.price_mode, 0);
        assert_eq!(frame.multiplier, None);
        assert!(!frame.include_leg_exchanges);
    }
    #[test]
    fn attached_combo_separate_underlying_uses_its_price_rule_and_frame_terms() {
        use crate::control::contracts::{MarketRule, PriceIncrement};
        let shared = SharedState::new();
        shared.reference.set_attached_combo_rules("IBCX".into(), Default::default());
        shared.reference.set_attached_combo_rules("CASHX".into(), Default::default());
        let contract = Contract {
            sec_type: "BAG".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            symbol: "ABC".into(),
            combo_legs: vec![
                ComboLeg {
                    con_id: 1,
                    ratio: 1,
                    action: "BUY".into(),
                    exchange: "CASHX".into(),
                    ..Default::default()
                },
                ComboLeg {
                    con_id: 2,
                    ratio: 1,
                    action: "SELL".into(),
                    exchange: "SMART".into(),
                    ..Default::default()
                },
            ],
            delta_neutral_contract: Some(crate::types::model::DeltaNeutralContract {
                con_id: 1,
                delta: 0.5,
                price: 40.0,
            }),
            ..Default::default()
        };
        let mut underlying = definition(1, SecurityType::Forex, "CASHX", false);
        underlying.market_rule_id = Some(10);
        shared.reference.cache_contract_definition(underlying);
        shared.reference.cache_contract_definition(definition(
            2,
            SecurityType::Option,
            "SMART",
            false,
        ));
        shared.reference.cache_contract_definition(definition(
            shared.reference.smart_combo_conid("USD").unwrap_or(0) as u32,
            SecurityType::Combo,
            "SMART",
            false,
        ));
        shared.reference.push_market_rules(vec![MarketRule {
            rule_id: 10,
            negative_prices: false,
            price_magnifier: 100,
            price_increments: vec![PriceIncrement { low_edge: 0.0, increment: 1.0 }],
            size_increments: Vec::new(),
            price_places: None,
            size_places: None,
        }]);
        let (control, receive) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let shared = &shared;
            scope.spawn(move || {
                for subtype in ["36", "7"] {
                    let Request::ConfirmAttachedCombo { request_key, fields } =
                        receive.recv_timeout(Duration::from_secs(1)).unwrap()
                    else {
                        panic!("combination request");
                    };
                    assert!(fields.contains(&(6040, subtype.into())));
                    assert!(fields.contains(&(6147, "1".into())));
                    assert!(fields.contains(&(6149, "0.4".into())));
                    assert_eq!(fields.iter().filter(|(tag, _)| *tag == 616).count(), 2);
                    shared.reference.answer_attached_combo_confirmation(
                        &request_key,
                        if subtype == "36" {
                            b"231=50\x01".to_vec()
                        } else {
                            b"6085=1/1,2/-1\x01".to_vec()
                        },
                    );
                }
            });
            assert!(
                !resolve(
                    shared,
                    &contract,
                    &OrderSession::single("U1"),
                    &control,
                    Instant::now() + Duration::from_secs(1),
                    |_| panic!("cached definitions")
                )
                .unwrap()
            );
        });
        let frame = shared
            .reference
            .attached_combo(&format!("{contract:?}"))
            .and_then(|combo| combo.frame)
            .unwrap();
        assert_eq!(frame.multiplier, Some(50.0));
        assert_eq!(frame.combo_type, 11);
        assert!(frame.separate_delta_neutral);
        assert!(frame.include_leg_exchanges);
        assert_eq!(frame.delta_neutral_contract.unwrap().price, 0.4);
    }
    #[test]
    fn attached_combo_economic_rules_use_the_definition_and_matching_legs() {
        let shared = SharedState::new();
        let mut combo = definition(20, SecurityType::Combo, "SMART", false);
        let mut leg = definition(1, SecurityType::Option, "SMART", false);
        leg.ev_rule = "factor:one".into();
        let error = native_result(&shared, &combo, std::slice::from_ref(&leg), true).unwrap_err();
        assert_eq!(error.code, 200);
        assert!(error.message.ends_with(
            "Combos are not supported for products trading on the basis other than currency price."
        ));
        combo.order_type_rules.push(("EVRULE".into(), 0));
        shared.reference.cache_contract_definition(combo.clone());
        let mut other = leg.clone();
        other.ev_rule = "factor:two".into();
        assert!(
            native_result(&shared, &combo, &[leg.clone(), other], true)
                .unwrap_err()
                .message
                .ends_with(
                    "Combos with legs for products with different economic rules are not supported"
                )
        );
        assert!(native_result(&shared, &combo, std::slice::from_ref(&leg), false).unwrap_err().message.ends_with("Combos for products trading on the basis other than currency price could not be Non-Guaranteed"));
        assert!(native_result(&shared, &combo, &[leg], true).unwrap());
        assert_eq!(economic_rule("factor:"), economic_rule("factor"));
        assert_eq!(economic_rule("factor:one:ignored"), economic_rule("factor:one"));
    }
}
