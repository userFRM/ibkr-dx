//! Typed preset values and the selection used by attached orders.

use super::order_presets::PresetValues;

/// The unit of a price or trailing offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PriceUnit {
    #[default]
    Amount,
    Ticks,
    Percent,
    Unavailable,
}

impl PriceUnit {
    /// A unit as a preset states it: its number, or else its label.
    // ponytail: ASCII digits; a gateway also reads other scripts' digits.
    fn from_wire(value: &str) -> Self {
        match value.parse::<i32>() {
            Ok(0) => Self::Amount,
            Ok(1) => Self::Ticks,
            Ok(100) => Self::Percent,
            Ok(_) => Self::Unavailable,
            Err(_) => match value {
                "amt" => Self::Amount,
                "ticks" => Self::Ticks,
                "%" => Self::Percent,
                _ => Self::Unavailable,
            },
        }
    }
}

/// A price selector and its offset. Selectors retain their numeric values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PriceSpec {
    pub price_type: i32,
    pub offset: f64,
    pub unit: PriceUnit,
    pub use_parent_trade_price: bool,
}

impl PriceSpec {
    fn for_tag(tag: i32) -> Option<Self> {
        let (price_type, offset) = match tag {
            4077 => (11, 1.0),
            4078 => (11, -1.0),
            4079 | 4082 => (3, 0.0),
            4080 | 4081 => (3, 1.0),
            _ => return None,
        };
        Some(Self { price_type, offset, unit: PriceUnit::Amount, use_parent_trade_price: false })
    }
}

/// The preset fields used to construct attached children.
#[derive(Clone, Debug, PartialEq)]
pub struct AttachedPreset {
    /// The original order-type value, including the `-1` none value.
    pub profit_order_type: String,
    pub profit_offset: f64,
    pub scale_initial_size: i32,
    pub scale_subsequent_size: i32,
    pub scale_price_increment: f64,
    /// The stop selection, from 0 (none) through 11 (trailing stop limit).
    pub auto_stop: i32,
    pub auto_attach_stop_loss: bool,
    pub auto_attach_profit_taker: bool,
    pub limit: PriceSpec,
    pub stop: PriceSpec,
    pub stop_limit: PriceSpec,
    pub adjusted_trigger: PriceSpec,
    pub adjusted_stop: PriceSpec,
    pub adjusted_stop_limit: PriceSpec,
    pub trailing_amount: f64,
    pub trailing_unit: PriceUnit,
    pub adjusted_trailing_amount: f64,
    pub adjusted_trailing_unit: PriceUnit,
    pub primary_trailing_amount: f64,
    pub primary_trailing_unit: PriceUnit,
    pub primary_outside_rth: bool,
    pub primary_use_price_mgmt_algo: bool,
    pub primary_auto_cancel_parent: bool,
    /// A stated fractional share default selects the parent's exact quantity.
    pub profit_uses_exact_parent_quantity: bool,
}

impl Default for AttachedPreset {
    fn default() -> Self {
        Self {
            profit_order_type: "-1".into(),
            profit_offset: 0.0,
            scale_initial_size: i32::MAX,
            scale_subsequent_size: i32::MAX,
            scale_price_increment: f64::MAX,
            auto_stop: 0,
            auto_attach_stop_loss: false,
            auto_attach_profit_taker: false,
            limit: PriceSpec::for_tag(4077).unwrap(),
            stop: PriceSpec::for_tag(4078).unwrap(),
            stop_limit: PriceSpec::for_tag(4079).unwrap(),
            adjusted_trigger: PriceSpec::for_tag(4080).unwrap(),
            adjusted_stop: PriceSpec::for_tag(4081).unwrap(),
            adjusted_stop_limit: PriceSpec::for_tag(4082).unwrap(),
            trailing_amount: f64::MAX,
            trailing_unit: PriceUnit::Amount,
            adjusted_trailing_amount: f64::MAX,
            adjusted_trailing_unit: PriceUnit::Amount,
            primary_trailing_amount: 1.0,
            primary_trailing_unit: PriceUnit::Amount,
            primary_outside_rth: false,
            primary_use_price_mgmt_algo: false,
            primary_auto_cancel_parent: false,
            profit_uses_exact_parent_quantity: false,
        }
    }
}

/// Whether an answer names a price specification it cannot: an answer a
/// gateway cannot read, which it holds as the key alone, with every default.
pub(crate) fn is_unreadable(fields: &[(u32, String)]) -> bool {
    fields.iter().any(|(tag, value)| {
        *tag == 4084 && !value.is_empty() && !value.contains('=') && value.parse::<i32>().is_err()
    })
}

impl AttachedPreset {
    /// Read independent preset defaults followed by the stated field values.
    /// A repeated price specification starts from its own defaults each time.
    /// An error the answer states does not change what it states. `None` where
    /// the quantity type is not one a preset has.
    pub fn read(values: &PresetValues) -> Option<Self> {
        let mut preset = Self::default();
        let mut shares = true;
        let mut precise_quantity = None;
        for (tag, value) in &values.fields {
            match tag {
                4016 => preset.primary_trailing_amount = read_double(value),
                4039 => preset.primary_trailing_unit = PriceUnit::from_wire(value),
                4041 => {
                    shares = match value.as_str() {
                        "Shares" => true,
                        "Amount" | "AllAvailableSize" | "Empty" => false,
                        _ => return None,
                    };
                }
                6433 => preset.primary_outside_rth = value == "1",
                8339 => preset.primary_use_price_mgmt_algo = value == "1",
                6965 => preset.primary_auto_cancel_parent = value == "1",
                4200 => precise_quantity = Some(read_double(&value.replace(',', ""))),
                _ => {}
            }
        }
        if is_unreadable(&values.fields) {
            return Some(Self::default());
        }
        let fields: Vec<_> =
            values.fields.iter().filter(|(tag, _)| (4066..=4088).contains(tag)).collect();
        let mut index = 0;
        while let Some((tag, value)) = fields.get(index).copied() {
            if value.is_empty() || value.contains('=') {
                index += 1;
                continue;
            }
            match tag {
                4066 => preset.profit_offset = read_double(value),
                4067 => preset.scale_initial_size = read_integer(value),
                4068 => preset.scale_subsequent_size = read_integer(value),
                4069 => preset.scale_price_increment = read_double(value),
                4070 => preset.trailing_amount = read_double(value),
                4071 => preset.trailing_unit = PriceUnit::from_wire(value),
                4072 => preset.adjusted_trailing_amount = read_double(value),
                4073 => preset.adjusted_trailing_unit = PriceUnit::from_wire(value),
                4074 => preset.auto_attach_stop_loss = value == "1",
                4075 => preset.auto_attach_profit_taker = value == "1",
                4076 => {
                    let value = read_integer(value);
                    preset.auto_stop = if (0..=11).contains(&value) { value } else { 0 };
                }
                4083 => preset.profit_order_type.clone_from(value),
                4084 => {
                    let identity = value.parse::<i32>().unwrap_or_default();
                    let mut spec = PriceSpec::for_tag(identity);
                    while let Some((member, value)) = fields.get(index + 1).copied() {
                        if !(4085..=4088).contains(member) {
                            break;
                        }
                        index += 1;
                        if value.is_empty() || value.contains('=') {
                            continue;
                        }
                        if let Some(spec) = &mut spec {
                            match member {
                                4085 => spec.offset = read_double(value),
                                4086 => spec.unit = PriceUnit::from_wire(value),
                                4087 => spec.use_parent_trade_price = value == "1",
                                4088 => {
                                    let value = read_integer(value);
                                    spec.price_type =
                                        if (0..=38).contains(&value) { value } else { 3 };
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                    if let Some(spec) = spec {
                        match identity {
                            4077 => preset.limit = spec,
                            4078 => preset.stop = spec,
                            4079 => preset.stop_limit = spec,
                            4080 => preset.adjusted_trigger = spec,
                            4081 => preset.adjusted_stop = spec,
                            4082 => preset.adjusted_stop_limit = spec,
                            _ => unreachable!(),
                        }
                    }
                }
                _ => {}
            }
            index += 1;
        }
        let key = PresetKey::parse(&values.key);
        preset.profit_uses_exact_parent_quantity =
            matches!(key.security_type.as_deref(), Some("STK" | "CRYPTO" | "FUND"))
                && shares
                && precise_quantity.is_some_and(|quantity| {
                    quantity.is_finite() && quantity != f64::MAX && quantity.fract() != 0.0
                });
        Some(preset)
    }
}

/// A number as a preset states one: an optional sign, digits, a point and an
/// exponent, or `NaN` and `Infinity`. Anything else is unset.
fn read_double(value: &str) -> f64 {
    let value = value.trim_matches(|character| character <= ' ');
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    let written = matches!(unsigned, "NaN" | "Infinity")
        || unsigned.bytes().all(|byte| byte.is_ascii_digit() || b".+-eE".contains(&byte));
    if written { value.parse().unwrap_or(f64::MAX) } else { f64::MAX }
}

fn read_integer(value: &str) -> i32 {
    value.parse().unwrap_or(i32::MAX)
}

/// The selectors encoded in a preset key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PresetKey {
    pub smart_routing: Option<String>,
    pub security_type: Option<String>,
    pub universe: Option<String>,
    pub symbol: Option<String>,
    pub mifid_selector: Option<String>,
    pub allocation_selector: Option<String>,
}

impl PresetKey {
    /// Read as a gateway reads a key: `&`-separated `name=value` pairs, each
    /// value decoded, the last of a name kept, anything else passed over.
    pub fn parse(value: &str) -> Self {
        let mut key = Self::default();
        for part in value.split('&') {
            // The empty values a trailing `=` leaves are dropped, not read.
            let Some((name, value)) = part.trim_end_matches('=').split_once('=') else {
                continue;
            };
            if value.contains('=') {
                continue;
            }
            let target = match name {
                "sr" => &mut key.smart_routing,
                "s" => &mut key.security_type,
                "u" => &mut key.universe,
                "tc" => &mut key.symbol,
                "m2" => &mut key.mifid_selector,
                "f" => &mut key.allocation_selector,
                _ => continue,
            };
            *target = Some(decode_key_value(value));
        }
        key
    }

    /// The key as a gateway writes it into a request: its selectors in a fixed
    /// order, each escaped, whatever the listed key's order and spelling.
    pub fn selector(&self) -> String {
        [
            ("sr", &self.smart_routing),
            ("s", &self.security_type),
            ("u", &self.universe),
            ("tc", &self.symbol),
            ("m2", &self.mifid_selector),
            ("f", &self.allocation_selector),
        ]
        .into_iter()
        .filter_map(|(name, value)| {
            let value = value.as_deref().filter(|value| !value.is_empty())?;
            Some(format!("{name}={}", encode_key_value(value)))
        })
        .collect::<Vec<_>>()
        .join("&")
    }

    /// A key naming at least one selector, none of them a smart-routing,
    /// allocation, MiFID or submitter preset.
    fn is_order_preset(&self) -> bool {
        let named = [
            &self.smart_routing,
            &self.security_type,
            &self.universe,
            &self.symbol,
            &self.mifid_selector,
            &self.allocation_selector,
        ];
        named.iter().any(|selector| selector.is_some())
            && self.smart_routing.is_none()
            && self.mifid_selector.is_none()
            && self.allocation_selector.is_none()
            && self.symbol.as_deref() != Some("__SA__")
    }
}

/// The key a request for a listed preset's values names.
pub fn request_selector(listed: &str) -> String {
    PresetKey::parse(listed).selector()
}

/// The attributes in a list entry are distinct from its selectors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PresetAttributes {
    pub active: bool,
    pub strategy: bool,
}

impl PresetAttributes {
    pub fn parse(value: &str) -> Self {
        let mut attributes = Self::default();
        for part in value.split('&') {
            let Some((name, value)) = part.split_once('=') else { continue };
            if value.is_empty() || value.contains('=') {
                continue;
            }
            match name {
                "a" => attributes.active = value == "1",
                "st" => attributes.strategy = value == "1",
                _ => {}
            }
        }
        attributes
    }
}

fn decode_key_value(value: &str) -> String {
    value.replace("%25", "%").replace("%20", " ").replace("%3D", "=").replace("%26", "&")
}

fn encode_key_value(value: &str) -> String {
    value.replace('%', "%25").replace(' ', "%20").replace('=', "%3D").replace('&', "%26")
}

/// The instrument fields used to select a preset.
#[derive(Clone, Copy, Debug, Default)]
pub struct PresetInstrument<'a> {
    pub security_type: &'a str,
    pub underlying_security_type: &'a str,
    pub symbol: &'a str,
    pub currency: &'a str,
}

/// Select the account entry for an instrument. No entry means type defaults.
/// Active child selection is enabled by the session's `PRESETS` feature.
pub fn select_preset_key<'a>(
    presets: &'a [(String, String, String)],
    instrument: PresetInstrument<'_>,
    presets_enabled: bool,
) -> Option<&'a str> {
    struct Entry<'a> {
        raw: &'a str,
        key: PresetKey,
        attributes: PresetAttributes,
    }
    let entries: Vec<_> = presets
        .iter()
        .map(|(key, attributes, _)| Entry {
            raw: key.as_str(),
            key: PresetKey::parse(key),
            attributes: PresetAttributes::parse(attributes),
        })
        .filter(|entry| entry.key.is_order_preset())
        .collect();
    let root = entries.iter().find(|entry| {
        entry.key.security_type.is_none()
            && entry.key.symbol.is_none()
            && matches!(entry.key.universe.as_deref(), None | Some("ANY"))
    });
    let security_type = if matches!(instrument.security_type, "BAG" | "PDC")
        || instrument.symbol == "IECombo"
    {
        "COMB"
    } else if instrument.security_type == "CFD" && instrument.underlying_security_type == "CASH" {
        "CASH"
    } else {
        instrument.security_type
    };
    if security_type.is_empty() {
        return root.map(|entry| entry.raw);
    }
    let type_entry = entries.iter().find(|entry| {
        entry.key.security_type.as_deref() == Some(security_type) && entry.key.symbol.is_none()
    });
    let mut children: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry.key.security_type.as_deref() == Some(security_type) && entry.key.symbol.is_some()
        })
        .collect();
    children.sort_by(|left, right| {
        right.attributes.strategy.cmp(&left.attributes.strategy).then_with(|| {
            left.key
                .symbol
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .cmp(&right.key.symbol.as_deref().unwrap_or_default().to_lowercase())
        })
    });
    let mut selected = None;
    if !instrument.symbol.is_empty() {
        if security_type == "CASH" && !instrument.currency.is_empty() {
            selected = children
                .iter()
                .copied()
                .find(|entry| entry.key.symbol.as_deref() == Some(instrument.currency));
        }
        if selected.is_none() {
            selected = children
                .iter()
                .copied()
                .find(|entry| entry.key.symbol.as_deref() == Some(instrument.symbol));
        }
    }
    if selected.is_none() {
        selected = type_entry.or(root);
        if presets_enabled && !selected.is_some_and(|entry| entry.attributes.active) {
            selected = children.iter().copied().find(|entry| entry.attributes.active).or(selected);
        }
    }
    if !matches!(security_type, "" | "*" | "UNK")
        && selected.is_some_and(|entry| entry.attributes.strategy && !entry.attributes.active)
    {
        selected = root;
    }
    selected.map(|entry| entry.raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(key: &str, fields: &[(u32, &str)]) -> PresetValues {
        PresetValues {
            request_key: "OPR.3".into(),
            key: key.into(),
            attributes: "v=1&a=1".into(),
            error: None,
            fields: fields.iter().map(|(tag, value)| (*tag, (*value).into())).collect(),
        }
    }

    fn list(entries: &[(&str, &str)]) -> Vec<(String, String, String)> {
        entries.iter().map(|(key, attrs)| ((*key).into(), (*attrs).into(), String::new())).collect()
    }

    fn stock() -> PresetInstrument<'static> {
        PresetInstrument {
            security_type: "STK",
            symbol: "ABC",
            currency: "USD",
            ..PresetInstrument::default()
        }
    }

    #[test]
    fn primary_attribute_flags_use_the_last_stated_boolean() {
        let preset =
            AttachedPreset::read(&values("s=STK", &[(6433, "1"), (8339, "1"), (6965, "1")]))
                .unwrap();
        assert!(preset.primary_outside_rth);
        assert!(preset.primary_use_price_mgmt_algo);
        assert!(preset.primary_auto_cancel_parent);
        let preset = AttachedPreset::read(&values(
            "s=STK",
            &[(6433, "1"), (8339, "1"), (6965, "1"), (6433, "0"), (8339, "true"), (6965, "0")],
        ))
        .unwrap();
        assert!(!preset.primary_outside_rth);
        assert!(!preset.primary_use_price_mgmt_algo);
        assert!(!preset.primary_auto_cancel_parent);
    }

    #[test]
    fn absent_fields_use_independent_attached_defaults() {
        let preset = AttachedPreset::read(&values("s=STK", &[])).unwrap();
        assert_eq!(preset, AttachedPreset::default());
        assert_eq!(preset.profit_order_type, "-1");
        assert!(!preset.auto_attach_profit_taker && !preset.auto_attach_stop_loss);
        assert_eq!((preset.limit.price_type, preset.limit.offset), (11, 1.0));
        assert_eq!((preset.stop.price_type, preset.stop.offset), (11, -1.0));
        assert_eq!((preset.stop_limit.price_type, preset.stop_limit.offset), (3, 0.0));
        assert_eq!((preset.adjusted_trigger.price_type, preset.adjusted_trigger.offset), (3, 1.0));
        assert_eq!((preset.adjusted_stop.price_type, preset.adjusted_stop.offset), (3, 1.0));
        assert_eq!(
            (preset.adjusted_stop_limit.price_type, preset.adjusted_stop_limit.offset),
            (3, 0.0)
        );
        assert_eq!(preset.primary_trailing_amount, 1.0);
        assert_eq!(preset.trailing_amount, f64::MAX);
        assert_eq!(preset.scale_initial_size, i32::MAX);
    }

    #[test]
    fn all_scalar_attachment_fields_and_primary_trail_are_read() {
        let preset = AttachedPreset::read(&values(
            "s=STK",
            &[
                (4066, "2.5"),
                (4067, "10"),
                (4068, "20"),
                (4069, "0.25"),
                (4070, "3"),
                (4071, "100"),
                (4072, "4"),
                (4073, "1"),
                (4074, "1"),
                (4075, "1"),
                (4076, "11"),
                (4083, "2"),
                (4016, "5"),
                (4039, "100"),
            ],
        ))
        .unwrap();
        assert_eq!(preset.profit_order_type, "2");
        assert_eq!(preset.profit_offset, 2.5);
        assert_eq!((preset.scale_initial_size, preset.scale_subsequent_size), (10, 20));
        assert_eq!(preset.scale_price_increment, 0.25);
        assert_eq!((preset.trailing_amount, preset.trailing_unit), (3.0, PriceUnit::Percent));
        assert_eq!(
            (preset.adjusted_trailing_amount, preset.adjusted_trailing_unit),
            (4.0, PriceUnit::Ticks)
        );
        assert_eq!(
            (preset.primary_trailing_amount, preset.primary_trailing_unit),
            (5.0, PriceUnit::Percent)
        );
        assert!(preset.auto_attach_stop_loss && preset.auto_attach_profit_taker);
        assert_eq!(preset.auto_stop, 11);
    }

    #[test]
    fn price_blocks_are_consecutive_and_repeated_blocks_restart_defaults() {
        let preset = AttachedPreset::read(&values(
            "s=STK",
            &[
                (4084, "4077"),
                (4085, "7"),
                (4086, "100"),
                (4087, "1"),
                (4088, "28"),
                (4084, "4077"),
                (4085, "2"),
                (4074, "1"),
                (4088, "0"),
                (4084, "4078"),
                (4085, "-0.5"),
                (4086, "1"),
                (4087, "1"),
                (4088, "37"),
                (4084, "4079"),
                (4088, "29"),
                (4084, "4080"),
                (4088, "4"),
                (4084, "4081"),
                (4088, "38"),
                (4084, "4082"),
                (4088, "12"),
            ],
        ))
        .unwrap();
        assert_eq!(preset.limit, PriceSpec { offset: 2.0, ..PriceSpec::for_tag(4077).unwrap() });
        assert_eq!(
            preset.stop,
            PriceSpec {
                price_type: 37,
                offset: -0.5,
                unit: PriceUnit::Ticks,
                use_parent_trade_price: true
            }
        );
        assert_eq!(preset.stop_limit.price_type, 29);
        assert_eq!(preset.adjusted_trigger.price_type, 4);
        assert_eq!(preset.adjusted_stop.price_type, 38);
        assert_eq!(preset.adjusted_stop_limit.price_type, 12);
    }

    #[test]
    fn invalid_scalar_values_use_their_sentinels_and_unknown_enums_use_none() {
        let preset = AttachedPreset::read(&values(
            "s=STK",
            &[
                (4066, "bad"),
                (4067, " 12"),
                (4076, "42"),
                (4074, "true"),
                (4071, "bad"),
                (4084, "4077"),
                (4085, "bad"),
                (4088, "99"),
                (4084, "9999"),
                (4085, "10"),
            ],
        ))
        .unwrap();
        assert_eq!(preset.profit_offset, f64::MAX);
        assert_eq!(preset.scale_initial_size, i32::MAX);
        assert_eq!(preset.auto_stop, 0);
        assert!(!preset.auto_attach_stop_loss);
        assert_eq!(preset.trailing_unit, PriceUnit::Unavailable);
        assert_eq!((preset.limit.offset, preset.limit.price_type), (f64::MAX, 3));
    }

    #[test]
    fn price_numbers_are_read_as_the_preset_writer_states_them() {
        for (stated, expected) in [
            (" \t+1.25\n", 1.25),
            ("-2.5", -2.5),
            ("1.0E-4", 0.0001),
            ("2e3", 2000.0),
            ("+Infinity", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
        ] {
            let preset = AttachedPreset::read(&values(
                "s=STK",
                &[(4016, stated), (4200, stated), (4084, "4077"), (4085, stated)],
            ))
            .unwrap();
            assert_eq!(preset.limit.offset.to_bits(), expected.to_bits(), "{stated}");
            assert_eq!(preset.primary_trailing_amount.to_bits(), expected.to_bits(), "{stated}");
        }
        for stated in ["NaN", "+NaN", "-NaN"] {
            assert!(read_double(stated).is_nan(), "{stated}");
        }
        for stated in
            ["inf", "infinity", "nan", "NaNf", "1.25D", "-2.5f", "0x1.8p1", "1e", "\u{a0}1\u{a0}"]
        {
            assert_eq!(read_double(stated), f64::MAX, "{stated}");
        }
    }

    #[test]
    fn precise_share_defaults_accept_the_same_numeric_syntax() {
        for stated in [" 1.5 ", "1,000.5", "1.5E0"] {
            assert!(
                AttachedPreset::read(&values("s=STK", &[(4200, stated)]))
                    .unwrap()
                    .profit_uses_exact_parent_quantity,
                "{stated}"
            );
        }
    }

    #[test]
    fn empty_and_multiple_assignment_attachment_values_are_ignored() {
        let preset = AttachedPreset::read(&values(
            "s=STK",
            &[
                (4066, "2"),
                (4066, ""),
                (4084, "4077"),
                (4085, ""),
                (4086, "1=2"),
                (4084, ""),
                (4075, "1=1"),
            ],
        ))
        .unwrap();
        assert_eq!(preset.profit_offset, 2.0);
        assert_eq!(preset.limit, PriceSpec::for_tag(4077).unwrap());
        assert!(!preset.auto_attach_profit_taker);
    }

    #[test]
    fn a_stated_error_leaves_the_values_and_an_unreadable_answer_is_the_key_alone() {
        let mut answer = values("s=STK", &[(4075, "1"), (4083, "2")]);
        answer.error = Some("Not found".into());
        let preset = AttachedPreset::read(&answer).unwrap();
        assert!(preset.auto_attach_profit_taker);
        assert_eq!(preset.profit_order_type, "2");
        answer.fields.clear();
        assert_eq!(AttachedPreset::read(&answer).unwrap(), AttachedPreset::default());
        answer.fields = vec![(4075, "1".into()), (4016, "2".into()), (4084, "bad".into())];
        assert!(is_unreadable(&answer.fields));
        assert_eq!(AttachedPreset::read(&answer).unwrap(), AttachedPreset::default());
        answer.fields = vec![(4084, "bad".into()), (4041, "bad".into())];
        assert!(AttachedPreset::read(&answer).is_none());
        answer.fields = vec![(4084, "".into()), (4084, "1=2".into()), (4084, "9999".into())];
        assert!(!is_unreadable(&answer.fields));
    }

    #[test]
    fn primary_fields_do_not_interrupt_an_attachment_group() {
        let preset = AttachedPreset::read(&values(
            "s=STK",
            &[(4084, "4077"), (4016, "2"), (4085, "3"), (9999, "ignored"), (4086, "100")],
        ))
        .unwrap();
        assert_eq!(preset.primary_trailing_amount, 2.0);
        assert_eq!((preset.limit.offset, preset.limit.unit), (3.0, PriceUnit::Percent));
    }

    #[test]
    fn fractional_share_defaults_select_exact_parent_quantity() {
        for key in ["s=STK", "s=FUND", "s=CRYPTO"] {
            let preset = AttachedPreset::read(&values(key, &[(4200, "1,000.5")])).unwrap();
            assert!(preset.profit_uses_exact_parent_quantity);
        }
        for (key, fields) in [
            ("s=FUT", vec![(4200, "0.5")]),
            ("s=STK", vec![(4200, "1")]),
            ("s=STK", vec![(4200, "NaN")]),
            ("s=STK", vec![(4200, "0.5"), (4041, "Amount")]),
        ] {
            assert!(
                !AttachedPreset::read(&values(key, &fields))
                    .unwrap()
                    .profit_uses_exact_parent_quantity
            );
        }
    }

    #[test]
    fn key_fields_decode_escaped_separators_and_attributes_remain_separate() {
        let key = PresetKey::parse("s=STK&tc=A%20B%26C%3DD%25&u=ANY&m2=x&f=y&sr=t");
        assert_eq!(key.symbol.as_deref(), Some("A B&C=D%"));
        assert_eq!(key.universe.as_deref(), Some("ANY"));
        assert_eq!(key.mifid_selector.as_deref(), Some("x"));
        assert_eq!(key.allocation_selector.as_deref(), Some("y"));
        assert_eq!(key.smart_routing.as_deref(), Some("t"));
        assert_eq!(
            PresetAttributes::parse("v=1&a=1&fr=1&st=0&ms=42"),
            PresetAttributes { active: true, strategy: false }
        );
    }

    #[test]
    fn a_key_is_read_with_the_last_of_each_name_and_a_trailing_equals_dropped() {
        let key = PresetKey::parse("s=STK&tc=A&&tc=B=&u==ANY&x=1&=FUT&s");
        assert_eq!(key.security_type.as_deref(), Some("STK"));
        assert_eq!(key.symbol.as_deref(), Some("B"));
        assert_eq!(key.universe, None);
    }

    #[test]
    fn a_request_names_the_key_rebuilt_in_order_and_escaped() {
        for (listed, sent) in [
            ("s=STK&tc=ABC", "s=STK&tc=ABC"),
            ("tc=A B&s=STK", "s=STK&tc=A%20B"),
            ("tc=A%20B%26C%3DD%25&s=STK", "s=STK&tc=A%20B%26C%3DD%25"),
            ("f=y&m2=x&tc=T&u=ANY&s=OPT&sr=t&zz=1", "sr=t&s=OPT&u=ANY&tc=T&m2=x&f=y"),
            ("s=STK&tc=", "s=STK"),
            ("u=ANY", "u=ANY"),
        ] {
            assert_eq!(request_selector(listed), sent, "{listed}");
        }
    }

    #[test]
    fn a_unit_is_its_number_or_else_its_label() {
        for (stated, unit) in [
            ("0", PriceUnit::Amount),
            ("1", PriceUnit::Ticks),
            ("100", PriceUnit::Percent),
            ("+100", PriceUnit::Percent),
            ("99", PriceUnit::Unavailable),
            ("7", PriceUnit::Unavailable),
            ("amt", PriceUnit::Amount),
            ("ticks", PriceUnit::Ticks),
            ("%", PriceUnit::Percent),
            ("n/a", PriceUnit::Unavailable),
            ("Amt", PriceUnit::Unavailable),
            (" %", PriceUnit::Unavailable),
        ] {
            assert_eq!(PriceUnit::from_wire(stated), unit, "{stated}");
        }
        let preset = AttachedPreset::read(&values(
            "s=STK",
            &[(4071, "%"), (4073, "ticks"), (4039, "amt"), (4084, "4077"), (4086, "%")],
        ))
        .unwrap();
        assert_eq!(
            (preset.trailing_unit, preset.adjusted_trailing_unit, preset.primary_trailing_unit),
            (PriceUnit::Percent, PriceUnit::Ticks, PriceUnit::Amount)
        );
        assert_eq!(preset.limit.unit, PriceUnit::Percent);
    }

    #[test]
    fn selection_prefers_symbol_then_type_then_root() {
        let entries = list(&[("u=ANY", ""), ("s=STK", "a=1"), ("s=STK&tc=ABC", "")]);
        assert_eq!(select_preset_key(&entries, stock(), true), Some("s=STK&tc=ABC"));
        assert_eq!(
            select_preset_key(&entries, PresetInstrument { symbol: "OTHER", ..stock() }, true),
            Some("s=STK")
        );
        assert_eq!(
            select_preset_key(&entries, PresetInstrument { security_type: "FUT", ..stock() }, true),
            Some("u=ANY")
        );
        assert_eq!(select_preset_key(&[], stock(), true), None);
    }

    #[test]
    fn active_variants_require_feature_and_an_inactive_type() {
        let mut entries =
            list(&[("s=STK", ""), ("s=STK&tc=Zulu", "st=1&a=1"), ("s=STK&tc=Alpha", "st=1&a=1")]);
        assert_eq!(select_preset_key(&entries, stock(), false), Some("s=STK"));
        assert_eq!(select_preset_key(&entries, stock(), true), Some("s=STK&tc=Alpha"));
        entries[0].1 = "a=1".into();
        assert_eq!(select_preset_key(&entries, stock(), true), Some("s=STK"));
    }

    #[test]
    fn inactive_matching_strategy_falls_back_to_root() {
        let entries = list(&[("u=ANY", ""), ("s=STK", "a=1"), ("s=STK&tc=ABC", "st=1")]);
        assert_eq!(select_preset_key(&entries, stock(), true), Some("u=ANY"));
        assert_eq!(select_preset_key(&entries, stock(), false), Some("u=ANY"));
    }

    #[test]
    fn cash_prefers_currency_and_cash_cfds_use_cash_presets() {
        let entries = list(&[("s=CASH", "a=1"), ("s=CASH&tc=EUR", ""), ("s=CASH&tc=USD", "")]);
        for security_type in ["CASH", "CFD"] {
            let instrument = PresetInstrument {
                security_type,
                underlying_security_type: "CASH",
                symbol: "EUR",
                currency: "USD",
            };
            assert_eq!(select_preset_key(&entries, instrument, true), Some("s=CASH&tc=USD"));
        }
    }

    #[test]
    fn combinations_and_special_combo_symbol_use_comb_presets() {
        let entries = list(&[("s=COMB", "a=1"), ("s=STK", "a=1")]);
        for security_type in ["BAG", "PDC"] {
            assert_eq!(
                select_preset_key(&entries, PresetInstrument { security_type, ..stock() }, true),
                Some("s=COMB")
            );
        }
        assert_eq!(
            select_preset_key(&entries, PresetInstrument { symbol: "IECombo", ..stock() }, true),
            Some("s=COMB")
        );
    }

    #[test]
    fn administrative_selectors_do_not_replace_order_presets() {
        let entries = list(&[
            ("s=STK&m2=x", "a=1"),
            ("s=STK&f=y", "a=1"),
            ("s=STK&tc=__SA__", "a=1"),
            ("sr=t&s=STK", "a=1"),
            ("s=STK", "a=1"),
        ]);
        assert_eq!(select_preset_key(&entries, stock(), true), Some("s=STK"));
        let smart_routing = list(&[("sr=t", "a=1"), ("s=STK", "")]);
        assert_eq!(
            select_preset_key(
                &smart_routing,
                PresetInstrument { security_type: "FUT", ..stock() },
                true
            ),
            None
        );
    }

    #[test]
    fn a_key_naming_no_selector_is_no_preset() {
        for root in ["", "x=1", "u=", "&"] {
            let entries = list(&[(root, "a=1")]);
            assert_eq!(select_preset_key(&entries, stock(), true), None, "{root:?}");
        }
    }
}
