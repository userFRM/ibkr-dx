//! Requests used when attached orders need combination definitions and prices.

use super::contracts::{ContractDefinition, SecurityType};
use crate::types::model::Contract;

pub(crate) fn combo_type(kinds: impl IntoIterator<Item = SecurityType>) -> i32 {
    let mask = kinds.into_iter().fold(0_u8, |mask, kind| {
        mask | match kind {
            SecurityType::Option => 1,
            SecurityType::Stock => 2,
            SecurityType::Future => 4,
            SecurityType::FutureOption => 8,
            SecurityType::Forex => 16,
            SecurityType::Commodity => 32,
            SecurityType::Warrant => 64,
            SecurityType::Cfd => 128,
            _ => 0,
        }
    });
    match mask {
        mask if mask & 128 != 0 => 14,
        mask if mask & 64 != 0 => 12,
        mask if mask & 32 != 0 => 13,
        1 => 0,
        2 => 9,
        3 => 1,
        4 => 2,
        5 => 4,
        6 => 3,
        7 => 5,
        8 => 7,
        12 => 8,
        17 => 11,
        _ => -1,
    }
}

/// The price type a combination is confirmed under: 2 for a stock and option
/// combination on one symbol whose option routes to ISE, else 0.
pub(crate) fn price_mode(contract: &Contract, legs: &[ContractDefinition]) -> i32 {
    let kind = combo_type(legs.iter().map(|leg| leg.sec_type.clone()));
    let same_symbol =
        legs.first().is_some_and(|first| legs.iter().all(|leg| leg.symbol == first.symbol));
    if !contract.combo_legs.is_empty()
        && legs.len() > 1
        && same_symbol
        && matches!(kind, 1 | 3 | 5)
        && legs.iter().any(|leg| leg.sec_type == SecurityType::Stock)
        && legs
            .iter()
            .position(|leg| leg.sec_type == SecurityType::Option)
            .is_some_and(|index| contract.combo_legs[index].exchange == "ISE")
    {
        2
    } else {
        0
    }
}

pub(crate) fn type_code(kind: i32) -> char {
    match kind {
        0 => 'o',
        1 => 's',
        2 => 'f',
        3 => 'u',
        4 => 't',
        5 => 'k',
        7 => 'y',
        8 => 'r',
        9 => 'z',
        10 => 'p',
        11 => 'c',
        12 => 'w',
        13 => 'm',
        14 => 'd',
        _ => '?',
    }
}

pub(crate) fn confirmation_request(
    request_key: &str,
    exchange: &str,
    contract: &Contract,
    delta_neutral: bool,
    legs: &[ContractDefinition],
    multiplier: Option<f64>,
    include_exchanges: bool,
) -> Vec<(u32, String)> {
    let kind = combo_type(legs.iter().map(|leg| leg.sec_type.clone()));
    let mut fields = vec![
        (6040, "7".into()),
        (320, request_key.into()),
        (207, super::contracts::exchange_to_fix(exchange).into()),
        (6134, kind.to_string()),
    ];
    if contract.combo_legs.is_empty() {
        return fields;
    }
    if let Some(multiplier) = multiplier
        && !matches!(kind, 2 | 3 | 4 | 5 | 7 | 8 | 10)
    {
        fields.push((231, multiplier.to_string()));
    }
    fields.push((6079, contract.combo_legs.len().to_string()));
    let gcd = if contract.combo_legs.len() < 2 { 1 } else { legs_divisor(&contract.combo_legs) };
    let mut ordered = std::collections::BTreeMap::new();
    for leg in &contract.combo_legs {
        ordered.insert(leg.con_id, leg);
    }
    for (con_id, leg) in ordered {
        let ratio =
            if gcd > 1 { i64::from(leg.ratio) / i64::from(gcd) } else { i64::from(leg.ratio) };
        fields.extend([
            (6080, con_id.to_string()),
            (6081, ratio.to_string()),
            (6082, if leg.action == "BUY" { "1" } else { "0" }.into()),
        ]);
        if include_exchanges {
            fields.push((
                616,
                if matches!(leg.exchange.as_str(), "SMART" | "ZERO") {
                    String::new()
                } else {
                    super::contracts::exchange_to_fix(&leg.exchange).into()
                },
            ));
        }
    }
    fields.push((6175, price_mode(contract, legs).to_string()));
    if let Some(neutral) = &contract.delta_neutral_contract
        && delta_neutral
    {
        fields.extend([
            (6147, "1".into()),
            (6148, neutral.delta.to_string()),
            (6149, neutral.price.to_string()),
            (6150, neutral.con_id.to_string()),
        ]);
    }
    fields
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ComboRules {
    pub allowed: String,
    pub dn_only: String,
}

pub(crate) fn rules_exchange(exchange: &str) -> &str {
    match exchange {
        "SMART" | "BEST" | "ZERO" => "IBCX",
        other => other,
    }
}

pub(crate) fn rules_request(exchange: &str) -> Vec<(u32, String)> {
    let exchange = rules_exchange(exchange);
    vec![(6040, "153".into()), (8066, "1".into()), (6004, exchange.into())]
}

pub(crate) fn rules_response(message: &[u8]) -> Vec<(String, ComboRules)> {
    let tags = super::contracts::tag_sequence(message);
    if !tags.iter().any(|(tag, value)| *tag == 6040 && value == "154")
        || !tags.iter().any(|(tag, value)| {
            *tag == 8066 && value.parse::<i32>().is_ok_and(|count| count > 0 && count != i32::MAX)
        })
    {
        return Vec::new();
    }
    let mut rules = Vec::new();
    for (tag, value) in tags {
        match tag {
            6004 => rules.push((value, ComboRules::default())),
            8067 => {
                if let Some((_, rule)) = rules.last_mut() {
                    rule.allowed = value;
                }
            }
            8068 => {
                if let Some((_, rule)) = rules.last_mut() {
                    rule.dn_only =
                        value.split_once("dnonly=").map_or(String::new(), |(_, text)| {
                            text.chars().filter(|value| "osfutkyrzpcwmd".contains(*value)).collect()
                        });
                }
            }
            _ => {}
        }
    }
    rules
}

/// The greatest common divisor of two numbers.
pub(crate) fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The largest number every leg's ratio divides by.
pub(crate) fn legs_divisor(legs: &[crate::types::model::ComboLeg]) -> u32 {
    legs.iter().fold(0, |divisor, leg| gcd(divisor, u64::from(leg.ratio.unsigned_abs()))) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::model::ComboLeg;

    #[test]
    fn exchange_rules_stay_with_the_named_exchange() {
        let rules = rules_response(
            b"6040=154\x018066=2\x016004=CME\x016111=cleground\x016004=CBOE\x018067=o\x01",
        );
        assert!(rules[0].1.allowed.is_empty());
        assert_eq!(rules[1].1.allowed, "o");
    }

    #[test]
    fn confirmation_sorts_and_simplifies_legs_without_order_attributes() {
        let contract = Contract {
            combo_legs: vec![
                ComboLeg { con_id: 12, ratio: 4, action: "SELL".into(), ..Default::default() },
                ComboLeg { con_id: 11, ratio: 2, action: "BUY".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(
            confirmation_request(
                "AC.1",
                "CME",
                &contract,
                false,
                &[ContractDefinition { sec_type: SecurityType::Future, ..Default::default() }],
                None,
                false
            ),
            vec![
                (6040, "7".into()),
                (320, "AC.1".into()),
                (207, "CME".into()),
                (6134, "2".into()),
                (6079, "2".into()),
                (6080, "11".into()),
                (6081, "1".into()),
                (6082, "1".into()),
                (6080, "12".into()),
                (6081, "2".into()),
                (6082, "0".into()),
                (6175, "0".into()),
            ]
        );
    }

    #[test]
    fn combo_types_keep_mixed_legs_and_precedence() {
        use SecurityType::*;
        for (kinds, expected) in [
            (vec![Option], 0),
            (vec![Stock], 9),
            (vec![Stock, Option], 1),
            (vec![Future], 2),
            (vec![Future, Option], 4),
            (vec![Future, Stock], 3),
            (vec![Future, Stock, Option], 5),
            (vec![FutureOption], 7),
            (vec![Future, FutureOption], 8),
            (vec![Forex, Option], 11),
            (vec![Commodity, Warrant], 12),
            (vec![Commodity, Stock], 13),
            (vec![Cfd, Warrant], 14),
            (vec![Bond], -1),
        ] {
            assert_eq!(combo_type(kinds), expected);
        }
        assert_eq!(type_code(2), 'f');
    }

    #[test]
    fn combo_rule_answers_keep_each_exchange_and_explicit_empty_rules() {
        assert_eq!(
            rules_request("SMART"),
            vec![(6040, "153".into()), (8066, "1".into()), (6004, "IBCX".into())]
        );
        let answer =
            b"35=U\x016040=154\x018066=2\x016004=CME\x018068=dnonly=of\x016004=CBOE\x018067=o\x01";
        assert_eq!(
            rules_response(answer),
            vec![
                ("CME".into(), ComboRules { dn_only: "of".into(), ..Default::default() }),
                ("CBOE".into(), ComboRules { allowed: "o".into(), ..Default::default() })
            ]
        );
        assert!(rules_response(b"6040=153\x016004=CME\x01").is_empty());
        assert_eq!(
            rules_response(b"6040=154\x018066=1\x016004=CME\x018068=dnonly=?Fo\x01")[0].1.dn_only,
            "o"
        );
    }
    #[test]
    fn attached_combo_confirmation_keeps_single_ratios_and_stated_exchanges() {
        let contract = Contract {
            combo_legs: vec![ComboLeg {
                con_id: 7,
                ratio: 4,
                action: "SELL".into(),
                exchange: "ZERO".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let definitions =
            [ContractDefinition { sec_type: SecurityType::Option, ..Default::default() }];
        let fields =
            confirmation_request("AC.2", "SMART", &contract, false, &definitions, Some(1.0), true);
        assert!(fields.contains(&(6081, "4".into())));
        assert!(fields.contains(&(231, "1".into())));
        assert!(fields.contains(&(616, String::new())));
        assert!(!fields.iter().any(|(tag, _)| matches!(tag, 6087 | 654 | 6718)));
        let fields =
            confirmation_request("AC.4", "CBOE", &Contract::default(), false, &[], Some(1.0), true);
        assert_eq!(
            fields,
            vec![
                (6040, "7".into()),
                (320, "AC.4".into()),
                (207, "CBOE".into()),
                (6134, "-1".into())
            ]
        );
    }

    #[test]
    fn attached_combo_confirmation_uses_the_first_option_for_ise_price_mode() {
        let mut contract = Contract {
            combo_legs: vec![
                ComboLeg {
                    con_id: 1,
                    ratio: 100,
                    action: "BUY".into(),
                    exchange: "SMART".into(),
                    ..Default::default()
                },
                ComboLeg {
                    con_id: 2,
                    ratio: 1,
                    action: "SELL".into(),
                    exchange: "ISE".into(),
                    ..Default::default()
                },
                ComboLeg {
                    con_id: 3,
                    ratio: 1,
                    action: "BUY".into(),
                    exchange: "CBOE".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut definitions = vec![
            ContractDefinition {
                symbol: "ABC".into(),
                sec_type: SecurityType::Stock,
                ..Default::default()
            },
            ContractDefinition {
                symbol: "ABC".into(),
                sec_type: SecurityType::Option,
                ..Default::default()
            },
            ContractDefinition {
                symbol: "ABC".into(),
                sec_type: SecurityType::Option,
                ..Default::default()
            },
        ];
        let mode = |contract: &Contract, definitions: &[ContractDefinition]| {
            confirmation_request("AC.3", "CBOE", contract, false, definitions, Some(1.0), false)
                .into_iter()
                .find(|(tag, _)| *tag == 6175)
                .unwrap()
                .1
        };
        assert_eq!(mode(&contract, &definitions), "2");
        contract.combo_legs[1].exchange = "CBOE".into();
        contract.combo_legs[2].exchange = "ISE".into();
        assert_eq!(mode(&contract, &definitions), "0");
        contract.combo_legs[1].exchange = "ISE".into();
        definitions[2].symbol = "OTHER".into();
        assert_eq!(mode(&contract, &definitions), "0");
    }
}
