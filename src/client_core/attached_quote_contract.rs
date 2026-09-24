//! The contract whose indicative quote is used by an attached parent.

use crate::bridge::SharedState;
use crate::control::contracts::{ContractDefinition, SecurityType};
use crate::types::model::{Contract, ContractDetails};

pub(crate) use crate::bridge::CachedAttachedQuote as CachedSelection;

#[allow(clippy::large_enum_variant)]
pub(crate) enum Selection {
    Ready(Contract),
    Resolve { con_id: u32, exchange: String },
    Unavailable,
}

pub(crate) fn key(contract: &Contract) -> (u32, u8) {
    (
        contract.con_id as u32,
        if contract.sec_type == "CONTFUT" {
            1
        } else if matches!(contract.exchange.as_str(), "OVERNIGHT" | "IBEOS") {
            2
        } else {
            0
        },
    )
}

fn held(shared: &SharedState, contract: &Contract) -> Option<Selection> {
    shared.reference.attached_quote_contract(key(contract)).map(|cached| match cached {
        CachedSelection::Original => Selection::Ready(contract.clone()),
        CachedSelection::Proxy(proxy) => Selection::Ready(proxy),
        CachedSelection::Unavailable => Selection::Unavailable,
    })
}

fn requires_proxy(shared: &SharedState, contract: &Contract) -> bool {
    if contract.sec_type == "CONTFUT" {
        return true;
    }
    let Some(definition) = super::attached_orders::contract_definition(shared, contract) else {
        return false;
    };
    if !["USESTKMD", "USESTKMD1", "USECSHMD"].iter().any(|name| shared.reference.enables(name))
        || !matches!(definition.under_sec_type.as_str(), "" | "*" | "UNK" | "STK" | "CS" | "CASH")
        || matches!(
            contract.sec_type.as_str(),
            "" | "*"
                | "UNK"
                | "STK"
                | "IND"
                | "CASH"
                | "CMDTY"
                | "FIXED"
                | "BILL"
                | "BOND"
                | "FUND"
                | "NEWS"
                | "BSK"
                | "ICU"
                | "CRYPTO"
                | "BAG"
                | "COMB"
                | "PDC"
        )
    {
        return false;
    }
    if let Some(cached) = shared.reference.attached_quote_contract(key(contract)) {
        return !matches!(cached, CachedSelection::Original);
    }
    let stock = shared.reference.supports_attached_order_type(&definition, "USESTKMD")
        || shared.reference.supports_attached_order_type(&definition, "USESTKMD1");
    let cash = shared.reference.supports_attached_order_type(&definition, "USECSHMD");
    stock && matches!(definition.under_sec_type.as_str(), "" | "*" | "UNK" | "STK" | "CS")
        || cash && definition.under_sec_type == "CASH"
        || matches!(contract.exchange.as_str(), "OVERNIGHT" | "IBEOS")
            && matches!(definition.under_sec_type.as_str(), "STK" | "CS")
}

/// Price construction reads an existing indication without starting a lookup:
/// the contract whose quotes price the order, or nothing yet.
pub(crate) fn cached(shared: &SharedState, contract: &Contract) -> Option<Contract> {
    if !requires_proxy(shared, contract) {
        return Some(contract.clone());
    }
    match held(shared, contract) {
        Some(Selection::Ready(ready)) => Some(ready),
        _ => None,
    }
}

pub(crate) fn select(shared: &SharedState, contract: &Contract) -> Selection {
    if !requires_proxy(shared, contract) {
        return Selection::Ready(contract.clone());
    }
    if let Some(held) = held(shared, contract) {
        return held;
    }
    let Some(definition) = super::attached_orders::contract_definition(shared, contract) else {
        return Selection::Unavailable;
    };
    if contract.sec_type == "CONTFUT" {
        return Selection::Resolve { con_id: definition.con_id, exchange: definition.exchange };
    }
    let con_id = definition.under_con_id;
    if con_id == 0 {
        return Selection::Unavailable;
    }
    let exchange =
        if matches!(contract.exchange.as_str(), "OVERNIGHT" | "IBEOS") { "OVERNIGHT" } else { "" };
    match shared.reference.contract_definition_exact(con_id, exchange) {
        Some(underlying) => complete(shared, contract, underlying),
        None => Selection::Resolve { con_id, exchange: exchange.into() },
    }
}

pub(crate) fn complete(
    shared: &SharedState,
    contract: &Contract,
    underlying: ContractDefinition,
) -> Selection {
    let proxy = ContractDetails::from_definition(&underlying).contract;
    if contract.sec_type == "CONTFUT" || underlying.sec_type == SecurityType::Forex {
        shared
            .reference
            .cache_attached_quote_contract(key(contract), CachedSelection::Proxy(proxy.clone()));
        return Selection::Ready(proxy);
    }
    if underlying.sec_type != SecurityType::Stock {
        shared.reference.cache_attached_quote_contract(key(contract), CachedSelection::Original);
        return Selection::Ready(contract.clone());
    }
    // The indication is recorded before resolving its preferred listing.
    shared
        .reference
        .cache_attached_quote_contract(key(contract), CachedSelection::Proxy(proxy.clone()));
    if matches!(underlying.exchange.as_str(), "OVERNIGHT" | "IBEOS") {
        return Selection::Ready(proxy);
    }
    let preferred = shared
        .reference
        .preferred_market(underlying.agg_group)
        .filter(|market| !market.is_empty() && market != "SMART");
    let primary = if shared.reference.enables("SEPLSTDIV")
        || underlying.valid_exchanges.contains(&underlying.primary_exchange)
    {
        underlying.primary_exchange.clone()
    } else {
        underlying.primary_exchange.split('.').next().unwrap_or_default().to_string()
    };
    let route = if let Some(preferred) = preferred {
        (underlying.exchange != preferred && underlying.valid_exchanges.contains(&preferred))
            .then_some(preferred)
    } else {
        (underlying.exchange != "SMART"
            && underlying.valid_exchanges.iter().any(|market| market == "SMART"))
        .then(|| "SMART".to_string())
    }
    .or_else(|| {
        (underlying.exchange != "SMART" && !primary.is_empty() && primary != underlying.exchange)
            .then_some(primary)
    });
    if let Some(exchange) = route {
        return match shared.reference.contract_definition_exact(underlying.con_id, &exchange) {
            Some(proxy) => complete(shared, contract, proxy),
            None => Selection::Resolve { con_id: underlying.con_id, exchange },
        };
    }
    Selection::Ready(proxy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent(shared: &SharedState, under_type: &str) -> Contract {
        shared.reference.set_enabled_features(vec!["USESTKMD".into(), "USECSHMD".into()]);
        let definition = ContractDefinition {
            con_id: 1,
            sec_type: SecurityType::Cfd,
            exchange: "SMART".into(),
            under_con_id: 2,
            under_sec_type: under_type.into(),
            order_type_key: "X".into(),
            order_type_rules: vec![("USESTKMD".into(), 0), ("USECSHMD".into(), 0)],
            ..ContractDefinition::default()
        };
        let contract = ContractDetails::from_definition(&definition).contract;
        shared.reference.cache_contract_definition(definition);
        contract
    }

    #[test]
    fn construction_reads_only_an_existing_indication() {
        let shared = SharedState::new();
        let contract = parent(&shared, "STK");
        shared.reference.cache_contract_definition(ContractDefinition {
            con_id: 2,
            sec_type: SecurityType::Stock,
            exchange: "SMART".into(),
            ..Default::default()
        });
        assert!(cached(&shared, &contract).is_none());
        assert!(matches!(select(&shared, &contract), Selection::Ready(proxy) if proxy.con_id == 2));
        assert_eq!(cached(&shared, &contract).unwrap().con_id, 2);
        shared.reference.set_enabled_features(Vec::new());
        assert_eq!(cached(&shared, &contract).unwrap().con_id, 1);
    }

    #[test]
    fn a_primary_listing_keeps_its_division_when_enabled_or_routable() {
        for (separate, valid, expected) in
            [(false, false, "LSE"), (true, false, "LSE.INT"), (false, true, "LSE.INT")]
        {
            let shared = SharedState::new();
            let contract = parent(&shared, "STK");
            if separate {
                shared.reference.add_enabled_features(vec!["SEPLSTDIV".into()]);
            }
            shared.reference.cache_contract_definition(ContractDefinition {
                con_id: 2,
                sec_type: SecurityType::Stock,
                exchange: "BATEUK".into(),
                primary_exchange: "LSE.INT".into(),
                valid_exchanges: if valid { vec!["LSE.INT".into()] } else { Vec::new() },
                ..Default::default()
            });
            assert!(
                matches!(select(&shared, &contract), Selection::Resolve { con_id: 2, exchange } if exchange == expected)
            );
            assert_eq!(cached(&shared, &contract).unwrap().exchange, "BATEUK");
        }
    }

    #[test]
    fn an_unresolved_underlying_is_qualified_without_a_quote() {
        let shared = SharedState::new();
        let contract = parent(&shared, "STK");
        assert!(
            matches!(select(&shared, &contract), Selection::Resolve { con_id: 2, exchange } if exchange.is_empty())
        );
    }

    #[test]
    fn a_stock_proxy_uses_its_preferred_market_and_qualified_route() {
        let shared = SharedState::new();
        let contract = parent(&shared, "STK");
        shared.reference.set_aggregate_exchanges("4,ARCA,CS,NYSE".into());
        shared.reference.cache_contract_definition(ContractDefinition {
            con_id: 2,
            sec_type: SecurityType::Stock,
            exchange: "NYSE".into(),
            agg_group: 4,
            valid_exchanges: vec!["ARCA".into(), "SMART".into()],
            ..ContractDefinition::default()
        });
        assert!(
            matches!(select(&shared, &contract), Selection::Resolve { con_id: 2, exchange } if exchange == "ARCA")
        );
        shared.reference.cache_contract_definition(ContractDefinition {
            con_id: 2,
            sec_type: SecurityType::Stock,
            exchange: "ARCA".into(),
            ..ContractDefinition::default()
        });
        assert!(
            matches!(complete(&shared, &contract, shared.reference.contract_definition_exact(2, "ARCA").unwrap()), Selection::Ready(proxy) if proxy.con_id == 2 && proxy.exchange == "ARCA")
        );
    }

    #[test]
    fn cash_uses_its_underlying_but_an_index_keeps_the_original() {
        for (kind, under_type, expected) in
            [(SecurityType::Forex, "CASH", 2), (SecurityType::Index, "STK", 1)]
        {
            let shared = SharedState::new();
            let contract = parent(&shared, under_type);
            shared.reference.cache_contract_definition(ContractDefinition {
                con_id: 2,
                sec_type: kind,
                exchange: "IDEALPRO".into(),
                ..ContractDefinition::default()
            });
            assert!(
                matches!(select(&shared, &contract), Selection::Ready(proxy) if proxy.con_id == expected)
            );
        }
    }

    #[test]
    fn an_overnight_underlying_requires_its_overnight_definition() {
        let shared = SharedState::new();
        let mut contract = parent(&shared, "STK");
        contract.exchange = "IBEOS".into();
        shared.reference.cache_contract_definition(ContractDefinition {
            con_id: 2,
            sec_type: SecurityType::Stock,
            exchange: "SMART".into(),
            ..ContractDefinition::default()
        });
        assert!(
            matches!(select(&shared, &contract), Selection::Resolve { con_id: 2, exchange } if exchange == "OVERNIGHT")
        );
    }
}
