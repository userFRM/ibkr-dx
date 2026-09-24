//! The venue's answer to a chargeable snapshot.
//!
//! One record of fixed layout, told apart by its length: 72 bytes in the older
//! layout, 96 in the newer, and 136 where the newer one carries the odd lot
//! beside it. Any other length is not a layout the venue states, and nothing
//! is read from it.
//!
//! A side of the quote stands only where the record says it does: in the
//! older layout by a flag beside it, in the newer by a size that is stated and
//! above nought. Sizes are counts of the contract's size increment, as every
//! size the venue states is. Where each side is quoted is a mask over the
//! venue's list of exchanges for the bid and the ask, and one exchange's place
//! in that list for the last.

/// What the answer states, side by side.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SnapshotAnswer {
    /// The bid's price and size, where it stands.
    pub bid: Option<(f64, f64)>,
    /// The ask's price and size, where it stands.
    pub ask: Option<(f64, f64)>,
    /// The last trade's price and size, where it stands.
    pub last: Option<(f64, f64)>,
    /// The exchanges the bid is quoted on, as a mask; nought for none.
    pub bid_exchanges: i64,
    /// The exchanges the ask is quoted on, as a mask; nought for none.
    pub ask_exchanges: i64,
    /// The exchange the last traded on, as its place in the list.
    pub last_exchange: Option<u32>,
    /// The session's high.
    pub high: Option<f64>,
    /// The session's low.
    pub low: Option<f64>,
    /// The previous close.
    pub close: Option<f64>,
    /// The session's volume, counted in the contract's size increment.
    pub volume: Option<f64>,
    /// The odd lot's bid, price and size, where it stands.
    pub odd_bid: Option<(f64, f64)>,
    /// The odd lot's ask, price and size, where it stands.
    pub odd_ask: Option<(f64, f64)>,
    /// The exchanges the odd lot's bid is quoted on, as a mask.
    pub odd_bid_exchanges: i64,
    /// The exchanges the odd lot's ask is quoted on, as a mask.
    pub odd_ask_exchanges: i64,
}

/// Read an answer. `None` where its length is not one of the three layouts.
pub fn parse(payload: &[u8]) -> Option<SnapshotAnswer> {
    match payload.len() {
        72 => older(payload),
        96 => newer(payload, false),
        136 => newer(payload, true),
        _ => None,
    }
}

/// The older layout: single-precision prices, whole sizes, and a flag per
/// group saying whether the group stands.
fn older(p: &[u8]) -> Option<SnapshotAnswer> {
    let bid = f32_at(p, 0)?;
    let ask = f32_at(p, 4)?;
    let bid_size = i32_at(p, 8)?;
    let ask_size = i32_at(p, 12)?;
    let quoted = i32_at(p, 16)? > 0;
    let bid_exchanges = i32_at(p, 20)?;
    let ask_exchanges = i32_at(p, 24)?;
    let last = f32_at(p, 28)?;
    let last_size = i32_at(p, 32)?;
    let traded = i32_at(p, 36)? > 0;
    // Eight bytes the venue states nothing this reads with.
    let last_exchanges = i32_at(p, 48)?;
    let rest = i32_at(p, 52)? > 0;
    let close = f32_at(p, 56)?;
    let high = f32_at(p, 60)?;
    let low = f32_at(p, 64)?;
    let volume = i32_at(p, 68)?;
    let extremes = rest && high >= low;
    Some(SnapshotAnswer {
        bid: quoted.then_some((bid, f64::from(bid_size))),
        ask: quoted.then_some((ask, f64::from(ask_size))),
        last: traded.then_some((last, f64::from(last_size))),
        bid_exchanges: if quoted { i64::from(bid_exchanges) } else { 0 },
        ask_exchanges: if quoted { i64::from(ask_exchanges) } else { 0 },
        // The last's exchanges are a mask here, and the one it names is the
        // highest bit set.
        last_exchange: (traded && last_exchanges >= 1).then(|| last_exchanges.ilog2()),
        high: extremes.then_some(high),
        low: extremes.then_some(low),
        close: rest.then_some(close),
        volume: rest.then_some(f64::from(volume)),
        ..SnapshotAnswer::default()
    })
}

/// The newer layout: double-precision prices, decimal sizes, and a side that
/// stands where its size is stated and above nought.
fn newer(p: &[u8], odd_lot: bool) -> Option<SnapshotAnswer> {
    let close_stands = i32_at(p, 0)? & 1 != 0;
    let bid_exchanges = i32_at(p, 4)?;
    let ask_exchanges = i32_at(p, 8)?;
    let last_exchange = i32_at(p, 12)?;
    let bid_size = decimal64(u64_at(p, 16)?);
    let ask_size = decimal64(u64_at(p, 24)?);
    let last_size = decimal64(u64_at(p, 32)?);
    let volume = decimal64(u64_at(p, 40)?);
    let (bid, ask, last) = (f64_at(p, 48)?, f64_at(p, 56)?, f64_at(p, 64)?);
    let (close, high, low) = (f64_at(p, 72)?, f64_at(p, 80)?, f64_at(p, 88)?);
    let stands = |size: Option<f64>| size.filter(|s| *s > 0.0);
    let (bid_size, ask_size, last_size) = (stands(bid_size), stands(ask_size), stands(last_size));
    // A high and a low both of nought are no extremes at all, and a low above
    // the high is not a range. Anything else stands, as it does for a
    // gateway: a figure that is not a number is not above the other.
    let extremes =
        !(high == 0.0 && low == 0.0) && low.partial_cmp(&high) != Some(std::cmp::Ordering::Greater);
    let mut answer = SnapshotAnswer {
        bid: bid_size.map(|size| (bid, size)),
        ask: ask_size.map(|size| (ask, size)),
        last: last_size.map(|size| (last, size)),
        bid_exchanges: if bid_size.is_some() { i64::from(bid_exchanges) } else { 0 },
        ask_exchanges: if ask_size.is_some() { i64::from(ask_exchanges) } else { 0 },
        last_exchange: last_size.and_then(|_| u32::try_from(last_exchange).ok()),
        high: extremes.then_some(high),
        low: extremes.then_some(low),
        close: close_stands.then_some(close),
        volume: volume.filter(|v| *v >= 0.0),
        ..SnapshotAnswer::default()
    };
    if odd_lot {
        let odd_bid_size = stands(decimal64(u64_at(p, 104)?));
        let odd_ask_size = stands(decimal64(u64_at(p, 112)?));
        let (odd_bid, odd_ask) = (f64_at(p, 120)?, f64_at(p, 128)?);
        answer.odd_bid = odd_bid_size.map(|size| (odd_bid, size));
        answer.odd_ask = odd_ask_size.map(|size| (odd_ask, size));
        if odd_bid_size.is_some() {
            answer.odd_bid_exchanges = i64::from(i32_at(p, 96)?);
        }
        if odd_ask_size.is_some() {
            answer.odd_ask_exchanges = i64::from(i32_at(p, 100)?);
        }
    }
    Some(answer)
}

/// An IEEE 754-2008 decimal64 in its binary-integer encoding, as the venue
/// states a size. `None` for the encoding's not-a-number.
///
/// The encoding's infinity is read as the venue's own reader holds it: as the
/// most negative whole number, which no size that stands can be.
pub fn decimal64(bits: u64) -> Option<f64> {
    const NAN: u64 = 0x7C00_0000_0000_0000;
    const INFINITY: u64 = 0x7800_0000_0000_0000;
    const STEERED: u64 = 0x6000_0000_0000_0000;
    const BIAS: i32 = 398;
    if bits & NAN == NAN {
        return None;
    }
    if bits & INFINITY == INFINITY {
        return Some(i64::MIN as f64);
    }
    let (coefficient, exponent) = if bits & STEERED == STEERED {
        (
            (bits & 0x0007_FFFF_FFFF_FFFF) | 0x0020_0000_0000_0000,
            (bits & 0x1FF8_0000_0000_0000) >> 51,
        )
    } else {
        (bits & 0x001F_FFFF_FFFF_FFFF, (bits & 0x7FE0_0000_0000_0000) >> 53)
    };
    let scale = exponent as i32 - BIAS;
    let magnitude = coefficient as f64;
    let value =
        if scale >= 0 { magnitude * 10f64.powi(scale) } else { magnitude / 10f64.powi(-scale) };
    Some(if bits & 0x8000_0000_0000_0000 != 0 { -value } else { value })
}

fn i32_at(p: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(p.get(at..at + 4)?.try_into().ok()?))
}

fn f32_at(p: &[u8], at: usize) -> Option<f64> {
    Some(f64::from(f32::from_be_bytes(p.get(at..at + 4)?.try_into().ok()?)))
}

fn u64_at(p: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(p.get(at..at + 8)?.try_into().ok()?))
}

fn f64_at(p: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_be_bytes(p.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decimal64 with coefficient `c` and exponent `e`, in the plain form.
    fn plain(c: u64, e: i32) -> u64 {
        ((e + 398) as u64) << 53 | c
    }

    #[test]
    fn a_decimal_size_is_read_as_the_venue_encodes_it() {
        assert_eq!(decimal64(0x31C0_0000_0000_0064), Some(100.0));
        assert_eq!(decimal64(plain(100, 0)), Some(100.0));
        assert_eq!(decimal64(plain(15, -1)), Some(1.5));
        assert_eq!(decimal64(plain(1, -8)), Some(0.000_000_01));
        assert_eq!(decimal64(plain(3, 2)), Some(300.0));
        assert_eq!(decimal64(plain(7, 0) | 0x8000_0000_0000_0000), Some(-7.0));
        // The steered form: the coefficient's top bits are implied.
        let steered = 0x6000_0000_0000_0000 | (398u64 << 51) | 1;
        assert_eq!(decimal64(steered), Some(9_007_199_254_740_993.0));
        assert_eq!(decimal64(0x7C00_0000_0000_0000), None, "not a number");
        assert_eq!(decimal64(0x7E00_0000_0000_0000), None, "the signalling one too");
        assert_eq!(decimal64(0x7800_0000_0000_0000), Some(i64::MIN as f64));
    }

    /// The newer layout, built field by field.
    fn newer_answer(odd_lot: bool) -> Vec<u8> {
        let mut p = Vec::new();
        for v in [1i32, 0b101, 0b10, 3] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        for size in [plain(3, 0), plain(4, 0), plain(1, 0), plain(1_234_567, 0)] {
            p.extend_from_slice(&size.to_be_bytes());
        }
        for price in [150.25f64, 150.30, 150.27, 149.0, 151.5, 148.75] {
            p.extend_from_slice(&price.to_be_bytes());
        }
        if odd_lot {
            for v in [0b1000i32, 0] {
                p.extend_from_slice(&v.to_be_bytes());
            }
            for size in [plain(5, -1), plain(0, 0)] {
                p.extend_from_slice(&size.to_be_bytes());
            }
            for price in [150.24f64, 150.31] {
                p.extend_from_slice(&price.to_be_bytes());
            }
        }
        p
    }

    #[test]
    fn the_newer_layout_is_read_field_by_field() {
        let answer = parse(&newer_answer(false)).expect("ninety-six bytes");
        assert_eq!(
            answer,
            SnapshotAnswer {
                bid: Some((150.25, 3.0)),
                ask: Some((150.30, 4.0)),
                last: Some((150.27, 1.0)),
                bid_exchanges: 0b101,
                ask_exchanges: 0b10,
                last_exchange: Some(3),
                high: Some(151.5),
                low: Some(148.75),
                close: Some(149.0),
                volume: Some(1_234_567.0),
                ..SnapshotAnswer::default()
            },
        );
        let with_odd_lot = parse(&newer_answer(true)).expect("a hundred and thirty-six bytes");
        assert_eq!(with_odd_lot.odd_bid, Some((150.24, 0.5)));
        assert_eq!(with_odd_lot.odd_bid_exchanges, 0b1000);
        assert_eq!(with_odd_lot.odd_ask, None, "a size of nought does not stand");
        assert_eq!(with_odd_lot.odd_ask_exchanges, 0);
    }

    #[test]
    fn a_side_stands_only_where_the_answer_says_it_does() {
        let mut p = newer_answer(false);
        // No size on the bid, and a low above the high.
        p[16..24].copy_from_slice(&0x7C00_0000_0000_0000u64.to_be_bytes());
        p[80..88].copy_from_slice(&100.0f64.to_be_bytes());
        // The close flag down.
        p[0..4].copy_from_slice(&0i32.to_be_bytes());
        let answer = parse(&p).unwrap();
        assert_eq!((answer.bid, answer.bid_exchanges), (None, 0));
        assert_eq!((answer.high, answer.low), (None, None));
        assert_eq!(answer.close, None);
        assert!(answer.ask.is_some());
    }

    /// A high and a low both of nought are no extremes, and neither is
    /// published; one that is not a number beside a stated one leaves the
    /// stated one standing, as a gateway's reader leaves it.
    #[test]
    fn extremes_of_nought_state_nothing() {
        let mut p = newer_answer(false);
        p[80..88].copy_from_slice(&0.0f64.to_be_bytes());
        p[88..96].copy_from_slice(&0.0f64.to_be_bytes());
        let answer = parse(&p).unwrap();
        assert_eq!((answer.high, answer.low), (None, None));

        p[80..88].copy_from_slice(&151.5f64.to_be_bytes());
        p[88..96].copy_from_slice(&f64::NAN.to_be_bytes());
        let answer = parse(&p).unwrap();
        assert_eq!(answer.high, Some(151.5));
        assert!(answer.low.is_some_and(f64::is_nan));
    }

    #[test]
    fn the_older_layout_is_read_by_its_flags() {
        let mut p = Vec::new();
        p.extend_from_slice(&150.25f32.to_be_bytes());
        p.extend_from_slice(&150.5f32.to_be_bytes());
        for v in [3i32, 4, 1, 0b100, 0b1] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        p.extend_from_slice(&150.375f32.to_be_bytes());
        for v in [2i32, 1] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        p.extend_from_slice(&[0; 8]);
        for v in [0b1000i32, 1] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        for price in [149.0f32, 151.5, 148.75] {
            p.extend_from_slice(&price.to_be_bytes());
        }
        p.extend_from_slice(&1000i32.to_be_bytes());
        let answer = parse(&p).expect("seventy-two bytes");
        assert_eq!(
            answer,
            SnapshotAnswer {
                bid: Some((150.25, 3.0)),
                ask: Some((150.5, 4.0)),
                last: Some((150.375, 2.0)),
                bid_exchanges: 0b100,
                ask_exchanges: 0b1,
                last_exchange: Some(3),
                high: Some(151.5),
                low: Some(148.75),
                close: Some(149.0),
                volume: Some(1000.0),
                ..SnapshotAnswer::default()
            },
        );
    }

    #[test]
    fn an_answer_of_no_layout_states_nothing() {
        for len in [0, 1, 71, 73, 95, 97, 135, 137] {
            assert_eq!(parse(&vec![0; len]), None, "{len} bytes");
        }
    }
}
