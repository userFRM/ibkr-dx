use super::model::{Inputs, calculate};
use super::{
    Dividend, RatePoint, continuous_rate, dividend_present_value, rate_at, rate_term,
    time_to_expiry,
};

// Each cell is an unsigned binary64 bit pattern, including unset values.
fn rows(data: &str) -> impl Iterator<Item = Vec<f64>> + '_ {
    data.lines().map(|line| line.split(',').map(|s| f64::from_bits(s.parse().unwrap())).collect())
}

fn distance(a: f64, b: f64) -> u64 {
    fn ordered(x: f64) -> u64 {
        let bits = x.to_bits();
        if bits >> 63 != 0 { !bits } else { bits | (1 << 63) }
    }
    if a == b || a.is_nan() && b.is_nan() { 0 } else { ordered(a).abs_diff(ordered(b)) }
}

#[test]
fn model_grid() {
    // Columns: call, American, price-based, spot, strike, years, rate, yield,
    // volatility, index schedule, tax adjustment, forward, quote selector,
    // futures premium, quanto, expiry day, and two (ex-day, milliseconds, amount)
    // dividends; then price, delta, gamma, vega, theta, underlying, dividend PV,
    // forward, intrinsic value, time value and uncapped theta.
    // Budgets count ulps at max(1, abs(expected)). Finite differences can lose
    // many ulps near zero, especially vega's subtraction of two tree prices.
    const BUDGET: [f64; 11] = [64.0, 32.0, 1.0, 16384.0, 16.0, 0.0, 1.0, 2.0, 4.0, 64.0, 16.0];
    let mut maxima = [0_u64; 11];
    let mut absolute = [0.0_f64; 11];
    let mut worst = [0; 11];
    let mut failures = 0;
    let mut scaled = [0.0_f64; 11];
    for (row, f) in rows(include_str!("fixtures/grid.csv")).enumerate() {
        let dividends = [
            Dividend {
                ex_day: f[16] as i32,
                millis_to_ex_date: f[17] as i64,
                millis_from_today: f[16] as i64 * 86400000,
                days_to_end_of_ex_date: f[16] as i64,
                amount: f[18],
            },
            Dividend {
                ex_day: f[19] as i32,
                millis_to_ex_date: f[20] as i64,
                millis_from_today: f[19] as i64 * 86400000,
                days_to_end_of_ex_date: f[19] as i64,
                amount: f[21],
            },
        ];
        let pv = dividend_present_value(&dividends, f[15] as i32, f[6], f[10], f[9] == 1.0);
        let input = Inputs {
            is_call: f[0] == 1.0,
            american: f[1] == 1.0,
            price_based: f[2] == 1.0,
            spot: f[3],
            strike: f[4],
            years: f[5],
            rate: f[6],
            yield_rate: f[7],
            volatility: f[8],
            index_dividends: f[9] == 1.0,
            tax_adjustment: f[10],
            forward: f[11],
            futures_style: f[13] == 1.0,
            quanto: f[14] == 1.0,
            dividends: &dividends,
            dividend_pv: pv,
        };
        let value = calculate(&input, (f[12] == 1.0).then_some(0.001));
        let actual = [
            value.price,
            value.delta,
            value.gamma,
            value.vega,
            value.theta,
            value.underlying_price,
            value.dividend_pv,
            value.forward,
            value.intrinsic_value,
            value.time_value,
            value.uncapped_theta,
        ];
        for i in 0..actual.len() {
            let expected = f[i + 22];
            let ulps = distance(actual[i], expected);
            if actual[i].is_finite() && expected.is_finite() {
                let scale = expected.abs().max(1.0);
                let unit = f64::from_bits(scale.to_bits() + 1) - scale;
                scaled[i] = scaled[i].max((actual[i] - expected).abs() / unit);
            }
            if ulps > maxima[i] {
                maxima[i] = ulps;
                worst[i] = row + 1;
            }
            absolute[i] = absolute[i].max((actual[i] - expected).abs());
            let scale = expected.abs().max(1.0);
            let unit = f64::from_bits(scale.to_bits() + 1) - scale;
            let matches = if expected.is_nan() {
                actual[i].is_nan()
            } else if expected.is_infinite() {
                actual[i] == expected
            } else {
                actual[i].is_finite() && (actual[i] - expected).abs() <= BUDGET[i] * unit
            };
            if !matches {
                if failures < 20 {
                    eprintln!(
                        "row {} output {i}: {} != {expected} ({ulps} ulps)",
                        row + 1,
                        actual[i]
                    );
                }
                failures += 1;
            }
        }
    }
    eprintln!(
        "maximum ulps: {maxima:?}\nmaximum absolute: {absolute:?}\nworst rows: {worst:?}\nnormalized ulps: {scaled:?}"
    );
    assert_eq!(failures, 0);
}

#[test]
fn dividend_fixtures() {
    let dividends = [-2, 0, 1, 20, 80, 200].map(|day| Dividend {
        ex_day: day,
        millis_to_ex_date: day as i64 * 86400000 - 43200000,
        millis_from_today: day as i64 * 86400000,
        days_to_end_of_ex_date: day as i64,
        amount: 3.0 + (day + 2) as f64 * 0.1,
    });
    for f in rows(include_str!("fixtures/dividends.csv")) {
        let actual = dividend_present_value(&dividends, f[3] as i32, f[2], f[1], f[0] == 1.0);
        assert!(distance(actual, f[4]) <= 4, "{f:?}: {actual}");
    }
}

#[test]
fn calendar_dividend_fixtures() {
    // The schedule carries elapsed milliseconds and calendar day decisions
    // separately, including midnight and daylight-saving transitions.
    let mut maxima = [0.0_f64; 11];
    for f in rows(include_str!("fixtures/calendar.csv")) {
        let schedule = [Dividend {
            ex_day: f[3] as i32,
            millis_to_ex_date: f[4] as i64,
            millis_from_today: f[5] as i64,
            days_to_end_of_ex_date: f[6] as i64,
            amount: 3.0,
        }];
        let mut input = terms();
        input.is_call = f[0] == 1.0;
        input.american = f[1] == 1.0;
        input.years = f[2];
        input.dividends = &schedule;
        input.dividend_pv = dividend_present_value(&schedule, f[7] as i32, input.rate, 1.0, false);
        let v = calculate(&input, None);
        let actual = [
            v.price,
            v.delta,
            v.gamma,
            v.vega,
            v.theta,
            v.underlying_price,
            v.dividend_pv,
            v.forward,
            v.intrinsic_value,
            v.time_value,
            v.uncapped_theta,
        ];
        for (i, a) in actual.into_iter().enumerate() {
            let expected = f[i + 8];
            let scale = expected.abs().max(1.0);
            let unit = f64::from_bits(scale.to_bits() + 1) - scale;
            let error = (a - expected).abs() / unit;
            maxima[i] = maxima[i].max(error);
            assert_eq!(a.to_bits(), expected.to_bits(), "output {i}, inputs {f:?}");
        }
    }
    eprintln!("calendar normalized ulps: {maxima:?}");
}

fn terms() -> Inputs<'static> {
    Inputs {
        is_call: true,
        american: false,
        price_based: false,
        spot: 100.0,
        strike: 100.0,
        years: 0.25,
        rate: 0.03,
        yield_rate: 0.0,
        volatility: 0.2,
        dividend_pv: 0.0,
        dividends: &[],
        index_dividends: false,
        tax_adjustment: 1.0,
        forward: f64::NAN,
        futures_style: false,
        quanto: false,
    }
}

#[test]
fn ten_minute_floor_stops_at_expiry() {
    assert_eq!(time_to_expiry(-1, false), 0.0);
    assert_eq!(time_to_expiry(0, false), time_to_expiry(600000, false));
    assert_eq!(time_to_expiry(599999, false), time_to_expiry(600000, false));
    assert!(time_to_expiry(600001, false) > time_to_expiry(600000, false));
}

#[test]
fn date_only_expiry_adds_sixteen_hours() {
    assert_eq!(time_to_expiry(0, true), 16.0 / 8760.0);
    assert_eq!(time_to_expiry(-57600001, true), 0.0);
    assert_eq!(time_to_expiry(-57600000, true), time_to_expiry(0, false));
}

#[test]
fn negative_theta_is_capped_by_the_side_quotes_time_value() {
    let input = terms();
    let value = calculate(&input, None);
    let capped = calculate(&input, Some(value.intrinsic_value + 0.001));
    assert!(capped.uncapped_theta < -0.001);
    assert_eq!(capped.theta, -capped.time_value);
    assert_eq!(capped.price, value.price);
    assert_eq!(capped.delta, value.delta);
    let below_intrinsic = calculate(&input, Some(0.0));
    assert_eq!(below_intrinsic.theta, 0.0);
}

#[test]
fn lognormal_calls_require_a_dividend_for_early_exercise() {
    let mut input = terms();
    input.american = true;
    input.spot = 150.0;
    input.yield_rate = 0.5;
    let without = calculate(&input, None);
    // Continuous yield alone does not enable the call's exercise check.
    assert!(without.price < input.spot - input.strike - 1.0);
    let schedule = [Dividend {
        ex_day: 30,
        millis_to_ex_date: 30 * 86400000,
        millis_from_today: 30 * 86400000,
        days_to_end_of_ex_date: 30,
        amount: 3.0,
    }];
    input.dividends = &schedule;
    let before = calculate(&input, None);
    assert!(before.price >= input.spot - input.strike - 1e-11);
    let mut after = schedule;
    after[0].millis_to_ex_date = 180 * 86400000;
    after[0].millis_from_today = 180 * 86400000;
    after[0].ex_day = 180;
    after[0].days_to_end_of_ex_date = 180;
    input.dividends = &after;
    assert_eq!(calculate(&input, None).price, without.price);
}

#[test]
fn arithmetic_calls_can_exercise_without_a_dividend() {
    let mut input = terms();
    input.american = true;
    input.price_based = true;
    input.spot = 150.0;
    input.volatility = 20.0;
    assert!(calculate(&input, None).price >= 50.0 - 1e-11);
    input.american = false;
    assert!(calculate(&input, None).price < 50.0);
}

#[test]
fn expiry_fixtures() {
    for f in rows(include_str!("fixtures/time.csv")) {
        assert_eq!(time_to_expiry(f[0] as i64, f[1] == 1.0).to_bits(), f[2].to_bits());
    }
}

#[test]
fn rate_fixtures() {
    assert_eq!(rate_term(0.0), 0.0);
    assert_eq!(rate_term(1.9 / 365.0), 1.0 / 365.0);
    assert_eq!(rate_term(365.9 / 365.0), 1.0);
    for f in rows(include_str!("fixtures/rates.csv")) {
        assert!(distance(continuous_rate(f[0]), f[1]) <= 2);
    }
    for f in rows(include_str!("fixtures/curve.csv")) {
        let points = [
            RatePoint { years: 0.0, rate: f[2] },
            RatePoint { years: 30.0 / 365.0, rate: f[3] },
            RatePoint { years: 1.0, rate: f[4] },
        ];
        let curve = if f[0] == 3.0 { &[][..] } else { &points[..] };
        assert_eq!(distance(rate_at(curve, f[1]), f[5]), 0);
    }
    for f in rows(include_str!("fixtures/rounding.csv")) {
        let points = [RatePoint { years: f[0], rate: f[1] }, RatePoint { years: f[2], rate: f[3] }];
        assert_eq!(distance(rate_at(&points, f[4]), f[5]), 0);
    }
}
