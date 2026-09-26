//! The venue's definitions of the algorithms an order can go through.
//!
//! A gateway asks for them the first time an order names an algorithm: the
//! list of documents per provider and security type (`6040=80`, answered by
//! `6040=81` on tag 6597), then each document by name (`6040=53`, answered by
//! `6040=54` with the document on tag 6118). One document states a provider —
//! its name and the parameters its algorithms share — and the others state the
//! algorithms under it and their parameters.

use crate::control::xml::{elements, tag};
use std::collections::HashMap;

/// A request for the list of algorithm documents.
pub fn list_request() -> Vec<(u32, String)> {
    vec![(6040, "80".into())]
}

/// A request for one algorithm document, by the name the list gives it.
pub fn document_request(name: &str) -> Vec<(u32, String)> {
    vec![(6040, "53".into()), (6364, name.into())]
}

/// What kind of value a parameter takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueClass {
    /// Text, taken as stated: `String` and `DayTime`.
    Text,
    /// Text of at most so many characters: `FixSizeString`.
    Fixed(usize),
    Double,
    Integer,
    Boolean,
    /// A moment: `Time`, `Date` and `DateTime`.
    Time,
}

/// One of an algorithm's parameters, as a gateway checks an order against it.
#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    /// The name an order states it under.
    pub short_name: String,
    /// What a refusal calls it.
    pub description: String,
    /// Whether it names the algorithm itself.
    pub strategy_selector: bool,
    pub class: ValueClass,
    pub required: bool,
    /// The value it holds where an order states none.
    pub default: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// A number is read in units of ten to this power.
    pub divisor: Option<i32>,
    /// An integer must be a multiple of this.
    pub modifier: Option<i64>,
    /// A price, read as stated rather than in units of the divisor.
    pub price_market_rule: bool,
    /// The legal strings its text is one of, by name.
    pub legal_strings: Option<String>,
    /// The order type the contract must take for it to be stated, and the
    /// one its primary exchange must take.
    pub require_ib_type: Option<String>,
    pub require_ib_type_primary: Option<String>,
}

/// An algorithm as a provider's document defines it.
#[derive(Clone, Debug, PartialEq)]
pub struct Algorithm {
    /// The provider it runs under.
    pub provider: String,
    pub short_name: String,
    pub short_description: Option<String>,
    pub description: Option<String>,
    /// The provider's shared parameter sets it takes, by name.
    pub common_sets: Vec<String>,
    /// Whether its document states the array of its own parameters at all:
    /// one stating none builds without a parameter naming it, and no shared
    /// set may name one either.
    pub contents_stated: bool,
    /// Whether it may work an order in the overnight session.
    pub allow_overnight: bool,
    /// Its own parameters.
    pub parameters: Vec<Parameter>,
}

impl Algorithm {
    /// The name an order names it by: its short name, except the adaptive
    /// algorithm, which is named by its short description, or its description
    /// where it states none.
    pub fn key(&self) -> &str {
        if self.short_name == "Adaptive" {
            self.short_description.as_deref().or(self.description.as_deref()).unwrap_or_default()
        } else {
            &self.short_name
        }
    }
}

/// A provider as its document defines it.
#[derive(Clone, Debug, PartialEq)]
pub struct Provider {
    pub name: String,
    /// The parameter sets its algorithms share, by name.
    pub common: HashMap<String, Vec<Parameter>>,
    /// Its legal strings by name, each `PREFIX:NAME`.
    pub legal: HashMap<String, Vec<String>>,
}

/// What one document states.
#[derive(Clone, Debug, PartialEq)]
pub enum Document {
    Providers(Vec<Provider>),
    Algorithms(Vec<Algorithm>),
}

/// Read one document as the venue states it; `None` where it is empty or
/// states neither providers nor algorithms.
pub fn parse(xml: &str) -> Option<Document> {
    let xml = xml.trim();
    if xml.starts_with("<AlgoExchange>") {
        return Some(Document::Providers(vec![provider(xml)?]));
    }
    let providers: Vec<Provider> =
        elements(xml, "AlgoExchange").into_iter().filter_map(provider).collect();
    if !providers.is_empty() {
        return Some(Document::Providers(providers));
    }
    let algorithms: Vec<Algorithm> =
        elements(xml, "Algorithm").into_iter().filter_map(algorithm).collect();
    (!algorithms.is_empty()).then_some(Document::Algorithms(algorithms))
}

fn provider(xml: &str) -> Option<Provider> {
    let own = without(&without(xml, "AlgoAttributeContentHolderMap"), "AlgoLegalStringsMap");
    let name = text(&own, "name")?;
    let common = elements(xml, "AlgoAttributeContentHolder")
        .into_iter()
        .filter_map(|holder| {
            let name = text(&without(holder, "Array"), "name")?;
            Some((name, parameters(holder)))
        })
        .collect();
    let legal = elements(xml, "AlgoLegalStrings")
        .into_iter()
        .filter_map(|strings| {
            let name = text(&without(strings, "ArString"), "name")?;
            let entries = strings
                .split("<String")
                .skip(1)
                .filter_map(|entry| {
                    let body = &entry[entry.find('>')? + 1..];
                    Some(unescape(&body[..body.find("</String>")?]))
                })
                .collect();
            Some((name, entries))
        })
        .collect();
    Some(Provider { name, common, legal })
}

fn algorithm(xml: &str) -> Option<Algorithm> {
    let own = without(xml, "Array");
    let contents_stated =
        xml.contains("varName=\"attribContents\"") || xml.contains("<attribContents");
    Some(Algorithm {
        provider: text(&own, "algoExchange")?,
        short_name: text(&own, "shortName")?,
        short_description: text(&own, "shortDescription"),
        description: text(&own, "description"),
        common_sets: text(&own, "commonAttributeSets")
            .map(|sets| sets.split(',').map(str::to_string).collect())
            .unwrap_or_default(),
        contents_stated,
        allow_overnight: text(&own, "allowOvernight")
            .is_some_and(|allowed| allowed.eq_ignore_ascii_case("yes")),
        parameters: if contents_stated { parameters(xml) } else { Vec::new() },
    })
}

fn parameters(xml: &str) -> Vec<Parameter> {
    elements(xml, "AlgoAttributeContent")
        .into_iter()
        .filter_map(|content| {
            let number = |name: &str| {
                object(content, name).and_then(|value| value.trim().parse::<f64>().ok())
            };
            let class = match text(content, "valueClassName").as_deref() {
                Some("Double") => ValueClass::Double,
                Some("Integer") => ValueClass::Integer,
                Some("Boolean") => ValueClass::Boolean,
                Some("Time" | "Date" | "DateTime") => ValueClass::Time,
                Some("FixSizeString") => ValueClass::Fixed(
                    text(content, "length")
                        .and_then(|length| length.trim().parse().ok())
                        .unwrap_or(32),
                ),
                _ => ValueClass::Text,
            };
            Some(Parameter {
                short_name: text(content, "shortName")?,
                description: text(content, "description").unwrap_or_default(),
                strategy_selector: text(content, "isStrategySelector").as_deref() == Some("true"),
                class,
                required: text(content, "required").as_deref() == Some("true"),
                default: object(content, "defaultValue"),
                min: number("minValue"),
                max: number("maxValue"),
                divisor: text(content, "divisor").and_then(|divisor| divisor.trim().parse().ok()),
                modifier: text(content, "modifier")
                    .and_then(|modifier| modifier.trim().parse().ok()),
                price_market_rule: text(content, "priceMarketRule").as_deref() == Some("true"),
                legal_strings: text(content, "legalStringsName"),
                require_ib_type: text(content, "requireIBType"),
                require_ib_type_primary: text(content, "requireIBTypePrimExch"),
            })
        })
        .collect()
}

/// The value of an `<Object varName="…">` field.
fn object(xml: &str, name: &str) -> Option<String> {
    let at = xml.find(&format!("<Object varName=\"{name}\""))?;
    let body = &xml[at..];
    let body = &body[body.find('>')? + 1..];
    Some(unescape(&body[..body.find("</Object>")?]))
}

/// A field's text, its entities read.
fn text(xml: &str, name: &str) -> Option<String> {
    tag(xml, name).map(unescape)
}

/// Text with its entities read.
fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The document with every `<name …>…</name>` section taken out.
fn without(xml: &str, name: &str) -> String {
    let close = format!("</{name}>");
    let mut out = String::new();
    let mut rest = xml;
    while let Some(at) = rest.find(&format!("<{name}")) {
        let opens = rest[at + name.len() + 1..]
            .chars()
            .next()
            .is_some_and(|c| c == '>' || c.is_whitespace());
        if !opens {
            out.push_str(&rest[..at + name.len() + 1]);
            rest = &rest[at + name.len() + 1..];
            continue;
        }
        out.push_str(&rest[..at]);
        match rest[at..].find(&close) {
            Some(end) => rest = &rest[at + end + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The algorithms a security type and provider group can use, assembled from
/// their documents as a gateway assembles them: each provider, and each
/// algorithm under the provider it names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Algorithms {
    providers: Vec<(Provider, Vec<Algorithm>)>,
}

/// Put the documents a list names together, a document that does not read
/// counting for nothing. `None` where the last algorithm read names no
/// provider the documents state, or none is stated at all, or an algorithm
/// held does not build — one without a short name or a description, or
/// without a parameter naming it, or with one both of its own and in a shared
/// set — as a gateway, which then refuses the order.
pub fn assemble(documents: &[Option<Document>]) -> Option<Algorithms> {
    let mut providers: Vec<(Provider, Vec<Algorithm>)> = Vec::new();
    for document in documents.iter().flatten() {
        let Document::Providers(stated) = document else { continue };
        for provider in stated {
            match providers.iter_mut().find(|(held, _)| held.name == provider.name) {
                Some(held) => *held = (provider.clone(), Vec::new()),
                None => providers.push((provider.clone(), Vec::new())),
            }
        }
    }
    let mut last = false;
    for document in documents.iter().flatten() {
        let Document::Algorithms(algorithms) = document else { continue };
        for algorithm in algorithms {
            let under =
                providers.iter_mut().find(|(provider, _)| provider.name == algorithm.provider);
            last = under.is_some();
            if let Some((_, held)) = under {
                held.retain(|other| other.key() != algorithm.key());
                held.push(algorithm.clone());
            }
        }
    }
    let builds = |provider: &Provider, algorithm: &Algorithm| {
        let own = algorithm.parameters.iter().any(|parameter| parameter.strategy_selector);
        let shared = algorithm
            .common_sets
            .iter()
            .filter_map(|set| provider.common.get(set))
            .filter(|set| set.iter().any(|parameter| parameter.strategy_selector))
            .count();
        let named = !algorithm.short_name.trim().is_empty()
            && algorithm
                .description
                .as_deref()
                .is_some_and(|description| !description.trim().is_empty());
        if !algorithm.contents_stated {
            return named && shared == 0;
        }
        named && (own as usize + shared) == 1
    };
    let built = providers.iter().all(|(provider, algorithms)| {
        algorithms.iter().all(|algorithm| builds(provider, algorithm))
    });
    (last && built).then_some(Algorithms { providers })
}

impl Algorithms {
    /// The first provider holding an algorithm under a name, and the
    /// algorithm.
    pub fn find(&self, name: &str) -> Option<(&Provider, &Algorithm)> {
        if name.is_empty() {
            return None;
        }
        self.providers.iter().find_map(|(provider, algorithms)| {
            algorithms
                .iter()
                .find(|algorithm| algorithm.key() == name)
                .map(|algorithm| (provider, algorithm))
        })
    }

    /// Whether any provider is held.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl Algorithm {
    /// Every parameter it takes: its own, and those of the provider's shared
    /// sets it names.
    pub fn every_parameter<'a>(
        &'a self,
        provider: &'a Provider,
    ) -> impl Iterator<Item = &'a Parameter> {
        self.parameters
            .iter()
            .chain(self.common_sets.iter().filter_map(|set| provider.common.get(set)).flatten())
    }

    /// Whether it takes a parameter of a name.
    pub fn takes(&self, provider: &Provider, name: &str) -> bool {
        self.every_parameter(provider).any(|parameter| parameter.short_name == name)
    }

    /// Whether any parameter names the algorithm itself.
    pub fn has_strategy_selector(&self, provider: &Provider) -> bool {
        self.every_parameter(provider).any(|parameter| parameter.strategy_selector)
    }
}

/// A refusal of an order through an algorithm: its code and words.
pub type Refused = (i32, String);

/// A value an order's parameter holds.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Text(String),
    Number(f64),
    Whole(i64),
    Flag(bool),
}

/// What a gateway refuses of an order's parameters against the algorithm's
/// definition, as it checks them: each parameter the order states, in the
/// order stated, must be one the algorithm takes (443), not its amount
/// (10205), a moment in a form a gateway reads (10314), and a number where
/// the parameter takes one (441). Then every parameter the order carries — the
/// algorithm's own, at their defaults where the contract takes them, and
/// those the order states — must be one of its legal strings (145), within
/// its length (10130), one the contract takes (441), and stated where it is
/// required and within its bounds (441, every failure listed).
///
/// `takes` says whether the contract takes an order type, `primary` names
/// its primary exchange, and `zoneless` is told of a moment stated without a
/// zone, which a gateway warns of before it reads it.
pub fn check(
    provider: &Provider,
    algorithm: &Algorithm,
    stated: &[(&str, &str)],
    side: &str,
    order_type: &str,
    takes: impl Fn(&str) -> bool,
    primary: &str,
    zoneless: &mut dyn FnMut(),
) -> Result<(), Refused> {
    let allowed = |parameter: &Parameter| {
        parameter.require_ib_type.as_deref().is_none_or(&takes)
            && parameter.require_ib_type_primary.as_deref().is_none_or(&takes)
    };
    // A name the definition repeats means its last statement.
    let defined = |name: &str| {
        algorithm.every_parameter(provider).filter(|parameter| parameter.short_name == name).last()
    };
    let mut definition = NameMap::new();
    for parameter in algorithm.every_parameter(provider) {
        definition.put(&parameter.short_name);
    }
    // The order's copy of those the contract takes, at their defaults, less
    // every flag not set.
    let mut map = NameMap::new();
    let mut held: Vec<(&Parameter, Option<Value>)> = Vec::new();
    for parameter in definition.names().filter_map(defined).filter(|parameter| allowed(parameter)) {
        let default = if parameter.strategy_selector {
            Some(Value::Text(algorithm.short_name.clone()))
        } else {
            parameter.default.as_deref().and_then(|default| read_default(parameter.class, default))
        };
        map.put(&parameter.short_name);
        held.push((parameter, default));
    }
    held.retain(|(parameter, value)| {
        let kept =
            parameter.class != ValueClass::Boolean || matches!(value, Some(Value::Flag(true)));
        if !kept {
            map.remove(&parameter.short_name);
        }
        kept
    });
    for (tag, value) in stated {
        let Some(parameter) = defined(tag) else {
            return Err((443, format!("Order processing failed. Unknown algo attribute  :{tag}")));
        };
        if *tag == "monetaryValue" {
            return Err((
                10205,
                "Cash Quantity cannot be send in monetaryValue field in Algo . Please try sending in Cash \
                 Quantity field."
                    .into(),
            ));
        }
        let default = || {
            held.iter()
                .find(|(held, _)| held.short_name == *tag)
                .and_then(|(_, value)| value.clone())
        };
        let read = if value.is_empty() {
            default().or_else(|| {
                parameter
                    .default
                    .as_deref()
                    .and_then(|default| read_default(parameter.class, default))
            })
        } else {
            if parameter.class == ValueClass::Time {
                if zone_is_left_out(value) {
                    zoneless();
                }
                if !moment(value) {
                    return Err((10314, format!("{tag}{MOMENT_FORMATS}")));
                }
            }
            let failed = || (441, format!("Algo attributes validation failed:{tag}={value}"));
            Some(match parameter.class {
                ValueClass::Integer => Value::Whole(java_integer(value).ok_or_else(failed)?),
                ValueClass::Double => {
                    let number = java_double(value).ok_or_else(failed)?;
                    match parameter.divisor.filter(|_| !parameter.price_market_rule) {
                        Some(divisor) => {
                            let scaled = number * 10_f64.powi(divisor).round();
                            Value::Number((scaled * 1e8).round() / 1e8)
                        }
                        None => Value::Number(number),
                    }
                }
                ValueClass::Boolean => Value::Flag(flag(value)),
                ValueClass::Text | ValueClass::Fixed(_) | ValueClass::Time => {
                    Value::Text(value.to_string())
                }
            })
        };
        map.put(&parameter.short_name);
        match held.iter_mut().find(|(held, _)| held.short_name == *tag) {
            Some(slot) => *slot = (parameter, read),
            None => held.push((parameter, read)),
        }
    }
    // Read in the order a gateway's map of them iterates.
    let held: Vec<(&Parameter, Option<Value>)> = map
        .names()
        .filter_map(|name| held.iter().find(|(parameter, _)| parameter.short_name == name).cloned())
        .collect();
    let prefixes = [
        "ALL".to_string(),
        side.to_string(),
        order_type.to_string(),
        format!("{side}/{order_type}"),
    ];
    for (parameter, value) in &held {
        if let Some(Value::Text(text)) = value
            && !text.is_empty()
            && let Some(strings) =
                parameter.legal_strings.as_ref().and_then(|name| provider.legal.get(name))
        {
            let legal =
                strings.iter().filter_map(|entry| entry.split_once(':')).any(|(prefix, name)| {
                    prefixes.iter().any(|allowed| allowed == prefix) && name == text
                });
            if !legal {
                return Err((145, format!("Error in validating entry fields -{text}")));
            }
        }
        if let (ValueClass::Fixed(length), Some(Value::Text(text))) = (parameter.class, value)
            && text.chars().count() > length
        {
            return Err((
                10130,
                format!(
                    "{}:The maximum length of {length} characters has been exceeded",
                    parameter.short_name
                ),
            ));
        }
    }
    if let Some((parameter, _)) = held.iter().find(|(parameter, _)| !allowed(parameter)) {
        return Err((
            441,
            format!(
                "Algo attributes validation failed:\nThe usage of algorithm parameter {} is not allowed on {primary}",
                parameter.short_name
            ),
        ));
    }
    let mut invalid = String::new();
    for (parameter, value) in &held {
        let mut say = |what: String| {
            invalid.push_str(&format!("'{}' is invalid: {what}.\n", parameter.description));
        };
        let number = match value {
            None => {
                if parameter.required {
                    say("value is required".into());
                }
                continue;
            }
            Some(Value::Number(number)) => *number,
            Some(Value::Whole(whole)) => {
                if let Some(modifier) = parameter.modifier.filter(|modifier| *modifier != 0)
                    && whole % modifier != 0
                {
                    say(format!("value must be an integer multiple of {modifier}"));
                }
                *whole as f64
            }
            Some(_) => continue,
        };
        let shown = |bound: f64| match value {
            Some(Value::Whole(_)) => format!("{}", bound as i64),
            _ => java_double_text(bound),
        };
        if let Some(min) = parameter.min.filter(|min| number < *min) {
            say(format!("value is less than minimum value {}", shown(min)));
            continue;
        }
        if let Some(max) = parameter.max.filter(|max| number > *max) {
            say(format!("value is greater than maximum value {}", shown(max)));
        }
    }
    if !invalid.is_empty() {
        return Err((441, format!("Algo attributes validation failed:\n{invalid}")));
    }
    Ok(())
}

/// Names as a gateway's map of an order's parameters holds them: a concurrent
/// hash map built for sixteen at a load of three quarters, so thirty-two
/// bins, doubling once it holds three quarters of its bins. A name's bin is
/// its hash with the high half folded into the low; a bin keeps its names in
/// the order they went in, and the map reads bin by bin.
struct NameMap<'a> {
    bins: Vec<Vec<(usize, &'a str)>>,
    count: usize,
    grow_at: usize,
}

impl<'a> NameMap<'a> {
    fn new() -> Self {
        Self { bins: vec![Vec::new(); 32], count: 0, grow_at: 24 }
    }

    // ponytail: a bin of nine names in a map of sixty-four bins or more turns
    // into a tree a gateway reads differently; the venue's algorithms name at
    // most sixteen parameters, so the map never gets there.
    fn put(&mut self, name: &'a str) {
        let hash = name
            .encode_utf16()
            .fold(0i32, |hash, unit| hash.wrapping_mul(31).wrapping_add(i32::from(unit)));
        let hash = ((hash ^ ((hash as u32) >> 16) as i32) & 0x7fff_ffff) as usize;
        let size = self.bins.len();
        let bin = &mut self.bins[hash & (size - 1)];
        let (walked, added) = match bin.iter().position(|(_, held)| *held == name) {
            Some(at) => (at + 1, false),
            None => {
                bin.push((hash, name));
                (bin.len() - 1, true)
            }
        };
        // Eight names walked past in a map of fewer than sixty-four bins
        // grows it to four times the bins it had.
        if walked >= 8 && size < 64 {
            while self.bins.len() < 4 * size {
                self.grow();
            }
        }
        if added {
            self.count += 1;
            while self.count >= self.grow_at {
                self.grow();
            }
        }
    }

    fn remove(&mut self, name: &str) {
        for bin in &mut self.bins {
            if let Some(at) = bin.iter().position(|(_, held)| *held == name) {
                bin.remove(at);
                self.count -= 1;
            }
        }
    }

    /// Each bin splits in two by the hash's next bit. Its last run of names
    /// sharing that bit moves as it stands; the names before it are laid in
    /// front of their half one by one, so they end up reversed.
    fn grow(&mut self) {
        let size = self.bins.len();
        let mut bins = vec![Vec::new(); 2 * size];
        for (at, bin) in self.bins.iter().enumerate() {
            let high = |(hash, _): &(usize, &str)| hash & size != 0;
            let Some(last) = bin.last() else { continue };
            let run = bin
                .iter()
                .rposition(|entry| high(entry) != high(last))
                .map_or(0, |before| before + 1);
            for entry in bin[..run].iter().rev().chain(&bin[run..]) {
                bins[if high(entry) { at + size } else { at }].push(*entry);
            }
        }
        self.bins = bins;
        self.grow_at = 3 * size / 2;
    }

    fn names(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.bins.iter().flatten().map(|(_, name)| *name)
    }
}

/// A default as the definition states it, read for its class.
fn read_default(class: ValueClass, default: &str) -> Option<Value> {
    Some(match class {
        ValueClass::Double => Value::Number(default.trim().parse().ok()?),
        ValueClass::Integer => Value::Whole(default.trim().parse().ok()?),
        ValueClass::Boolean => Value::Flag(flag(default)),
        _ => Value::Text(default.to_string()),
    })
}

/// A flag as a gateway reads one: `true` in any case, or text starting with
/// 1 or Y; anything else is false.
fn flag(value: &str) -> bool {
    value.eq_ignore_ascii_case("true")
        || (!value.eq_ignore_ascii_case("false")
            && matches!(value.chars().next(), Some('1' | 'Y' | 'y')))
}

/// An integer as Java reads one: a sign, then digits, within 32 bits.
fn java_integer(value: &str) -> Option<i64> {
    value.parse::<i32>().ok().map(i64::from)
}

/// A number as Java reads one: surrounding space allowed, a trailing `d` or
/// `f`, `Infinity`; `nan` in any case reads as the largest number.
fn java_double(value: &str) -> Option<f64> {
    if value.eq_ignore_ascii_case("nan") {
        return Some(f64::MAX);
    }
    let trimmed = value.trim();
    let trimmed = trimmed.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(trimmed);
    match trimmed.trim_start_matches(['+', '-']) {
        "Infinity" => {
            Some(if trimmed.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY })
        }
        digits
            if digits
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) =>
        {
            trimmed.parse().ok()
        }
        _ => None,
    }
}

/// A number as Java writes a double: its shortest digits, with a point, in
/// scientific form below a thousandth and from ten million.
fn java_double_text(value: f64) -> String {
    if value == 0.0 {
        return "0.0".into();
    }
    if (1e-3..1e7).contains(&value.abs()) {
        let text = format!("{value}");
        return if text.contains('.') { text } else { format!("{text}.0") };
    }
    let text = format!("{value:e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let mantissa =
        if mantissa.contains('.') { mantissa.to_string() } else { format!("{mantissa}.0") };
    format!("{mantissa}E{exponent}")
}

/// What a refusal of a moment says after the parameter's name.
const MOMENT_FORMATS: &str = ": The date, time, or time-zone entered is invalid.\nThe correct format is yyyymmdd \
     hh:mm:ss xx/xxxx\nwhere yyyymmdd and xx/xxxx are optional.\nE.g.: 20031126 15:59:00 US/Eastern\n\nNote that \
     there is a space between the date and time,\nand between the time and time-zone.\n\nIf no date is specified, \
     current date is assumed.\nIf no time-zone is specified, local time-zone is assumed(deprecated).\n\nYou can also \
     provide yyyymmddd-hh:mm:ss time is in UTC.\nNote that there is a dash between the date and time in UTC notation.";

/// Whether a gateway reads a moment: `yyyyMMdd-HH:mm:ss` in UTC, exactly; or
/// a time `H:m:s` on a 24-hour clock, with an eight-digit date before it and
/// a zone after it where they are stated, the zone one a gateway knows.
pub fn moment(value: &str) -> bool {
    if utc_moment(value) {
        return true;
    }
    if !local_moment(value) {
        return false;
    }
    let tokens: Vec<&str> = value.split_whitespace().collect();
    let zone = if tokens.first().is_some_and(|first| first.contains(':')) {
        &tokens[1..]
    } else {
        tokens.get(2..).unwrap_or(&[])
    };
    zone.is_empty() || known_zone(&zone.join(" "))
}

/// Whether a moment in a gateway's local form states no zone, which it warns
/// of.
pub fn zone_is_left_out(value: &str) -> bool {
    !value.is_empty() && local_moment(value) && stated_zone(value.trim()).is_empty()
}

fn utc_moment(value: &str) -> bool {
    let bytes = value.as_bytes();
    let number = |range: std::ops::Range<usize>| {
        value
            .get(range)
            .filter(|digits| digits.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u32>().ok())
    };
    if bytes.len() != 17 || bytes[8] != b'-' || bytes[11] != b':' || bytes[14] != b':' {
        return false;
    }
    let (Some(_), Some(month), Some(day), Some(hour), Some(minute), Some(second)) =
        (number(0..4), number(4..6), number(6..8), number(9..11), number(12..14), number(15..17))
    else {
        return false;
    };
    (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && ((hour <= 23 && minute <= 59 && second <= 59)
            || (hour == 24 && minute == 0 && second == 0))
}

/// The zone after a moment: the trailing words that do not start with a
/// digit, after the first.
fn stated_zone(value: &str) -> String {
    let words: Vec<&str> = value.split(' ').collect();
    let mut zone: Vec<&str> = Vec::new();
    for word in words.iter().skip(1).rev() {
        if word.chars().next().is_some_and(|c| !c.is_ascii_digit()) {
            zone.push(word);
        } else {
            break;
        }
    }
    zone.reverse();
    zone.join(" ")
}

fn local_moment(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let trimmed = value.trim();
    let zone = stated_zone(trimmed);
    let rest = if zone.is_empty() { trimmed } else { &trimmed[..trimmed.len() - zone.len() - 1] };
    let time = |text: &str| {
        let parts: Vec<&str> = text.split(':').filter(|part| !part.is_empty()).collect();
        let numbers: Vec<Option<i32>> = parts.iter().map(|part| part.parse().ok()).collect();
        matches!(numbers[..], [Some(h), Some(m), Some(s)] if (0..=23).contains(&h) && (0..=59).contains(&m) && (0..=59).contains(&s))
    };
    let date = |text: &str| {
        let number = |range: std::ops::Range<usize>| {
            text.get(range).and_then(|digits| digits.parse::<i32>().ok())
        };
        text.len() == 8
            && matches!((number(0..4), number(4..6), number(6..8)),
                (Some(year), Some(month), Some(day)) if (1978..=3000).contains(&year) && (1..=12).contains(&month) && (0..=31).contains(&day))
    };
    match rest.split_once(' ') {
        Some((day, at)) => time(at) && date(day),
        None => time(rest),
    }
}

/// Whether a gateway knows a zone by the name stated: `GMT`, or a name its
/// runtime gives a zone — the zone's identifier, its standard name, or its name
/// and abbreviation at the moment — among the zones it keeps, save six
/// abbreviations it never reads.
fn known_zone(zone: &str) -> bool {
    let lower = zone.to_lowercase();
    lower == "gmt"
        || zone_names().contains(&format!("{lower}_DAYLIGHT"))
        || zone_names().contains(&format!("{lower}_NIGHT"))
}

/// The zones a gateway's runtime states, with the names it gives each in
/// English: identifier, whether it keeps daylight time, then its standard and
/// daylight names, long and short.
const RUNTIME_ZONES: &str = include_str!("fixtures/runtime_zones.tsv");

/// The names a gateway reads a zone by, each marked by whether the zone keeps
/// daylight time, as it holds them from its start.
fn zone_names() -> &'static std::collections::HashSet<String> {
    const SET_ASIDE: &[&str] = &[
        "America/Argentina/ComodRivadavia",
        "America/Catamarca",
        "America/Havana",
        "America/Indiana/Knox",
        "America/Jujuy",
        "America/Knox_IN",
        "America/Mendoza",
        "Antarctica/South_Pole",
        "Antarctica/Troll",
        "Asia/Ulan_Bator",
        "Asia/Yakutsk",
        "Australia/Yancowinna",
        "Canada/East-Saskatchewan",
        "Canada/Newfoundland",
        "Cuba",
        "Europe/Tiraspol",
        "Iran",
        "Mexico/BajaSur",
        "Pacific/Fakaofo",
        "Pacific/Ponape",
        "Pacific/Truk",
        "Pacific/Wake",
        "US/Michigan",
        "US/Indiana-Starke",
        "US/Pacific-New",
        "ACT",
        "AET",
        "AGT",
        "ART",
        "AST",
        "BET",
        "BST",
        "CAT",
        "CNT",
        "CST",
        "CTT",
        "EAT",
        "ECT",
        "IET",
        "IST",
        "JST",
        "MIT",
        "NET",
        "NST",
        "PLT",
        "PNT",
        "PRT",
        "PST",
        "SST",
        "VST",
    ];
    const NEVER_READ: [&str; 6] = ["amt", "ast", "bst", "cst", "gst", "ist"];
    static HELD: std::sync::OnceLock<std::collections::HashSet<String>> =
        std::sync::OnceLock::new();
    HELD.get_or_init(|| {
        let now = jiff::Timestamp::now();
        let mut names = std::collections::HashSet::new();
        for row in RUNTIME_ZONES.lines().filter(|row| !row.starts_with('#')) {
            let fields: Vec<&str> = row.split('\t').collect();
            let [id, held, daylight, long, long_daylight, short, short_daylight] = fields[..]
            else {
                continue;
            };
            if SET_ASIDE.contains(&held) {
                continue;
            }
            let keeps_daylight = daylight == "true";
            let in_daylight = keeps_daylight
                && crate::protocol::datetime::clock_named(id)
                    .is_some_and(|clock| clock.to_offset_info(now).dst().is_dst());
            let mark = if keeps_daylight { "_DAYLIGHT" } else { "_NIGHT" };
            let (now_long, now_short) =
                if in_daylight { (long_daylight, short_daylight) } else { (long, short) };
            for name in [held, now_long, now_short] {
                let name = name.to_lowercase();
                if !NEVER_READ.contains(&name.as_str()) {
                    names.insert(format!("{name}{mark}"));
                }
            }
        }
        names
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The provider's document and its share algorithms, as the venue
    /// answered them on a paper session.
    const PROVIDER: &str = include_str!("fixtures/algorithms_ibalgo_ae.xml");
    const SHARE_ALGORITHMS: &str = include_str!("fixtures/algorithms_ibalgo_al_stk.xml");

    /// The venue's documents read as a gateway reads them: the provider and
    /// the parameter set its algorithms share, then each algorithm under the
    /// name an order gives it, with its own parameters and the shared ones.
    /// Put together, an algorithm under a provider no document states leaves
    /// no set, and an algorithm without a parameter naming it voids the set.
    #[test]
    fn the_venues_algorithm_documents_read_as_a_gateway_reads_them() {
        let documents = [parse(PROVIDER), parse(SHARE_ALGORITHMS)];
        let algorithms = assemble(&documents).expect("the documents make a set");
        let (provider, adaptive) = algorithms.find("Adaptive").expect("the adaptive algorithm");
        assert_eq!(provider.name, "IBALGO");
        assert!(adaptive.allow_overnight);
        for parameter in ["monetaryValue", "adaptivePriority", "strategy"] {
            assert!(adaptive.takes(provider, parameter), "{parameter}");
        }
        assert!(adaptive.has_strategy_selector(provider));
        let (_, accumulate) = algorithms.find("AccuDistr").expect("accumulate-distribute");
        assert!(!accumulate.takes(provider, "monetaryValue"));
        assert!(!algorithms.find("ArrivalPx").unwrap().1.allow_overnight);
        assert!(
            algorithms.find("ArrPx").is_none(),
            "named by its short name, not its short description"
        );
        assert!(algorithms.find("adaptive").is_none(), "names are read as stated");

        // An algorithm whose document states no array of its own parameters
        // builds without a parameter naming it; a shared set naming one then
        // voids the set, as it voids one naming two.
        let unstated =
            SHARE_ALGORITHMS.replace("varName=\"attribContents\"", "varName=\"attribContentsX\"");
        assert_eq!(
            assemble(&[parse(PROVIDER), parse(&unstated)]),
            None,
            "no parameters of its own and a shared set naming one",
        );
        let unshared =
            unstated.replace("<commonAttributeSets>IBALGO_COMMON</commonAttributeSets>", "");
        assert!(
            assemble(&[parse(PROVIDER), parse(&unshared)]).is_some(),
            "no parameters of its own and no shared set naming one",
        );

        assert_eq!(assemble(&[parse(SHARE_ALGORITHMS)]), None, "no provider stated");
        assert_eq!(assemble(&[parse(PROVIDER)]), None, "no algorithm stated");
        assert_eq!(assemble(&[parse(PROVIDER), None, parse(SHARE_ALGORITHMS)]), Some(algorithms));

        // The map of an algorithm's parameters iterates in the order a
        // map of the same names a gateway holds iterates them (the order the
        // runtime's map was measured to give for these names, under the
        // layout a gateway builds it at).
        let mut map = NameMap::new();
        for name in [
            "maxPctVol",
            "startTime",
            "endTime",
            "allowPastEndTime",
            "noTakeLiq",
            "speedUp",
            "monetaryValue",
            "optoutOpeningAuction",
            "optoutClosingAuction",
            "conditionalPrice",
            "strategy",
        ] {
            map.put(name);
        }
        assert_eq!(
            map.names().collect::<Vec<_>>(),
            [
                "noTakeLiq",
                "optoutClosingAuction",
                "allowPastEndTime",
                "speedUp",
                "monetaryValue",
                "optoutOpeningAuction",
                "conditionalPrice",
                "startTime",
                "maxPctVol",
                "endTime",
                "strategy",
            ],
        );
        let unnamed = SHARE_ALGORITHMS
            .replace("<commonAttributeSets>IBALGO_COMMON</commonAttributeSets>", "");
        assert_eq!(
            assemble(&[parse(PROVIDER), parse(&unnamed)]),
            None,
            "no parameter names the algorithm"
        );
    }
}
