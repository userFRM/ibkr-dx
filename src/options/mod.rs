//! Option prices and greeks computed from stated model inputs.
//!
//! These calculations do not subscribe to data or publish ticks. The caller
//! supplies the clock, rates, dividends and volatility used for each calculation.

mod inputs;
pub mod model;
mod tree;

pub use inputs::{
    Dividend, RatePoint, continuous_rate, dividend_present_value, rate_at, rate_term,
    time_to_expiry,
};

#[cfg(test)]
mod tests;
