//! The aggregate destinations stated at logon.

use super::contracts::SecurityType;

pub(crate) fn smart_combo_ids(
    usd: i32,
    stated: Option<&str>,
) -> std::collections::HashMap<String, i32> {
    let usd = if usd == 0 && stated.is_none() { 28_812_380 } else { usd };
    let mut ids = std::collections::HashMap::new();
    for pair in stated.unwrap_or_default().split(',').filter(|pair| !pair.is_empty()) {
        let fields: Vec<_> = pair.trim_end_matches(':').split(':').collect();
        if fields.len() == 2
            && let Ok(id) = fields[1].parse::<i32>()
        {
            ids.insert(fields[0].to_string(), id);
        }
    }
    ids.entry("USD".into()).or_insert(usd);
    ids
}

pub(crate) fn carries_smart_leg(raw: &str, exchange: &str, security_type: &str) -> bool {
    let security_type = security_type.to_ascii_uppercase();
    if exchange == "SMART" || exchange == "IBKRATS" && security_type != "CASH" {
        return true;
    }
    let mut matches = 0;
    for entry in raw.split(';').filter(|entry| !entry.is_empty()) {
        let mut fields = entry.split(',').filter(|field| !field.is_empty());
        if fields.next().and_then(|id| id.parse::<i32>().ok()).is_none() {
            continue;
        }
        let Some(name) = fields.next() else { continue };
        let Some(kind) = fields.next() else { continue };
        let kind = kind.to_ascii_uppercase();
        let kind = if kind == "COMB" { "BAG" } else { &kind };
        if !matches!(name, "SMART" | "BEST" | "*")
            || SecurityType::from_fix(kind) != SecurityType::from_fix(&security_type)
        {
            continue;
        }
        if exchange.is_empty() || fields.any(|component| component == exchange) {
            matches += 1;
        }
    }
    matches == 1
}

pub(crate) fn preferred_market(raw: &str, group: i32) -> Option<String> {
    if group <= 0 {
        return None;
    }
    raw.split(';')
        .filter_map(|entry| {
            let mut fields = entry.split(',').filter(|field| !field.is_empty());
            let id = fields.next()?.parse::<i32>().ok()?;
            let market = fields.next()?;
            (id == group).then(|| super::contracts::exchange_from_fix(market).to_string())
        })
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preferred_market_uses_the_first_matching_positive_group() {
        assert_eq!(preferred_market("4,ARCA,CS,NYSE;4,SMART,CS,NYSE", 4).as_deref(), Some("ARCA"));
        assert_eq!(preferred_market("0,ARCA,CS,NYSE", 0), None);
    }

    #[test]
    fn smart_combo_contracts_keep_stated_currency_and_absence_distinct() {
        assert_eq!(smart_combo_ids(0, None).get("USD"), Some(&28_812_380));
        assert_eq!(smart_combo_ids(0, Some("")).get("USD"), Some(&0));
        assert_eq!(smart_combo_ids(19, None).get("USD"), Some(&19));
        let ids = smart_combo_ids(19, Some("EUR:5,USD:7,EUR:6,GBP:bad,CAD:8::,CHF:1:2"));
        assert_eq!(ids.get("USD"), Some(&7));
        assert_eq!(ids.get("EUR"), Some(&6));
        assert_eq!(ids.get("CAD"), Some(&8));
        assert!(!ids.contains_key("GBP"));
        assert!(!ids.contains_key("CHF"));
    }

    #[test]
    fn smart_and_stock_internal_routes_need_no_aggregate_map() {
        assert!(carries_smart_leg("", "SMART", "CASH"));
        assert!(carries_smart_leg("", "IBKRATS", "STK"));
        assert!(!carries_smart_leg("", "IBKRATS", "CASH"));
        assert!(!carries_smart_leg("", "ARCA", "STK"));
    }

    #[test]
    fn one_aggregate_must_match_both_type_and_component() {
        let raw = "1,SMART,CS,ARCA,ISLAND;2,SMART,OPT,BOX;3,DIRECT,STK,NYSE";
        assert!(carries_smart_leg(raw, "ARCA", "STK"));
        assert!(carries_smart_leg(raw, "BOX", "OPT"));
        assert!(!carries_smart_leg(raw, "ARCA", "OPT"));
        assert!(!carries_smart_leg(raw, "NYSE", "STK"));
        assert!(!carries_smart_leg(raw, "arca", "STK"));
        assert!(carries_smart_leg("0,BEST,stk,,ARCA", "ARCA", "STK"));
        assert!(carries_smart_leg("1,*,COMB,COMBOEX", "COMBOEX", "BAG"));
        assert!(!carries_smart_leg("1,SMART,*,ARCA", "ARCA", "STK"));
        assert!(!carries_smart_leg("broken,SMART,STK,ARCA", "ARCA", "STK"));
    }

    #[test]
    fn ambiguous_aggregate_membership_is_not_smart() {
        let raw = "1,SMART,STK,ARCA;2,SMART,STK,ARCA";
        assert!(!carries_smart_leg(raw, "ARCA", "STK"));
        assert!(!carries_smart_leg(raw, "", "STK"));
        assert!(carries_smart_leg("1,SMART,STK,ARCA", "", "STK"));
    }
}
