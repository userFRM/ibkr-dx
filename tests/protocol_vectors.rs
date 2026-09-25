//! Protocol test vectors.
//!
//! These run the client's own decoders over captured bytes. They used to run
//! copies of those decoders, written out here and drifted from the originals —
//! different signatures on two of them — so the file proved only that the
//! copies agreed with themselves and no change to the real ones could fail it.
//!
//! Sources: reference test vectors (VLQ, hibit strings, bar captures)

use ibkr_dx::control::historical::decode_bar_payload;
use ibkr_dx::protocol::tick_decoder::{read_hibit_str, read_vlq, vlq_signed};

// ============================================================
// VLQ (Variable-Length Quantity) decoder
// ============================================================
// IB uses VLQ encoding for tick-by-tick data. Bit 7 (0x80) marks the last byte.
// Multi-byte values: bits [6:0] concatenated, MSB first.

// --- VLQ tests (from reference test suite) ---

#[test]
fn vlq_single_byte() {
    // 0x88 → bit7=1 (last), value = 8
    let (val, n) = read_vlq(&[0x88], 0);
    assert_eq!(val, 8);
    assert_eq!(n, 1);
}

#[test]
fn vlq_multi_byte() {
    // 0x01 0x4d 0xe7 → 26343 (263.43 cents)
    let (val, n) = read_vlq(&[0x01, 0x4d, 0xe7], 0);
    assert_eq!(val, 26343);
    assert_eq!(n, 3);
}

#[test]
fn vlq_signed_positive() {
    assert_eq!(vlq_signed(8, 1), 8);
}

#[test]
fn vlq_signed_negative() {
    // 122 with 1 byte: 122 >= 64 → 122 - 128 = -6
    assert_eq!(vlq_signed(122, 1), -6);
}

#[test]
fn vlq_signed_zero() {
    assert_eq!(vlq_signed(0, 1), 0);
}

// --- Hi-bit string tests ---

#[test]
fn hibit_str_finra() {
    // FINRA = 46 49 4e 52 c1 (last char A=0x41|0x80)
    let (s, pos) = read_hibit_str(&[0x46, 0x49, 0x4e, 0x52, 0xc1], 0);
    assert_eq!(s, "FINRA");
    assert_eq!(pos, 5);
}

#[test]
fn hibit_str_single_char() {
    // 0xC9 = 'I' | 0x80
    let (s, _) = read_hibit_str(&[0xc9], 0);
    assert_eq!(s, "I");
}

#[test]
fn hibit_str_arca() {
    // "ARCA" = 0x41 0x52 0x43 0xc1
    let (s, pos) = read_hibit_str(&[0x41, 0x52, 0x43, 0xc1], 0);
    assert_eq!(s, "ARCA");
    assert_eq!(pos, 4);
}

// ============================================================
// Real-time bar captures (RTBAR)
// ============================================================
// Binary payload test vectors.
// Format: 8=O\x019=0043\x0135=G\x01 [binary OHLCV data]

/// One bar as the capture states it: time, open, high, low, close, volume, count.
type RtBar = (u32, f64, f64, f64, f64, u32, u32);

/// Known RTBAR captures, each raw frame with the bar it decodes to.
const RTBAR_CAPTURES: &[(&[u8], RtBar)] = &[
    (
        b"8=O\x019=0043\x0135=G\x01\
          \x00\xa8\x00\x00\x00\x01\x69\xa7\x01\xf2\
          \x0c\x0c\xd6\x20\xda\xd3\x18\x30\x00\x02\x5b\x81\x3a",
        (1772552690, 262.90, 262.95, 262.89, 262.95, 603, 6),
    ),
    (
        b"8=O\x019=0043\x0135=G\x01\
          \x00\xa8\x00\x00\x00\x01\x69\xa7\x01\xf7\
          \x0c\x0c\xd6\x03\x1b\xd1\x98\xb0\x00\x0f\x14\x86\x61",
        (1772552695, 262.93, 262.94, 262.88, 262.91, 3860, 24),
    ),
    (
        b"8=O\x019=0043\x0135=G\x01\
          \x00\xa8\x00\x00\x00\x01\x69\xa7\x01\xfc\
          \x0c\x0c\xd6\xa5\x3c\xfe\x70\x30\x00\x14\x51\xa4\x9f",
        (1772552700, 262.94, 263.21, 262.93, 263.21, 5201, 41),
    ),
    (
        b"8=O\x019=0043\x0135=G\x01\
          \x00\xa8\x00\x00\x00\x01\x69\xa7\x02\x01\
          \x0c\x0c\xd8\x06\xdd\x50\x66\x50\x00\x23\x2d\xb6\xf7",
        (1772552705, 263.22, 263.29, 263.04, 263.04, 9005, 54),
    ),
    (
        b"8=O\x019=0043\x0135=G\x01\
          \x00\xa8\x00\x00\x00\x01\x69\xa7\x02\x06\
          \x0c\x0c\xd6\xc3\x5e\x90\x31\x30\x00\x10\x64\x8c\x2b",
        (1772552710, 263.03, 263.06, 262.94, 262.94, 4196, 26),
    ),
    (
        b"8=O\x019=0043\x0135=G\x01\
          \x00\xa8\x00\x00\x00\x01\x69\xa7\x02\x0b\
          \x0c\x0c\xd5\x83\x7f\x53\xa5\x30\x00\x15\x39\x8c\xa6",
        (1772552715, 262.93, 262.93, 262.84, 262.91, 5433, 27),
    ),
];

#[test]
fn rtbar_captures_decode_to_their_bars() {
    for (raw, (time, open, high, low, close, volume, count)) in RTBAR_CAPTURES {
        // Behind the header: a ticker id, the bar's time, then the payload's
        // length and the payload.
        let body = raw.strip_prefix(b"8=O\x019=0043\x0135=G\x01").expect("header");
        assert_eq!(u32::from_be_bytes(body[6..10].try_into().unwrap()), *time);
        let payload = &body[11..11 + body[10] as usize];
        let bar = decode_bar_payload(payload, 0.01, 1.0).expect("decodes");
        for (got, want) in [
            (bar.open, *open),
            (bar.high, *high),
            (bar.low, *low),
            (bar.close, *close),
        ] {
            assert!((got - want).abs() < 1e-6, "{time}: {got} != {want}");
        }
        assert_eq!(bar.volume, f64::from(*volume), "{time}");
        assert_eq!(bar.count, *count as i32, "{time}");
    }
}
