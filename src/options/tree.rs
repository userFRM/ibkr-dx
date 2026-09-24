//! Fixed-step lognormal and arithmetic option trees.

use super::inputs::{integer_years, tax_factor};
use super::model::{Inputs, Values, arithmetic_zero, clamp_delta, max};

const LOGNORMAL_STEPS: usize = 100;
const ARITHMETIC_STEPS: usize = 60;

fn dividends(input: &Inputs<'_>) -> ([f64; LOGNORMAL_STEPS + 1], f64) {
    let mut values = [0.0; LOGNORMAL_STEPS + 1];
    let dt = input.years / LOGNORMAL_STEPS as f64;
    let growth = ((input.rate - input.effective_yield()) * dt).exp();
    let mut previous = 0.0;
    let mut maximum = 0.0;
    for dividend in input.dividends {
        let fraction = dividend.millis_to_ex_date as f64 / 3.1536e10;
        if input.years < fraction || dividend.days_to_end_of_ex_date <= 0 {
            continue;
        }
        let mut amount = dividend.amount * tax_factor(input.tax_adjustment);
        if input.index_dividends {
            let cumulative = amount;
            amount -= previous;
            previous = cumulative;
        }
        let years = integer_years(fraction);
        let mut discount = (-input.rate * years).exp();
        let mut time = 0.0;
        for value in values.iter_mut().take(LOGNORMAL_STEPS) {
            if time > years {
                break;
            }
            *value += amount * discount;
            time += dt;
            discount *= growth;
            if maximum < *value {
                maximum = *value;
            }
        }
    }
    (values, maximum)
}

// The equality position matters when adjacent terminal nodes coincide.
fn strike_position(nodes: &[f64], strike: f64) -> usize {
    let mut low = 0_i32;
    let mut high = nodes.len() as i32 - 1;
    while low <= high {
        let middle = ((low + high) as u32 >> 1) as usize;
        let node = if nodes[middle].is_nan() { f64::NAN } else { nodes[middle] };
        let key = if strike.is_nan() { f64::NAN } else { strike };
        let order = node.total_cmp(&key);
        match order {
            std::cmp::Ordering::Less => low = middle as i32 + 1,
            std::cmp::Ordering::Greater => high = middle as i32 - 1,
            std::cmp::Ordering::Equal => return middle,
        }
    }
    low as usize
}

pub(super) fn lognormal(input: &Inputs<'_>, mut values: Values) -> Values {
    let (owed, maximum) = dividends(input);
    values = lognormal_run(input, input.volatility, &owed, maximum, values);
    let bumped =
        lognormal_run(input, input.volatility + 0.01, &owed, maximum, Values::empty(input));
    values.vega = bumped.price - values.price;
    values
}

fn lognormal_run(
    input: &Inputs<'_>,
    volatility: f64,
    owed: &[f64; LOGNORMAL_STEPS + 1],
    maximum: f64,
    mut result: Values,
) -> Values {
    let steps = LOGNORMAL_STEPS;
    let dt = input.years / steps as f64;
    let mut up = (volatility * dt.sqrt()).exp();
    let mut down = 1.0 / up;
    let growth = ((input.rate - input.effective_yield()) * dt).exp();
    let mut probability = (growth - down) / (up - down);
    if probability > 1.0 {
        up = growth;
        down = 1.0 / growth;
        probability = 1.0;
    }
    let other_probability = 1.0 - probability;
    let discount = (-input.rate * dt).exp();
    let base = input.spot - owed[0];
    let square_up = up * up;
    let mut nodes = [0.0; LOGNORMAL_STEPS + 1];
    let mut prices = [0.0; LOGNORMAL_STEPS + 1];
    nodes[steps / 2] = base;
    for i in steps / 2 + 1..=steps {
        nodes[i] = nodes[i - 1] * square_up;
    }
    for i in (0..steps / 2).rev() {
        nodes[i] = nodes[i + 1] / square_up;
    }
    let sign = if input.is_call { 1.0 } else { -1.0 };
    let strike = input.strike - owed[steps];
    for i in 0..=steps {
        let exercise = (nodes[i] - strike) * sign;
        prices[i] = if exercise > 0.0 { exercise } else { 0.0 };
    }
    let threshold = if input.is_call { input.strike - maximum } else { input.strike };
    let position = strike_position(&nodes, threshold);
    let mut middle_price = f64::NAN;
    for level in (1..=steps).rev() {
        let strike = input.strike - owed[level - 1];
        if input.is_call {
            let lower = (steps - level) as i32;
            let lower = lower.max(position as i32 - 1);
            for i in ((lower + 1) as usize..=steps).rev() {
                let mut value =
                    (probability * prices[i] + other_probability * prices[i - 1]) * discount;
                // A lognormal call is checked for exercise only with dividends due.
                if maximum > 0.0 {
                    nodes[i] *= down;
                    let exercise = nodes[i] - strike;
                    if value < exercise {
                        value = exercise;
                    }
                }
                prices[i] = value;
            }
        } else {
            for i in 0..level.min(position + 1) {
                let mut value =
                    (probability * prices[i + 1] + other_probability * prices[i]) * discount;
                nodes[i] *= up;
                let exercise = strike - nodes[i];
                if value < exercise {
                    value = exercise;
                }
                prices[i] = value;
            }
        }
        let (low, mid, high) =
            if input.is_call { (steps - 2, steps - 1, steps) } else { (0, 1, 2) };
        if level == 3 {
            middle_price = prices[mid];
            let square_down = down * down;
            let upper_slope = 1.0 / (base * (square_up - 1.0));
            let lower_slope = 1.0 / (base * (1.0 - square_down));
            let width = 0.5 * base * (square_up - square_down);
            result.gamma = ((prices[high] - prices[mid]) * upper_slope
                - (prices[mid] - prices[low]) * lower_slope)
                / width;
        }
        if level == 2 {
            let (low, high) = if input.is_call { (steps - 1, steps) } else { (0, 1) };
            result.delta = clamp_delta((prices[high] - prices[low]) / (base * (up - down)));
        }
    }
    result.price = prices[if input.is_call { steps } else { 0 }];
    result.theta = (middle_price - result.price) / (2.0 * dt * 365.0);
    result
}

fn arithmetic_run(input: &Inputs<'_>, step: f64, discount: f64) -> [f64; ARITHMETIC_STEPS + 3] {
    let mut prices = [0.0; ARITHMETIC_STEPS + 3];
    let forward = input.arithmetic_forward();
    let twice_step = 2.0 * step;
    if input.is_call {
        let mut spot = forward + (prices.len() - 1) as f64 * step;
        for price in prices.iter_mut().rev() {
            if spot <= input.strike {
                break;
            }
            *price = spot - input.strike;
            spot -= twice_step;
        }
    } else {
        let mut exercise = input.strike - (forward + (1 - prices.len() as i32) as f64 * step);
        for price in &mut prices {
            if exercise <= 0.0 {
                break;
            }
            *price = exercise;
            exercise -= twice_step;
        }
    }
    let weight = 0.5 * discount;
    for level in (0..ARITHMETIC_STEPS).rev() {
        let size = level + 3;
        for i in 0..size {
            prices[i] = (prices[i + 1] + prices[i]) * weight;
        }
        if input.american {
            if input.is_call {
                let mut exercise = forward + (size - 1) as f64 * step - input.strike;
                for price in prices[..size].iter_mut().rev() {
                    if exercise <= *price {
                        break;
                    }
                    *price = exercise;
                    exercise -= twice_step;
                }
            } else {
                let mut exercise = input.strike - (forward + (1 - size as i32) as f64 * step);
                for price in &mut prices[..size] {
                    if exercise <= *price {
                        break;
                    }
                    *price = exercise;
                    exercise -= twice_step;
                }
            }
        }
    }
    prices
}

pub(super) fn arithmetic(input: &Inputs<'_>) -> Values {
    if input.volatility * input.years == 0.0 {
        return arithmetic_zero(input);
    }
    let mut value = Values::empty(input);
    let dt = input.years / ARITHMETIC_STEPS as f64;
    let step = dt.sqrt() * input.volatility;
    let discount = (-input.rate * dt).exp();
    let prices = arithmetic_run(input, step, discount);
    value.price = prices[1];
    value.delta = (prices[2] - prices[0]) / (4.0 * step);
    value.gamma = (prices[2] - 2.0 * prices[1] + prices[0]) / (4.0 * step * step);
    let sign = if input.is_call { 1.0 } else { -1.0 };
    let forward = input.arithmetic_forward();
    let mut high = (prices[2] + prices[1]) * 0.5 * discount;
    let mut low = (prices[1] + prices[0]) * 0.5 * discount;
    if input.american {
        high = max(high, sign * (forward - input.strike + step));
        low = max(low, sign * (forward - input.strike - step));
    }
    let mut root = (high + low) * 0.5 * discount;
    if input.american {
        root = max(root, sign * (forward - input.strike));
    }
    value.theta = (prices[1] - root) / (2.0 * dt * 365.0);
    let bumped_step = dt.sqrt() * (input.volatility + input.volatility * 0.01);
    let bumped = arithmetic_run(input, bumped_step, discount);
    value.vega = bumped[1] - value.price;
    value.forward = forward;
    value
}
