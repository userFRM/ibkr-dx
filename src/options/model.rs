//! European and American option prices, greeks and time values.
//!
//! Lognormal volatility uses Black-Scholes-Merton or a 100-step binomial tree.
//! Price-based volatility uses the arithmetic closed form or a 60-step tree.
//! Normal probabilities use the five-term Abramowitz-Stegun approximation.

use super::{Dividend, tree};

/// Model inputs after the contract and session's conventions have been applied.
///
/// Volatility and rates are annual. Futures options normally carry equal rate
/// and yield, no dividends, and zero rate and yield for futures-style premiums.
/// A missing yield curve means zero yield; it is not fitted to an option price.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    /// Whether this is a call rather than a put.
    pub is_call: bool,
    /// Whether the contract permits American exercise.
    pub american: bool,
    /// Whether volatility is in price units rather than proportional returns.
    pub price_based: bool,
    /// The selected underlying price.
    pub spot: f64,
    /// Strike price.
    pub strike: f64,
    /// Calendar years to expiry, after the close offset and minimum time rule.
    pub years: f64,
    /// Continuously compounded annual interest rate.
    pub rate: f64,
    /// Continuously compounded annual yield or borrow rate.
    pub yield_rate: f64,
    /// Stated annual volatility.
    pub volatility: f64,
    /// Dividend present value calculated with the option's rate.
    pub dividend_pv: f64,
    /// Discrete dividend schedule in ascending ex-date order.
    pub dividends: &'a [Dividend],
    /// Whether the schedule states cumulative index dividends.
    pub index_dividends: bool,
    /// Schedule tax adjustment, or one when disabled. NaN also means one.
    pub tax_adjustment: f64,
    /// Explicit forward; NaN means none was supplied.
    pub forward: f64,
    /// Whether premiums use futures-style settlement.
    pub futures_style: bool,
    /// Whether the yield uses the quanto adjustment.
    pub quanto: bool,
}

/// Price, greeks and the underlying and dividend figures used in a calculation.
#[derive(Clone, Copy, Debug)]
pub struct Values {
    /// Model price, independent of the quote used to cap theta.
    pub price: f64,
    /// Price sensitivity to the underlying.
    pub delta: f64,
    /// Delta sensitivity to the underlying.
    pub gamma: f64,
    /// Vega per volatility point; the arithmetic tree uses a one-percent bump.
    pub vega: f64,
    /// Theta per calendar day, with negative theta capped at time value.
    /// The zero-volatility arithmetic closed form returns rate times price.
    pub theta: f64,
    /// Theta before the time-value cap.
    pub uncapped_theta: f64,
    /// Underlying price supplied to the model.
    pub underlying_price: f64,
    /// Dividend present value supplied to the model.
    pub dividend_pv: f64,
    /// Forward returned by the pricing method, possibly unset.
    pub forward: f64,
    /// Discounted intrinsic value, including immediate American exercise.
    pub intrinsic_value: f64,
    /// Nonnegative price less intrinsic value, using the supplied quote if any.
    pub time_value: f64,
}

impl Values {
    pub(super) fn empty(input: &Inputs<'_>) -> Self {
        Self {
            price: f64::NAN,
            delta: f64::NAN,
            gamma: f64::NAN,
            vega: f64::NAN,
            theta: f64::NAN,
            uncapped_theta: f64::NAN,
            underlying_price: input.spot,
            dividend_pv: input.dividend_pv,
            forward: input.forward,
            intrinsic_value: f64::NAN,
            time_value: f64::NAN,
        }
    }
}

impl Inputs<'_> {
    pub(super) fn effective_yield(&self) -> f64 {
        if self.quanto {
            let vol = if self.volatility.is_nan() { 0.0 } else { self.volatility };
            2.0 * self.rate - self.yield_rate - vol * vol
        } else {
            self.yield_rate
        }
    }

    pub(super) fn arithmetic_forward(&self) -> f64 {
        if self.forward.is_finite() { self.forward } else { self.spot }
    }

    fn intrinsic(&self) -> f64 {
        let yield_rate = self.effective_yield();
        let yield_discount = if yield_rate == 0.0 { 1.0 } else { (-yield_rate * self.years).exp() };
        let underlying = max(0.0, self.spot * yield_discount - self.dividend_pv);
        let difference = underlying - self.strike * (-self.rate * self.years).exp();
        let mut value = max(0.0, if self.is_call { difference } else { -difference });
        if self.american {
            let immediate =
                if self.is_call { self.spot - self.strike } else { -(self.spot - self.strike) };
            if immediate > 0.0 && value < immediate {
                value = immediate;
            }
        }
        value
    }
}

/// Compute a model price and greeks at the stated volatility.
///
/// A quote selects the side-greek path and supplies the price used for time
/// value and the theta cap. Without one, time value uses the model price.
/// Nonfinite results remain nonfinite; publication validity is the caller's job.
pub fn calculate(input: &Inputs<'_>, quote: Option<f64>) -> Values {
    let mut value = raw(input, false);
    // Only the degenerate American lognormal side path differs from the
    // calculation that returns both price and greeks.
    if quote.is_some()
        && input.american
        && !input.price_based
        && !input.futures_style
        && (input.volatility == 0.0 || input.spot <= 0.0)
    {
        let price = value.price;
        value = raw(input, true);
        value.price = price;
    }
    value.uncapped_theta = value.theta;
    value.intrinsic_value = input.intrinsic();
    value.time_value = max(0.0, quote.unwrap_or(value.price) - value.intrinsic_value);
    if value.theta < 0.0 {
        let theta = -max(0.0, min(value.theta.abs(), value.time_value));
        if theta != value.theta {
            value.theta = theta;
        }
    }
    value
}

fn raw(input: &Inputs<'_>, side: bool) -> Values {
    if input.price_based {
        if input.american { tree::arithmetic(input) } else { arithmetic(input) }
    } else if input.american {
        let mut value = Values::empty(input);
        value.forward = if input.forward.is_nan() {
            (input.spot - input.dividend_pv) * ((input.rate - input.yield_rate) * input.years).exp()
        } else {
            input.forward
        };
        if input.volatility == 0.0 || input.spot <= 0.0 {
            deterministic(input, &mut value);
            if !side || input.futures_style {
                return value;
            }
        }
        if input.futures_style { lognormal(input, value) } else { tree::lognormal(input, value) }
    } else {
        let mut value = Values::empty(input);
        if input.volatility == 0.0 || input.spot <= 0.0 {
            deterministic(input, &mut value);
            value
        } else {
            lognormal(input, value)
        }
    }
}

fn deterministic(input: &Inputs<'_>, value: &mut Values) {
    value.price = input.intrinsic();
    let yield_rate =
        if input.quanto { 2.0 * input.rate - input.yield_rate } else { input.yield_rate };
    let discount = if yield_rate == 0.0 { 1.0 } else { (-yield_rate * input.years).exp() };
    value.vega = 0.0;
    value.gamma = 0.0;
    value.theta = 0.0;
    value.delta = 0.0;
    if value.price > 0.0 {
        value.delta = clamp_delta(if input.is_call { discount } else { -discount });
        if input.is_call {
            let strike = input.strike * (-input.rate * input.years).exp();
            value.theta = (yield_rate * input.spot * discount - input.rate * strike) / 365.0;
        }
    }
}

pub(super) fn arithmetic_zero(input: &Inputs<'_>) -> Values {
    let mut value = Values::empty(input);
    let forward = input.arithmetic_forward();
    let discount = if input.american { 1.0 } else { (-input.rate * input.years).exp() };
    value.price = discount
        * max(if input.is_call { forward - input.strike } else { input.strike - forward }, 0.0);
    let delta = if forward > input.strike { discount } else { 0.0 };
    value.delta = if input.is_call { delta } else { delta - discount };
    value.gamma = 0.0;
    value.vega = 0.0;
    // The zero-volatility arithmetic branch returns rate times price directly.
    value.theta = if input.american { 0.0 } else { input.rate * value.price };
    value.forward = forward;
    value
}

fn arithmetic(input: &Inputs<'_>) -> Values {
    let root_time = input.years.sqrt();
    let width = input.volatility * root_time;
    if width == 0.0 {
        return arithmetic_zero(input);
    }
    let mut value = Values::empty(input);
    let discount = (-input.rate * input.years).exp();
    let forward = input.arithmetic_forward();
    let difference = forward - input.strike;
    let standardized = difference / width;
    let cdf = normal_cdf(standardized);
    let pdf = normal_pdf(standardized);
    let call_price = discount * (difference * cdf + width * pdf);
    value.price = if input.is_call { call_price } else { call_price - discount * difference };
    let delta = discount * cdf;
    value.delta = if input.is_call { delta } else { delta - discount };
    value.gamma = discount / width * pdf;
    value.vega = (discount * root_time * pdf) * 0.01;
    let decay = if root_time.abs() < 1e-6 { 0.0 } else { 0.5 * input.volatility * pdf / root_time };
    let theta = -discount * decay + input.rate * call_price;
    value.theta =
        if input.is_call { theta } else { theta - discount * input.rate * difference } / 365.0;
    value.forward = forward;
    value
}

fn lognormal(input: &Inputs<'_>, mut value: Values) -> Values {
    let spot = input.spot - input.dividend_pv;
    let discount = (-input.rate * input.years).exp();
    let yield_rate = input.effective_yield();
    let growth = ((input.rate - yield_rate) * input.years).exp();
    let yield_discount = (-yield_rate * input.years).exp();
    let discounted_strike = input.strike * discount;
    let root_time = input.years.sqrt();
    let width = input.volatility * root_time;
    let d1 = ((spot / input.strike).ln()
        + (input.rate - yield_rate) * input.years
        + input.volatility * input.volatility * 0.5 * input.years)
        / width;
    let d2 = d1 - width;
    let mut first = normal_cdf(d1);
    let mut second = normal_cdf(d2);
    if !input.is_call {
        first = 1.0 - first;
        second = 1.0 - second;
    }
    let forward = growth * spot;
    let sign = if input.is_call { 1.0 } else { -1.0 };
    if input.volatility == 0.0 {
        value.price = discount
            * (if input.is_call { forward - input.strike } else { input.strike - forward });
        value.delta = 0.0;
        value.gamma = 0.0;
        value.vega = 0.0;
        value.theta = 0.0;
        if value.price > 0.0 {
            value.delta = clamp_delta(sign * yield_discount);
            value.theta = if input.is_call {
                (yield_discount * yield_rate * spot - input.rate * discounted_strike) / 365.0
            } else {
                (-yield_discount * yield_rate * spot + input.rate * discounted_strike) / 365.0
            };
        }
    } else {
        value.price = discount
            * if input.is_call {
                forward * first - input.strike * second
            } else {
                input.strike * second - forward * first
            };
        value.delta = clamp_delta(sign * first * yield_discount);
        let density = normal_pdf(d1);
        value.gamma = density * yield_discount / (spot * input.volatility * root_time);
        value.vega = spot * root_time * density * yield_discount * 0.01;
        let decay = spot * density * input.volatility / (2.0 * root_time);
        let interest = input.rate * discounted_strike * second;
        value.theta = if yield_rate == 0.0 {
            if input.is_call { (-decay - interest) / 365.0 } else { (-decay + interest) / 365.0 }
        } else {
            let carry = yield_rate * spot * first;
            if input.is_call {
                (yield_discount * (-decay + carry) - interest) / 365.0
            } else {
                (yield_discount * (-decay - carry) + interest) / 365.0
            }
        };
    }
    if value.price < 0.0 {
        value.price = 0.0;
    }
    value.forward = forward;
    value
}

fn normal_pdf(x: f64) -> f64 {
    (1.0 / (std::f64::consts::PI * 2.0).sqrt()) * (-x * x * 0.5).exp()
}

fn normal_cdf(x: f64) -> f64 {
    let density = normal_pdf(x);
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let tail = density
        * (((((1.330274429 * t + -1.821255978) * t + 1.781477937) * t + -0.356563782) * t
            + 0.31938153)
            * t);
    if x >= 0.0 { 1.0 - tail } else { tail }
}

pub(super) fn clamp_delta(value: f64) -> f64 {
    value.clamp(-0.999999999999999, 0.999999999999999)
}

pub(super) fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

fn min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}
