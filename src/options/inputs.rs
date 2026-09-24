//! Time, rate and dividend arithmetic used by the option calculations.

/// A dividend and its ex-date relative to the calculation's clock.
///
/// Schedules are in ascending ex-date order. Calendar days and elapsed time are
/// separate: the caller resolves the contract's calendar before calculating.
#[derive(Clone, Copy, Debug)]
pub struct Dividend {
    /// Calendar days from today to the ex-date; zero means today.
    pub ex_day: i32,
    /// Milliseconds from the calculation time to the ex-date.
    pub millis_to_ex_date: i64,
    /// Milliseconds from midnight of the calculation date to the ex-date.
    pub millis_from_today: i64,
    /// Complete elapsed days until the end of the ex-date, used by the tree.
    ///
    /// This uses the analytics calendar and can differ from `ex_day` at midnight
    /// or across a clock change.
    pub days_to_end_of_ex_date: i64,
    /// Cash amount, or cumulative amount for an index schedule.
    pub amount: f64,
}

/// A point in a currency's continuous interest rate curve.
#[derive(Clone, Copy, Debug)]
pub struct RatePoint {
    /// Years from the curve's reference time to this point.
    pub years: f64,
    /// Continuously compounded annual rate.
    pub rate: f64,
}

/// Calendar time to expiry, including the close for a date-only expiry.
///
/// Date-only expiries add sixteen hours. Negative times return zero; remaining
/// times below ten minutes return ten minutes. The caller resolves time zones.
pub fn time_to_expiry(milliseconds: i64, date_only: bool) -> f64 {
    let years = milliseconds as f64 / 3.1536e10;
    let years = if date_only { years + 0.0018264840182648401 } else { years };
    if years < 0.0 {
        0.0
    } else if years < 1.9025875190258754e-5 {
        1.9025875190258754e-5
    } else {
        years
    }
}

/// Convert an act/360 percentage quote through quarterly compounding.
pub fn continuous_rate(percent: f64) -> f64 {
    let annual = percent * 365.0 / 36000.0;
    4.0 * (1.0 + annual / 4.0).ln()
}

/// Whole calendar days used to select a currency rate, expressed as years.
pub fn rate_term(years: f64) -> f64 {
    0.0027397260273972603 * (years * 365.0) as i32 as f64
}

/// Day-weighted rate with flat extension of an ordered currency curve.
///
/// An empty curve returns the unset value, `f64::MAX`.
pub fn rate_at(points: &[RatePoint], years: f64) -> f64 {
    if points.is_empty() {
        return f64::MAX;
    }
    let end = years * 365.0;
    let mut average = 0.0;
    let mut previous_day = 0.0;
    let mut previous_rate = 0.0;
    let mut used_day = 0.0;
    for (i, point) in points.iter().enumerate() {
        let days = point.years * 365.0;
        let floor = days.floor();
        let rounded = if days - floor >= 0.5 { floor + 1.0 } else { floor };
        let day = rounded as i64 as i32 as f64;
        used_day = if day > end { end } else { day };
        let span = used_day - previous_day;
        if i == 0 {
            previous_rate = point.rate;
        }
        average = if used_day > 0.0 {
            (previous_rate * span + average * previous_day) / used_day
        } else {
            point.rate
        };
        if day > end {
            return average;
        }
        previous_rate = point.rate;
        previous_day = day;
    }
    if used_day != 0.0 {
        average = (previous_rate * (end - previous_day) + average * previous_day) / end;
    }
    average
}

/// Present value of dividends after today through the expiry's calendar day.
///
/// The tax adjustment is one unless the session enables dividend tax adjustment;
/// a missing ratio (NaN) also means one.
/// Index schedules contain cumulative amounts; their value is the discounted
/// last eligible amount less the last amount on or before the calculation time.
pub fn dividend_present_value(
    schedule: &[Dividend],
    expiry_day: i32,
    rate: f64,
    tax_adjustment: f64,
    index: bool,
) -> f64 {
    let tax_adjustment = tax_factor(tax_adjustment);
    if index {
        let before = schedule.iter().rfind(|d| d.millis_from_today <= 0);
        let Some(last) = schedule.iter().rfind(|d| d.ex_day > 0 && d.ex_day <= expiry_day) else {
            return 0.0;
        };
        let mut amount = last.amount * tax_adjustment;
        if let Some(before) = before {
            let between = integer_years(
                last.millis_from_today.wrapping_sub(before.millis_from_today) as f64 / 3.1536e10,
            );
            amount -= before.amount * tax_adjustment * (rate * between).exp();
        }
        if amount == 0.0 {
            return 0.0;
        }
        return amount * (-rate * integer_years(last.millis_from_today as f64 / 3.1536e10)).exp();
    }
    let mut pv = 0.0;
    for dividend in schedule {
        if dividend.ex_day > 0 && dividend.ex_day <= expiry_day {
            pv += dividend.amount
                * tax_adjustment
                * (-rate * integer_years(dividend.millis_from_today as f64 / 3.1536e10)).exp();
        }
    }
    pv
}

pub(super) fn integer_years(years: f64) -> f64 {
    (years * 365.0).ceil() / 365.0
}

pub(super) fn tax_factor(ratio: f64) -> f64 {
    if ratio.is_nan() { 1.0 } else { ratio }
}
