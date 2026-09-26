use crate::control::contracts::{MarketRule, PriceIncrement, SecurityType, parse_market_rules};

#[derive(Clone, Copy, Debug)]
pub(crate) struct ComboRuleTerms {
    pub smart: bool,
    pub delta_neutral: bool,
    pub uses_confirmation_rounding: bool,
    pub negative_prices: bool,
}

fn minimum_tick(rule: &MarketRule) -> f64 {
    rule.price_increments.iter().map(|step| step.increment).fold(f64::MAX, f64::min)
}

fn size_increment(rule: &MarketRule) -> i32 {
    let Some(step) = rule.size_increments.first() else {
        return 1;
    };
    let decimal = step.increment.to_string();
    let Some((whole, fraction)) = decimal.split_once('.') else {
        return decimal.parse::<i64>().unwrap_or(0) as i32;
    };
    let Ok(numerator) = format!("{whole}{fraction}").parse::<i64>() else {
        return 0;
    };
    let denominator = 10_i64.checked_pow(fraction.len() as u32).unwrap_or(i64::MAX);
    let divisor =
        crate::control::attached_combos::gcd(numerator.unsigned_abs(), denominator as u64);
    (numerator / divisor as i64) as i32
}

fn same_magnifier(legs: &[(SecurityType, Option<MarketRule>)]) -> bool {
    let mut magnifiers =
        legs.iter().filter_map(|(_, rule)| rule.as_ref().map(|r| r.price_magnifier));
    magnifiers.next().is_none_or(|first| magnifiers.all(|next| next == first))
}

fn leg_rule(
    legs: &[(SecurityType, Option<MarketRule>)],
    delta_neutral: bool,
) -> Option<MarketRule> {
    let same = same_magnifier(legs);
    let mut selected: Option<&MarketRule> = None;
    for (kind, rule) in legs {
        if delta_neutral && !matches!(kind, SecurityType::Option | SecurityType::FutureOption) {
            continue;
        }
        let rule = rule.as_ref()?;
        if !same && rule.price_magnifier == 1 {
            return Some(rule.clone());
        }
        if selected.is_none_or(|previous| {
            minimum_tick(previous) > minimum_tick(rule)
                || size_increment(previous) > size_increment(rule)
        }) {
            selected = Some(rule);
        }
    }
    selected.cloned()
}

fn set_tick(rule: &mut MarketRule, tick: f64) {
    rule.price_increments = vec![PriceIncrement { low_edge: -f64::MAX, increment: tick }];
    rule.negative_prices = true;
}

/// A combination's price rule follows its legs and any stated confirmation terms.
pub(crate) fn effective_rule(
    base: &MarketRule,
    legs: &[(SecurityType, Option<MarketRule>)],
    terms: ComboRuleTerms,
    confirmation: Option<&[u8]>,
) -> MarketRule {
    let tags = confirmation.map(crate::control::contracts::tag_sequence).unwrap_or_default();
    let rounding = if !terms.smart && terms.uses_confirmation_rounding {
        match tags.iter().find(|(tag, _)| *tag == 6111).map(|(_, value)| value.as_str()) {
            Some("cmintick") => 1,
            Some("cleground") => 2,
            _ => 0,
        }
    } else {
        1
    };
    if rounding == 0 {
        return base.clone();
    }
    if !terms.smart
        && !terms.delta_neutral
        && let Some(message) = confirmation
    {
        let mut rules = parse_market_rules(message);
        if rules.len() == 1 {
            return rules.remove(0);
        }
    }
    let mut selected = leg_rule(legs, terms.delta_neutral);
    if terms.smart
        && let Some(tick) = tags
            .iter()
            .find(|(tag, _)| *tag == 6871)
            .and_then(|(_, value)| value.parse::<f64>().ok())
    {
        let rule = selected.get_or_insert_with(|| base.clone());
        set_tick(rule, tick);
    }
    let Some(mut selected) = selected else {
        return base.clone();
    };
    if !terms.smart
        && !terms.delta_neutral
        && rounding == 2
        && minimum_tick(&selected) > minimum_tick(base)
    {
        selected = base.clone();
    }
    if !terms.smart && !terms.delta_neutral && rounding == 1 {
        selected.price_increments.truncate(1);
        selected.negative_prices = terms.negative_prices;
    } else {
        selected.negative_prices = true;
    }
    if selected.rule_id > 0 {
        selected.rule_id = -selected.rule_id;
    }
    if !same_magnifier(legs) {
        selected.price_magnifier = 1;
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i32, tick: f64, size: f64, magnifier: i32) -> MarketRule {
        MarketRule {
            rule_id: id,
            negative_prices: false,
            price_magnifier: magnifier,
            price_increments: vec![
                PriceIncrement { low_edge: 0.0, increment: tick },
                PriceIncrement { low_edge: 10.0, increment: tick * 2.0 },
            ],
            size_increments: vec![PriceIncrement { low_edge: 0.0, increment: size }],
            price_places: None,
            size_places: None,
        }
    }

    fn terms(smart: bool) -> ComboRuleTerms {
        ComboRuleTerms {
            smart,
            delta_neutral: false,
            uses_confirmation_rounding: true,
            negative_prices: false,
        }
    }

    #[test]
    fn mixed_magnifiers_prefer_the_unscaled_leg_and_reset_the_result() {
        let base = rule(1, 1.0, 1.0, 100);
        let legs = vec![
            (SecurityType::Future, Some(rule(2, 0.01, 1.0, 100))),
            (SecurityType::Future, Some(rule(3, 0.05, 1.0, 1))),
        ];
        let selected = effective_rule(&base, &legs, terms(true), None);
        assert_eq!(selected.rule_id, -3);
        assert_eq!(selected.price_magnifier, 1);
        assert!(selected.negative_prices);
        assert_eq!(selected.price_increments[0].increment, 0.05);
    }

    #[test]
    fn a_smaller_size_increment_can_select_a_larger_price_increment() {
        let base = rule(1, 1.0, 1.0, 1);
        let legs = vec![
            (SecurityType::Stock, Some(rule(2, 0.01, 100.0, 1))),
            (SecurityType::Option, Some(rule(3, 0.05, 1.0, 1))),
        ];
        assert_eq!(effective_rule(&base, &legs, terms(true), None).rule_id, -3);
    }

    #[test]
    fn fractional_size_steps_compare_their_reduced_numerators() {
        let base = rule(1, 1.0, 1.0, 1);
        let legs = vec![
            (SecurityType::Stock, Some(rule(2, 0.01, 2.5, 1))),
            (SecurityType::Option, Some(rule(3, 0.05, 3.0, 1))),
        ];
        assert_eq!(effective_rule(&base, &legs, terms(true), None).rule_id, -3);
    }

    #[test]
    fn missing_leg_rules_preserve_the_contract_rule() {
        let base = rule(1, 1.0, 1.0, 100);
        let legs =
            vec![(SecurityType::Stock, Some(rule(2, 0.01, 1.0, 1))), (SecurityType::Option, None)];
        let selected = effective_rule(&base, &legs, terms(true), None);
        assert_eq!(selected.rule_id, 1);
        assert!(!selected.negative_prices);
    }

    #[test]
    fn direct_confirmation_controls_rounding_and_can_supply_the_whole_rule() {
        let base = rule(1, 0.1, 1.0, 1);
        let legs = vec![(SecurityType::Future, Some(rule(2, 0.5, 1.0, 1)))];
        assert_eq!(effective_rule(&base, &legs, terms(false), None).rule_id, 1);
        let minimum = effective_rule(&base, &legs, terms(false), Some(b"6111=cmintick\x01"));
        assert_eq!(minimum.price_increments.len(), 1);
        assert_eq!(minimum.price_increments[0].increment, 0.5);
        assert!(!minimum.negative_prices);
        let leg_rounding = effective_rule(&base, &legs, terms(false), Some(b"6111=cleground\x01"));
        assert_eq!(leg_rounding.rule_id, -1);
        assert!(leg_rounding.negative_prices);
        let full = effective_rule(&base, &legs, terms(false), Some(
            b"6111=cleground\x016019=1\x016031=9\x016020=0\x016021=100\x016026=1\x016023=0\x016027=0.25\x01"
        ));
        assert_eq!(full.rule_id, 9);
        assert_eq!(full.price_magnifier, 100);
        assert_eq!(full.price_increments[0].increment, 0.25);
    }

    #[test]
    fn a_stated_minimum_tick_replaces_the_price_ladder() {
        let base = rule(1, 1.0, 1.0, 1);
        let legs = vec![(SecurityType::Option, Some(rule(2, 0.1, 1.0, 1)))];
        let stated = effective_rule(&base, &legs, terms(true), Some(b"6871=0.01\x01"));
        assert_eq!(stated.price_increments.len(), 1);
        assert_eq!(stated.price_increments[0].increment, 0.01);
    }

    #[test]
    fn delta_neutral_prices_use_option_rules_and_keep_all_price_bands() {
        let base = rule(1, 1.0, 1.0, 100);
        let legs = vec![
            (SecurityType::Forex, Some(rule(2, 0.0001, 1.0, 1))),
            (SecurityType::Option, Some(rule(3, 0.05, 1.0, 100))),
        ];
        let mut terms = terms(false);
        terms.delta_neutral = true;
        terms.uses_confirmation_rounding = false;
        let selected = effective_rule(&base, &legs, terms, None);
        assert_eq!(selected.rule_id, -3);
        assert_eq!(selected.price_magnifier, 1);
        assert_eq!(selected.price_increments.len(), 2);
        assert!(selected.negative_prices);
    }
}
