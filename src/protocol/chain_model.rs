//! The parameters the venue's option model works an underlying's chain from.
//!
//! Two series state them in one shape: the standing set, and the set as the
//! chain closed. The payload is compressed, and what it inflates to is a run of
//! big-endian fields whose presence depends on a version stated first. A
//! record that stops short of what its version says it carries states nothing:
//! a gateway skips such a record whole rather than keeping the part it read.

use std::collections::HashMap;
use std::io::Read;

/// What the venue's option model works one class of an underlying's options
/// from: the underlying's price, the dividends it expects, and per expiry the
/// yield, the rate, the forward and the at-the-money volatilities.
///
/// Stated on two series, one standing and one as the chain closed, each asked
/// for on the underlying. The documented API has no call for any of it; a
/// gateway reads it for its own option model and hands none of it on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChainModelParameters {
    /// The venue's number for the product the chain belongs to.
    pub product_id: i32,
    /// Each trading class the set covers, with the multipliers it covers the
    /// class at, in the order the venue named them.
    pub classes: Vec<(String, Vec<f64>)>,
    /// The underlying's price the set was worked from, or `None` where the
    /// venue says that price does not stand.
    pub underlying_price: Option<f64>,
    /// Each dividend expected, as its ex-date (`YYYYMMDD`) and its amount.
    pub dividends: Vec<(String, f64)>,
    /// Whether the dividends are carried the way an index's are rather than as
    /// a share's discrete payouts.
    pub index_style_dividends: bool,
    /// One set per expiry, in the order the venue stated them.
    pub terms: Vec<ChainModelTerm>,
    /// When the set was worked out, in milliseconds since the epoch.
    pub timestamp_millis: i64,
    /// The venue's flags for the set: one is set where it stands, two where
    /// its volatilities are worked from prices, four where the underlying's
    /// price stands.
    pub attributes: i32,
}

/// What the venue's option model works one expiry of a chain from.
///
/// The two volatilities are per trading day, as the venue states them. The
/// venue states no unit for the yield and the rate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChainModelTerm {
    /// The last day the expiry trades, as `YYYYMMDD`.
    pub last_trade_date: String,
    /// The yield the model carries the underlying at.
    pub model_yield: f64,
    /// The interest rate the model discounts at.
    pub interest_rate: f64,
    /// The underlying's forward price to that expiry.
    pub forward: f64,
    /// The at-the-money volatility of the calls, per trading day.
    pub call_atm_vol: f64,
    /// The at-the-money volatility of the puts, per trading day.
    pub put_atm_vol: f64,
    /// The venue's flags for the expiry, as for the whole set.
    pub attributes: i32,
}

/// The bytes that open a compressed stream, looked for rather than assumed to
/// sit at a fixed place: what stands in front of them is not part of it.
const ZLIB_MAGIC: [u8; 2] = [0x78, 0x9C];

/// Read one record of either series.
///
/// `None` where the record carries no compressed stream, where what it
/// inflates to stops short of what it states — reading at most what one
/// payload may become, so a record that needs more than that stops short too —
/// or where it states a count that cannot be. A count of sets below nought is
/// the venue saying it has none to state. What follows a whole record is not
/// read.
pub fn parse(payload: &[u8]) -> Option<Vec<ChainModelParameters>> {
    let start = payload.windows(2).position(|pair| pair == ZLIB_MAGIC)?;
    let mut stream = flate2::read::ZlibDecoder::new(&payload[start..])
        .take(crate::protocol::fixcomp::MAX_INFLATED);
    let version = read_i32(&mut stream)?;
    let count = read_i32(&mut stream)?;
    if count < 0 {
        return None;
    }
    let mut sets = Vec::new();
    for _ in 0..count {
        sets.push(read_set(&mut stream, version)?);
    }
    Some(sets)
}

fn read_set(stream: &mut impl Read, version: i32) -> Option<ChainModelParameters> {
    let product_id = read_i32(stream)?;
    let classes = read_classes(stream, version)?;
    let underlying_price = read_f64(stream)?;
    let stated = read_i32(stream)?;
    if stated < 0 {
        return None;
    }
    let mut dividends = Vec::new();
    for _ in 0..stated {
        let ex_date = read_date(stream)?;
        dividends.push((ex_date, read_f64(stream)?));
    }
    let stated = read_i32(stream)?;
    if stated < 0 {
        return None;
    }
    let mut terms: Vec<ChainModelTerm> = Vec::new();
    let mut placed: HashMap<String, usize> = HashMap::new();
    for _ in 0..stated {
        let term = read_term(stream, version)?;
        // One set per expiry: a date stated twice keeps its first place and
        // takes the later figures.
        match placed.get(&term.last_trade_date) {
            Some(&at) => terms[at] = term,
            None => {
                placed.insert(term.last_trade_date.clone(), terms.len());
                terms.push(term);
            }
        }
    }
    let timestamp_millis = i64::from(read_i32(stream)?) * 1000;
    let index_style = if version > 2 { read_i32(stream)? } else { 0 };
    let attributes = if version > 3 { read_i32(stream)? } else { 1 };
    Some(ChainModelParameters {
        product_id,
        classes,
        // The price is stated whether or not it stands; bit two says which.
        underlying_price: (attributes & 4 != 0).then_some(underlying_price),
        dividends,
        index_style_dividends: index_style > 0,
        terms,
        timestamp_millis,
        attributes,
    })
}

/// The trading classes a set covers, each with the multipliers it covers the
/// class at.
///
/// From the second version each multiplier is stated with the classes it
/// covers; before that, one list of classes shares one multiplier. A class
/// list the venue states as nothing covers no class.
fn read_classes(stream: &mut impl Read, version: i32) -> Option<Vec<(String, Vec<f64>)>> {
    let mut classes: Vec<(String, Vec<f64>)> = Vec::new();
    let mut placed: HashMap<String, usize> = HashMap::new();
    if version > 1 {
        let stated = read_i32(stream)?;
        for _ in 0..stated.max(0) {
            let multiplier = read_rational(stream)?;
            let Some(named) = read_ascii(stream)? else {
                continue;
            };
            for class in split_list(&named) {
                match placed.get(class) {
                    Some(&at) => {
                        let multipliers = &mut classes[at].1;
                        if !multipliers.iter().any(|m| m.to_bits() == multiplier.to_bits()) {
                            multipliers.push(multiplier);
                        }
                    }
                    None => {
                        placed.insert(class.to_string(), classes.len());
                        classes.push((class.to_string(), vec![multiplier]));
                    }
                }
            }
        }
    } else {
        let named = read_ascii(stream)?;
        let multiplier = read_rational(stream)?;
        for class in named.as_deref().into_iter().flat_map(split_list) {
            // One multiplier for every class named, so a class named twice is
            // a list that says two things about one class.
            if placed.insert(class.to_string(), classes.len()).is_some() {
                return None;
            }
            classes.push((class.to_string(), vec![multiplier]));
        }
    }
    Some(classes)
}

fn read_term(stream: &mut impl Read, version: i32) -> Option<ChainModelTerm> {
    let last_trade_date = read_date(stream)?;
    let model_yield = read_f64(stream)?;
    let interest_rate = read_f64(stream)?;
    let forward = read_f64(stream)?;
    let call_atm_vol = read_f64(stream)?;
    let put_atm_vol = read_f64(stream)?;
    let attributes = if version > 3 { read_i32(stream)? } else { 1 };
    Some(ChainModelTerm {
        last_trade_date,
        model_yield,
        interest_rate,
        forward,
        call_atm_vol,
        put_atm_vol,
        attributes,
    })
}

/// A list the venue writes with commas between its members, read the way its
/// writer splits one: the empty members at its end are not members, and a
/// list of nothing at all is one empty member.
fn split_list(named: &str) -> impl Iterator<Item = &str> {
    let members = named.trim_end_matches(',');
    (named.is_empty() || !members.is_empty()).then(|| members.split(',')).into_iter().flatten()
}

/// A day as the venue counts one here, days since the epoch, written the way
/// a contract's own dates are.
fn read_date(stream: &mut impl Read) -> Option<String> {
    let days = read_i32(stream)?;
    let date = jiff::civil::date(1970, 1, 1)
        .checked_add(jiff::Span::new().try_days(i64::from(days)).ok()?)
        .ok()?;
    Some(date.strftime("%Y%m%d").to_string())
}

/// A count, that many bytes of text, then padding to a multiple of four. A
/// count below nought is text the venue states as nothing, not a fault.
fn read_ascii(stream: &mut impl Read) -> Option<Option<String>> {
    let stated = read_i32(stream)?;
    let Ok(stated) = usize::try_from(stated) else {
        return Some(None);
    };
    let mut body = Vec::new();
    stream.by_ref().take(stated as u64).read_to_end(&mut body).ok()?;
    if body.len() != stated {
        return None;
    }
    let mut padding = [0u8; 3];
    stream.read_exact(&mut padding[..(4 - stated % 4) % 4]).ok()?;
    Some(Some(body.iter().map(|b| char::from(*b)).collect()))
}

/// A number stated as its numerator over its denominator. Over nothing, it is
/// no number at all.
fn read_rational(stream: &mut impl Read) -> Option<f64> {
    let numerator = read_f64(stream)?;
    let denominator = read_f64(stream)?;
    Some(if denominator == 0.0 { f64::NAN } else { numerator / denominator })
}

fn read_i32(stream: &mut impl Read) -> Option<i32> {
    let mut bytes = [0u8; 4];
    stream.read_exact(&mut bytes).ok()?;
    Some(i32::from_be_bytes(bytes))
}

fn read_f64(stream: &mut impl Read) -> Option<f64> {
    let mut bytes = [0u8; 8];
    stream.read_exact(&mut bytes).ok()?;
    Some(f64::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A record's fields, written the way the venue writes them.
    #[derive(Default)]
    struct Body(Vec<u8>);

    impl Body {
        fn i32(mut self, v: i32) -> Self {
            self.0.extend_from_slice(&v.to_be_bytes());
            self
        }
        fn f64(mut self, v: f64) -> Self {
            self.0.extend_from_slice(&v.to_be_bytes());
            self
        }
        fn ascii(mut self, text: &str) -> Self {
            self = self.i32(text.len() as i32);
            self.0.extend_from_slice(text.as_bytes());
            self.0.resize(self.0.len() + (4 - text.len() % 4) % 4, 0);
            self
        }
        /// Compressed, behind one byte of the venue's own that is not part of
        /// the stream.
        fn record(self) -> Vec<u8> {
            let mut out = vec![0x01];
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(&self.0).unwrap();
            out.extend(z.finish().unwrap());
            out
        }
    }

    /// One set as the fourth version states it, ending with `attributes`.
    fn one_set(version: i32, attributes: i32) -> Body {
        let body = Body::default().i32(version).i32(1).i32(1);
        let body = if version > 1 {
            body.i32(1).f64(100.0).f64(1.0).ascii("SPY")
        } else {
            body.ascii("SPY").f64(100.0).f64(1.0)
        };
        let body = body.f64(450.0).i32(1).i32(20500).f64(1.57).i32(1).i32(20560);
        let body = body.f64(0.0131).f64(0.0433).f64(451.2).f64(0.011).f64(0.0115);
        let body = if version > 3 { body.i32(5) } else { body };
        let body = body.i32(1_790_000_000);
        let body = if version > 2 { body.i32(0) } else { body };
        if version > 3 { body.i32(attributes) } else { body }
    }

    #[test]
    fn a_set_is_read_whole() {
        let sets = parse(&one_set(4, 5).record()).expect("a record the venue could send");
        assert_eq!(
            sets,
            [ChainModelParameters {
                product_id: 1,
                classes: vec![("SPY".into(), vec![100.0])],
                underlying_price: Some(450.0),
                dividends: vec![("20260216".into(), 1.57)],
                index_style_dividends: false,
                terms: vec![ChainModelTerm {
                    last_trade_date: "20260417".into(),
                    model_yield: 0.0131,
                    interest_rate: 0.0433,
                    forward: 451.2,
                    call_atm_vol: 0.011,
                    put_atm_vol: 0.0115,
                    attributes: 5,
                }],
                timestamp_millis: 1_790_000_000_000,
                attributes: 5,
            }],
        );
    }

    /// The underlying's price is stated either way, and stands only where the
    /// flags say it does.
    #[test]
    fn a_price_the_flags_do_not_stand_behind_is_not_one() {
        let sets = parse(&one_set(4, 1).record()).unwrap();
        assert_eq!(sets[0].underlying_price, None);
    }

    /// The earlier versions state less, and what they leave out takes the
    /// value the venue's own reader gives it.
    #[test]
    fn each_version_reads_only_what_it_states() {
        for version in [1, 2, 3] {
            let sets = parse(&one_set(version, 0).record())
                .unwrap_or_else(|| panic!("version {version} is read"));
            assert_eq!(sets[0].classes, [("SPY".to_string(), vec![100.0])], "v{version}");
            assert_eq!(sets[0].attributes, 1, "v{version} states no flags, so the set stands");
            assert_eq!(sets[0].underlying_price, None, "and says nothing for the price");
            assert_eq!(sets[0].terms[0].attributes, 1, "v{version}");
            assert_eq!(sets[0].timestamp_millis, 1_790_000_000_000, "v{version}");
        }
    }

    /// Classes named against more than one multiplier are one class with each
    /// of them, and a list stated as nothing names none.
    #[test]
    fn a_class_carries_every_multiplier_named_for_it() {
        let body = Body::default().i32(4).i32(1).i32(9).i32(3);
        let body = body.f64(100.0).f64(1.0).ascii("SPY,SPXW,");
        let body = body.f64(10.0).f64(1.0).ascii("SPY");
        let body = body.f64(1.0).f64(0.0).i32(-1);
        let body = body.f64(0.0).i32(0).i32(0).i32(0).i32(1).i32(0);
        let sets = parse(&body.record()).unwrap();
        assert_eq!(
            sets[0].classes,
            [("SPY".to_string(), vec![100.0, 10.0]), ("SPXW".to_string(), vec![100.0])],
        );
        assert!(sets[0].index_style_dividends);
    }

    /// The first version states one multiplier for the whole list, so a class
    /// the list names twice is a record that cannot be read.
    #[test]
    fn a_class_named_twice_under_one_multiplier_states_nothing() {
        let body = Body::default().i32(1).i32(1).i32(9).ascii("SPY,SPY").f64(100.0).f64(1.0);
        let body = body.f64(450.0).i32(0).i32(0).i32(1_790_000_000);
        assert_eq!(parse(&body.record()), None);
    }

    /// A list's empty members at its end are not members; a list of nothing
    /// at all is one empty member, and one of commas alone is none.
    #[test]
    fn a_list_is_split_the_way_its_writer_splits_it() {
        fn split(named: &str) -> Vec<&str> {
            split_list(named).collect()
        }
        assert_eq!(split("SPY,SPXW,,"), ["SPY", "SPXW"]);
        assert_eq!(split(",SPY"), ["", "SPY"]);
        assert_eq!(split(""), [""]);
        assert!(split(",,,").is_empty());
    }

    /// Nothing is kept from a record that cannot be read to its end.
    #[test]
    fn a_record_that_cannot_be_read_states_nothing() {
        let whole = one_set(4, 5);
        let mut short = Body(whole.0.clone());
        short.0.truncate(whole.0.len() - 1);
        assert_eq!(parse(&short.record()), None, "a stream that stops short");
        let compressed = one_set(4, 5).record();
        assert_eq!(parse(&compressed[..compressed.len() - 6]), None, "a stream cut off");
        assert_eq!(parse(&[0x01, 0x02, 0x03]), None, "no compressed stream at all");
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&Body::default().i32(4).i32(-1).record()), None, "a count below nought");
        assert_eq!(parse(&Body::default().i32(4).i32(0).record()), Some(Vec::new()));
    }
}
