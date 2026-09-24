use crate::control::attached_presets::{PriceSpec, PriceUnit};
use crate::control::contracts::MarketRule;

pub(crate) const UNSET_PRICE: f64 = f64::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PriceSide {
    #[default]
    Buy,
    Sell,
}

/// A contract's price rule, as the attached prices round and step by it.
impl MarketRule {
    fn increment(&self, price: f64, toward_zero: bool) -> Option<f64> {
        self.price_increments
            .iter()
            .enumerate()
            .rev()
            .find(|(index, step)| {
                *index == 0
                    || if toward_zero {
                        step.low_edge < price.abs()
                    } else {
                        step.low_edge <= price.abs()
                    }
            })
            .map(|(_, step)| step.increment)
    }

    pub(crate) fn round(&self, price: f64, up: bool) -> f64 {
        if price == UNSET_PRICE || price == 0.0 {
            return price + 0.0;
        }
        self.increment(price, false).map_or(price, |tick| directed(price, tick, up))
    }

    fn nearest(&self, price: f64) -> f64 {
        if price == UNSET_PRICE || price == 0.0 {
            return price + 0.0;
        }
        self.increment(price, false).map_or(price, |tick| nearest(price, tick))
    }

    pub(crate) fn is_valid(&self, price: f64) -> bool {
        valid_number(price)
            && (price > 0.0 || self.negative_prices)
            && (self.nearest(price) - price).abs() < 1e-9
    }

    fn amount(&self, amount: f64) -> f64 {
        if amount != UNSET_PRICE && self.price_magnifier > 1 {
            amount / f64::from(self.price_magnifier)
        } else {
            amount
        }
    }

    fn ticks(&self, base: f64, count: f64) -> f64 {
        if !valid_number(count) || count == 0.0 || self.price_increments.is_empty() {
            return base;
        }
        if let [only] = self.price_increments.as_slice() {
            return self.nearest(base + only.increment * count);
        }
        let mut price = base;
        let mut direction = count.signum();
        let mut crossed_zero = false;
        for _ in 0..(count.abs() as i32) {
            let toward_zero = (price > 0.0 && direction < 0.0) || (price < 0.0 && direction > 0.0);
            let Some(tick) = self.increment(price, toward_zero) else {
                continue;
            };
            let previous = price;
            price = nearest(price + direction * tick, tick) + 0.0;
            if previous != 0.0
                && (price == 0.0 || previous.signum() != price.signum())
                && !crossed_zero
            {
                direction = -direction;
                crossed_zero = true;
            }
        }
        if crossed_zero {
            price = -price;
        }
        self.nearest(price)
    }
}

fn directed(price: f64, increment: f64, up: bool) -> f64 {
    if !increment.is_finite() || increment <= 0.0 {
        return UNSET_PRICE;
    }
    let (numerator, denominator) = increment_fraction(increment);
    let units = price * denominator / numerator;
    let rounded = if up { (units - 1e-9).ceil() } else { (units + 1e-9).floor() };
    rounded * numerator / denominator
}

fn increment_fraction(increment: f64) -> (f64, f64) {
    let decimal = increment.to_string();
    if let Some((whole, fraction)) = decimal.split_once('.')
        && fraction.len() <= 18
        && let Ok(numerator) = format!("{whole}{fraction}").parse::<u64>()
    {
        return (numerator as f64, 10_f64.powi(fraction.len() as i32));
    }
    (increment, 1.0)
}

fn nearest(price: f64, increment: f64) -> f64 {
    let up = directed(price, increment, true);
    let down = directed(price, increment, false);
    if (up + down) / 2.0 <= price.abs() { up } else { down }
}

fn valid_number(value: f64) -> bool {
    value.is_finite() && value != UNSET_PRICE
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QuoteRecord {
    /// Stated prices, including zero and negative values.
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub bid_available: bool,
    pub ask_available: bool,
    /// Preferred prices additionally require size or a synthesized quote.
    pub bid_usable: bool,
    pub ask_usable: bool,
    pub last: Option<f64>,
    pub close: Option<f64>,
    pub vwap: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QuoteViews {
    pub current: QuoteRecord,
    pub regular: QuoteRecord,
    pub secondary: QuoteRecord,
    pub allow_secondary: bool,
    /// The midpoint helper has its own quote-availability predicate and view.
    pub last_midpoint: Option<f64>,
}

pub(crate) fn cached_quote_views(
    market: &crate::bridge::MarketDataState,
    instrument: crate::types::InstrumentId,
    negative_allowed: bool,
    is_index: bool,
    is_combo: bool,
    allow_negative_close: bool,
    allow_zero_ask_size: bool,
) -> QuoteViews {
    let held = market.pricing_quote_views(instrument);
    let convert = |index: usize| {
        let quote = held.records[index];
        let price = |value: Option<crate::types::Price>| {
            value.map(|value| value as f64 / crate::types::PRICE_SCALE as f64)
        };
        let bid = price(quote.bid);
        let ask = price(quote.ask);
        let bid_usable = bid.is_some() && quote.bid_size.is_some_and(|size| size != 0);
        let ask_usable = ask.is_some() && quote.ask_size.is_some_and(|size| size != 0);
        QuoteRecord {
            bid,
            ask,
            bid_usable,
            ask_usable,
            bid_available: bid_usable && quote.state_mask & 1 == 0,
            ask_available: ask_usable && quote.state_mask & 1 == 0,
            last: price(quote.last).filter(|price| {
                *price > 0.0
                    || quote.last_size.is_some_and(|size| size > 0)
                    || (is_index && negative_allowed)
            }),
            close: price(quote.close).filter(|price| {
                let valid_date = quote
                    .close_date
                    .is_some_and(|date| date > 0 && date != i32::MAX && date != 19700101);
                let invalid_attributes =
                    quote.close_attributes != -1 && quote.close_attributes & 1 != 0;
                let excluded = if is_combo && matches!(index, 2 | 3) {
                    !valid_date
                } else {
                    invalid_attributes || (is_combo && index == 0 && !valid_date)
                };
                (*price >= 0.0 || negative_allowed || allow_negative_close) && !excluded
            }),
            vwap: held.vwap,
        }
    };
    let current = convert(held.mode as usize);
    let regular = convert(usize::from(matches!(held.mode, 1 | 3)));
    let midpoint = regular.bid.zip(regular.ask).and_then(|(bid, ask)| {
        (regular.bid_usable
            && (regular.ask_usable || allow_zero_ask_size)
            && (bid > 0.0 || negative_allowed)
            && (ask > 0.0 || negative_allowed))
            .then_some((bid + ask) / 2.0)
    });
    QuoteViews {
        current,
        regular,
        secondary: convert(2),
        allow_secondary: matches!(held.mode, 2 | 3),
        last_midpoint: midpoint,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OrderPrice {
    pub side: PriceSide,
    pub limit: f64,
    pub stop: f64,
    pub touched_trigger: f64,
    pub uses_limit: bool,
    pub uses_stop: bool,
    pub is_market: bool,
    pub is_touched: bool,
}

impl Default for OrderPrice {
    fn default() -> Self {
        Self {
            side: PriceSide::Buy,
            limit: UNSET_PRICE,
            stop: UNSET_PRICE,
            touched_trigger: UNSET_PRICE,
            uses_limit: false,
            uses_stop: false,
            is_market: false,
            is_touched: false,
        }
    }
}

impl OrderPrice {
    pub(crate) fn selected(self) -> f64 {
        if self.uses_stop {
            return self.stop;
        }
        if (self.uses_limit || self.is_market) && self.limit != UNSET_PRICE {
            return self.limit;
        }
        if self.is_touched {
            return self.touched_trigger;
        }
        UNSET_PRICE
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum IndicativePrices<'a> {
    #[default]
    Original,
    Unavailable,
    Available(&'a PriceContext<'a>),
}

/// What an attached price is resolved from. Prices a trader's screen supplies
/// (a clicked, quoted or charted price) are never stated for an order an API
/// places, so the selectors reading them resolve to nothing here.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PriceContext<'a> {
    pub side: PriceSide,
    pub rule: Option<&'a MarketRule>,
    pub pricing_order: OrderPrice,
    pub quotes: QuoteViews,
    pub force_regular: bool,
    pub trades_at_settlement: bool,
    pub allow_portfolio_fallback: bool,
    pub portfolio_price: Option<f64>,
    /// Auction sides, supplied only for stock-loan pricing.
    pub loan_fee_sides: Option<(Option<f64>, Option<f64>)>,
    pub indicative: IndicativePrices<'a>,
}

impl PriceContext<'_> {
    fn round(&self, value: f64, up: bool) -> f64 {
        self.rule.map_or(value, |rule| rule.round(value, up))
    }

    fn aggressive(&self, value: f64) -> f64 {
        self.round(value, self.side == PriceSide::Buy)
    }

    fn preferred(&self, ask: bool, primary: QuoteRecord, secondary: Option<QuoteRecord>) -> f64 {
        let side = |quote: QuoteRecord, ask: bool| {
            let (price, usable) =
                if ask { (quote.ask, quote.ask_usable) } else { (quote.bid, quote.bid_usable) };
            price.filter(|value| usable && valid_number(*value))
        };
        side(primary, ask)
            .or_else(|| secondary.and_then(|quote| side(quote, ask)))
            .or_else(|| side(primary, !ask))
            .or_else(|| {
                (!self.trades_at_settlement).then_some(()).and_then(|()| {
                    primary
                        .last
                        .or(primary.close)
                        .or_else(|| secondary.and_then(|quote| quote.last.or(quote.close)))
                })
            })
            .unwrap_or(UNSET_PRICE)
    }
}

/// Resolve a price with an offset that already has its effective sign.
pub(crate) fn resolve_price(spec: &PriceSpec, context: &PriceContext<'_>) -> f64 {
    let selected;
    let context = if spec.price_type != 4 {
        match context.indicative {
            IndicativePrices::Original => context,
            IndicativePrices::Unavailable => return UNSET_PRICE,
            IndicativePrices::Available(proxy) => {
                selected = PriceContext {
                    side: context.side,
                    pricing_order: context.pricing_order,
                    force_regular: context.force_regular,
                    indicative: IndicativePrices::Original,
                    ..*proxy
                };
                &selected
            }
        }
    } else {
        context
    };
    let primary =
        if context.force_regular { context.quotes.regular } else { context.quotes.current };
    let secondary =
        (!context.force_regular && spec.offset == 0.0 && context.quotes.allow_secondary)
            .then_some(context.quotes.secondary);
    let preferred = |ask| context.preferred(ask, primary, secondary);
    let loan_side = |ask: bool, selector: i32| {
        let actual = if ask {
            primary.ask.filter(|_| primary.ask_available)
        } else {
            primary.bid.filter(|_| primary.bid_available)
        };
        let fee = context.loan_fee_sides.and_then(|(bid, ask_fee)| if ask { ask_fee } else { bid });
        match (actual, fee) {
            (Some(actual), Some(fee)) => match selector {
                0 => actual.max(fee),
                1 => actual.min(fee),
                _ => UNSET_PRICE,
            },
            (Some(price), None) | (None, Some(price)) => price,
            _ => UNSET_PRICE,
        }
    };
    let mut value = match spec.price_type {
        3 => UNSET_PRICE,
        4 => context.rule.map_or(spec.offset, |rule| rule.amount(spec.offset)),
        0 | 1 => {
            let ask = spec.price_type == 1;
            let price = if context.loan_fee_sides.is_some() {
                loan_side(ask, spec.price_type)
            } else {
                preferred(ask)
            };
            context.round(price, ask)
        }
        2 if !context.trades_at_settlement => primary.last.map_or_else(
            || {
                let midpoint = context.quotes.last_midpoint.unwrap_or(UNSET_PRICE);
                context.rule.map_or(midpoint, |rule| rule.nearest(midpoint))
            },
            |price| context.aggressive(price),
        ),
        5 => context.aggressive(preferred(context.side == PriceSide::Buy)),
        8 => {
            let mut price = preferred(context.side == PriceSide::Buy);
            if price == UNSET_PRICE && context.allow_portfolio_fallback {
                price = context.portfolio_price.unwrap_or(UNSET_PRICE);
            }
            context.aggressive(price)
        }
        9 if !context.trades_at_settlement => {
            context.aggressive(preferred(context.side == PriceSide::Buy))
        }
        11 | 37 => context.aggressive(context.pricing_order.selected()),
        12 => {
            let (bid, ask) = if context.loan_fee_sides.is_some() {
                (loan_side(false, 12), loan_side(true, 12))
            } else {
                (
                    primary.bid.filter(|_| primary.bid_available).unwrap_or(UNSET_PRICE),
                    primary.ask.filter(|_| primary.ask_available).unwrap_or(UNSET_PRICE),
                )
            };
            if bid == UNSET_PRICE || ask == UNSET_PRICE {
                UNSET_PRICE
            } else {
                let bid = context.round(bid, false);
                let ask = context.round(ask, true);
                context.round((bid + ask) / 2.0, context.side == PriceSide::Sell)
            }
        }
        13 => context.aggressive(primary.close.unwrap_or(UNSET_PRICE)),
        14 if !context.trades_at_settlement => {
            context.aggressive(primary.last.or(primary.close).unwrap_or(UNSET_PRICE))
        }
        15 => primary.vwap.unwrap_or(UNSET_PRICE),
        28 => context.aggressive(context.pricing_order.limit),
        29 => context.aggressive(context.pricing_order.stop),
        38 => 0.0,
        _ => UNSET_PRICE,
    };
    if spec.price_type != 4 && value != UNSET_PRICE && spec.offset != UNSET_PRICE {
        value = match spec.unit {
            PriceUnit::Amount => {
                value + context.rule.map_or(spec.offset, |rule| rule.amount(spec.offset))
            }
            PriceUnit::Percent => context.aggressive(value + value.abs() * spec.offset / 100.0),
            PriceUnit::Ticks => context.rule.map_or(value, |rule| rule.ticks(value, spec.offset)),
            PriceUnit::Unavailable => UNSET_PRICE,
        };
    }
    if context.rule.is_some_and(|rule| !rule.is_valid(value)) { UNSET_PRICE } else { value }
}

fn reversed(mut spec: PriceSpec) -> PriceSpec {
    spec.price_type = match spec.price_type {
        0 => 1,
        1 => 0,
        selector => selector,
    };
    if spec.offset != UNSET_PRICE {
        spec.offset = -spec.offset;
    }
    spec
}

fn relative(selector: i32) -> bool {
    !matches!(selector, 4 | 6)
}

fn primary_default(spec: &PriceSpec) -> bool {
    if spec.unit != PriceUnit::Amount || spec.use_parent_trade_price {
        return false;
    }
    matches!((spec.price_type, spec.offset), (1 | 3, 0.0) | (28 | 3, 1.0) | (29, 0.03) | (2, -1.0))
}

/// A child's price, as the side it trades on reads its selector: a parent
/// price selector prices from the parent, anything else from the child, with
/// bid and ask and the offset's sign turned round for the other side.
pub(crate) fn resolve_attached_price(
    spec: &PriceSpec,
    child: OrderPrice,
    parent: OrderPrice,
    context: &PriceContext<'_>,
) -> f64 {
    let using_parent = spec.price_type == 11;
    let pricing = if using_parent { parent } else { child };
    let mut spec = *spec;
    if !using_parent && relative(spec.price_type) {
        spec = reversed(spec);
    }
    let force_regular = !primary_default(&spec);
    if pricing.side == PriceSide::Sell && relative(spec.price_type) {
        spec = reversed(spec);
    }
    resolve_price(
        &spec,
        &PriceContext { side: pricing.side, pricing_order: pricing, force_regular, ..*context },
    )
}

pub(crate) fn resolve_adjusted_price(
    spec: &PriceSpec,
    child: OrderPrice,
    parent: OrderPrice,
    context: &PriceContext<'_>,
) -> f64 {
    let spec = if parent.side == PriceSide::Sell { reversed(*spec) } else { *spec };
    resolve_price(
        &spec,
        &PriceContext { side: child.side, pricing_order: parent, force_regular: false, ..*context },
    )
}

pub(crate) fn resolve_profit_price(
    spec: &PriceSpec,
    parent: OrderPrice,
    context: &PriceContext<'_>,
) -> f64 {
    let force_regular = !primary_default(spec);
    let spec = if parent.side == PriceSide::Sell && relative(spec.price_type) {
        reversed(*spec)
    } else {
        *spec
    };
    resolve_price(
        &spec,
        &PriceContext { side: parent.side, pricing_order: parent, force_regular, ..*context },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unresolved_indicative_contract_leaves_nonabsolute_prices_unset() {
        let context = PriceContext {
            pricing_order: OrderPrice { limit: 100.0, uses_limit: true, ..OrderPrice::default() },
            indicative: IndicativePrices::Unavailable,
            ..PriceContext::default()
        };
        for kind in [0, 1, 2, 8, 11, 12, 37] {
            assert_eq!(resolve_price(&spec(kind, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        }
        assert_eq!(resolve_price(&spec(4, 100.0, PriceUnit::Amount), &context), 100.0);
    }

    #[test]
    fn an_indicative_price_uses_proxy_quotes_and_rules_with_the_order_side() {
        let original_rule = rule(&[(0.0, 1.0)]);
        let proxy = PriceContext {
            rule: Some(rule(&[(0.0, 0.25)])),
            quotes: QuoteViews {
                current: QuoteRecord { last: Some(10.6), ..QuoteRecord::default() },
                ..QuoteViews::default()
            },
            ..PriceContext::default()
        };
        let mut context = PriceContext {
            side: PriceSide::Sell,
            rule: Some(original_rule),
            pricing_order: OrderPrice { limit: 100.6, uses_limit: true, ..OrderPrice::default() },
            indicative: IndicativePrices::Available(&proxy),
            ..PriceContext::default()
        };
        assert_eq!(resolve_price(&spec(2, 0.0, PriceUnit::Amount), &context), 10.5);
        assert_eq!(resolve_price(&spec(11, 0.0, PriceUnit::Amount), &context), 100.5);
        context.side = PriceSide::Buy;
        assert_eq!(resolve_price(&spec(2, 0.0, PriceUnit::Amount), &context), 10.75);
        assert_eq!(resolve_price(&spec(4, 10.5, PriceUnit::Amount), &context), UNSET_PRICE);
    }

    fn spec(price_type: i32, offset: f64, unit: PriceUnit) -> PriceSpec {
        PriceSpec { price_type, offset, unit, use_parent_trade_price: false }
    }

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
    }

    fn quote(bid: f64, ask: f64) -> QuoteRecord {
        QuoteRecord {
            bid: Some(bid),
            ask: Some(ask),
            bid_usable: true,
            ask_usable: true,
            bid_available: true,
            ask_available: true,
            ..QuoteRecord::default()
        }
    }

    fn rule(increments: &[(f64, f64)]) -> &'static MarketRule {
        rule_with(increments, false, 1)
    }

    fn rule_with(increments: &[(f64, f64)], negative: bool, magnifier: i32) -> &'static MarketRule {
        Box::leak(Box::new(MarketRule {
            rule_id: 0,
            negative_prices: negative,
            price_magnifier: magnifier,
            price_increments: increments
                .iter()
                .map(|(low_edge, increment)| crate::control::contracts::PriceIncrement {
                    low_edge: *low_edge,
                    increment: *increment,
                })
                .collect(),
            size_increments: Vec::new(),
        }))
    }

    #[test]
    fn preferred_quotes_follow_size_and_secondary_precedence() {
        let mut context = PriceContext {
            quotes: QuoteViews {
                current: QuoteRecord {
                    bid: Some(90.0),
                    ask: Some(110.0),
                    bid_usable: true,
                    ask_usable: false,
                    last: Some(80.0),
                    close: Some(70.0),
                    ..QuoteRecord::default()
                },
                secondary: QuoteRecord {
                    ask: Some(100.0),
                    ask_usable: true,
                    last: Some(60.0),
                    close: Some(50.0),
                    ..QuoteRecord::default()
                },
                allow_secondary: true,
                ..QuoteViews::default()
            },
            ..PriceContext::default()
        };
        let ask = spec(1, 0.0, PriceUnit::Amount);
        assert_eq!(resolve_price(&ask, &context), 100.0);
        context.quotes.secondary.ask_usable = false;
        assert_eq!(resolve_price(&ask, &context), 90.0);
        context.quotes.current.bid_usable = false;
        assert_eq!(resolve_price(&ask, &context), 80.0);
        context.quotes.current.last = None;
        assert_eq!(resolve_price(&ask, &context), 70.0);
        context.quotes.current.close = None;
        assert_eq!(resolve_price(&ask, &context), 60.0);
        context.quotes.secondary.last = None;
        assert_eq!(resolve_price(&ask, &context), 50.0);
        context.trades_at_settlement = true;
        assert_eq!(resolve_price(&ask, &context), UNSET_PRICE);
    }

    #[test]
    fn preferred_bid_is_symmetric_and_does_not_try_secondary_opposite_side() {
        let mut context = PriceContext {
            quotes: QuoteViews {
                current: QuoteRecord {
                    ask: Some(105.0),
                    ask_usable: true,
                    ..QuoteRecord::default()
                },
                secondary: quote(95.0, 106.0),
                allow_secondary: true,
                ..QuoteViews::default()
            },
            ..PriceContext::default()
        };
        let bid = spec(0, 0.0, PriceUnit::Amount);
        assert_eq!(resolve_price(&bid, &context), 95.0);
        context.quotes.secondary.bid_usable = false;
        assert_eq!(resolve_price(&bid, &context), 105.0);
        context.quotes.current.ask_usable = false;
        assert_eq!(resolve_price(&bid, &context), UNSET_PRICE);
    }

    #[test]
    fn secondary_quote_requires_zero_offset_and_permitted_current_view() {
        let mut context = PriceContext {
            quotes: QuoteViews {
                current: QuoteRecord::default(),
                regular: quote(79.0, 81.0),
                secondary: quote(99.0, 101.0),
                allow_secondary: true,
                ..QuoteViews::default()
            },
            ..PriceContext::default()
        };
        assert_eq!(resolve_price(&spec(1, 0.0, PriceUnit::Amount), &context), 101.0);
        assert_eq!(resolve_price(&spec(1, 1.0, PriceUnit::Amount), &context), UNSET_PRICE);
        context.force_regular = true;
        assert_eq!(resolve_price(&spec(1, 0.0, PriceUnit::Amount), &context), 81.0);
        context.force_regular = false;
        context.quotes.allow_secondary = false;
        assert_eq!(resolve_price(&spec(1, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
    }

    #[test]
    fn last_falls_back_only_to_its_midpoint_and_uses_nearest_rounding() {
        let mut context = PriceContext {
            rule: Some(rule(&[(0.0, 0.1)])),
            quotes: QuoteViews {
                current: QuoteRecord { last: Some(10.12), close: Some(11.0), ..quote(10.0, 10.3) },
                last_midpoint: Some(10.14),
                ..QuoteViews::default()
            },
            ..PriceContext::default()
        };
        let last = spec(2, 0.0, PriceUnit::Amount);
        close(resolve_price(&last, &context), 10.2);
        context.quotes.current.last = None;
        close(resolve_price(&last, &context), 10.1);
        context.quotes.last_midpoint = None;
        assert_eq!(resolve_price(&last, &context), UNSET_PRICE);
        close(resolve_price(&spec(14, 0.0, PriceUnit::Amount), &context), 11.0);
        context.trades_at_settlement = true;
        assert_eq!(resolve_price(&spec(14, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        close(resolve_price(&spec(13, 0.0, PriceUnit::Amount), &context), 11.0);
    }

    #[test]
    fn midpoint_rounds_sides_outward_then_uses_less_aggressive_side() {
        let mut context = PriceContext {
            rule: Some(rule(&[(0.0, 0.1)])),
            quotes: QuoteViews { current: quote(10.02, 10.21), ..QuoteViews::default() },
            ..PriceContext::default()
        };
        let midpoint = spec(12, 0.0, PriceUnit::Amount);
        close(resolve_price(&midpoint, &context), 10.1);
        context.side = PriceSide::Sell;
        close(resolve_price(&midpoint, &context), 10.2);
        context.quotes.current.ask = None;
        context.quotes.current.last = Some(10.0);
        assert_eq!(resolve_price(&midpoint, &context), UNSET_PRICE);
    }

    #[test]
    fn amount_uses_magnifier_without_extra_rounding() {
        let context = PriceContext {
            rule: Some(rule_with(&[(0.0, 0.05)], false, 100)),
            pricing_order: OrderPrice { limit: 10.0, ..OrderPrice::default() },
            ..PriceContext::default()
        };
        close(resolve_price(&spec(28, 5.0, PriceUnit::Amount), &context), 10.05);
        assert_eq!(resolve_price(&spec(28, 3.0, PriceUnit::Amount), &context), UNSET_PRICE);
        close(resolve_price(&spec(4, 1005.0, PriceUnit::Ticks), &context), 10.05);
    }

    #[test]
    fn negative_percent_uses_absolute_base_and_directional_rounding() {
        let mut context = PriceContext {
            rule: Some(rule_with(&[(0.0, 0.1)], true, 1)),
            pricing_order: OrderPrice { limit: -10.0, ..OrderPrice::default() },
            ..PriceContext::default()
        };
        close(resolve_price(&spec(28, 1.5, PriceUnit::Percent), &context), -9.8);
        context.side = PriceSide::Sell;
        close(resolve_price(&spec(28, 1.5, PriceUnit::Percent), &context), -9.9);
        context.rule = Some(rule(&[(0.0, 0.1)]));
        assert_eq!(resolve_price(&spec(28, 1.5, PriceUnit::Percent), &context), UNSET_PRICE);
    }

    #[test]
    fn ticks_walk_tiers_in_both_directions_and_truncate_multitier_count() {
        let market_rule = rule(&[(0.0, 0.01), (1.0, 0.05), (2.0, 0.1)]);
        close(market_rule.ticks(0.99, 3.9), 1.1);
        close(market_rule.ticks(1.05, -3.9), 0.98);
        close(market_rule.ticks(1.95, 2.0), 2.1);
        close(market_rule.ticks(2.0, -1.0), 1.95);
        close(market_rule.ticks(1.05, 0.9), 1.05);
        close(rule(&[(0.0, 0.1)]).ticks(1.0, 1.5), 1.2);
        assert_eq!(rule(&[]).ticks(3.0, 4.0), 3.0);
    }

    #[test]
    fn ticks_cross_zero_and_normalize_negative_zero() {
        let market_rule = rule_with(&[(0.0, 0.5), (2.0, 1.0)], true, 1);
        close(market_rule.ticks(0.5, -2.0), -0.5);
        close(market_rule.ticks(-0.5, 2.0), 0.5);
        assert_eq!(market_rule.ticks(0.5, -1.0).to_bits(), 0.0_f64.to_bits());
        close(market_rule.nearest(-0.76), -0.5);
        close(market_rule.round(-0.76, false), -1.0);
    }

    #[test]
    fn rounding_tolerance_and_absolute_tier_selection_are_preserved() {
        let market_rule = rule(&[(0.0, 0.05), (10.0, 0.25)]);
        close(market_rule.round(10.12, true), 10.25);
        close(market_rule.round(-10.12, false), -10.25);
        close(market_rule.round(1.00000000001, true), 1.0);
        close(market_rule.round(0.99999999999, false), 1.0);
        assert!(market_rule.is_valid(1.00000000001));
        assert!(!market_rule.is_valid(1.01));
        assert!(!market_rule.is_valid(0.0));
        assert_eq!(market_rule.round(UNSET_PRICE, true), UNSET_PRICE);
    }

    #[test]
    fn parent_price_selection_obeys_type_and_priority_without_quote_fallback() {
        let mut order = OrderPrice {
            limit: 101.0,
            stop: 99.0,
            uses_limit: true,
            uses_stop: true,
            ..OrderPrice::default()
        };
        assert_eq!(order.selected(), 99.0);
        order.stop = UNSET_PRICE;
        assert_eq!(order.selected(), UNSET_PRICE);
        order.uses_stop = false;
        order.uses_limit = false;
        order.is_market = true;
        assert_eq!(order.selected(), 101.0);
        order.is_market = false;
        order.is_touched = true;
        order.touched_trigger = 98.0;
        assert_eq!(order.selected(), 98.0);
        order.is_touched = false;
        let context = PriceContext {
            pricing_order: order,
            quotes: QuoteViews { current: quote(100.0, 101.0), ..QuoteViews::default() },
            ..PriceContext::default()
        };
        assert_eq!(resolve_price(&spec(11, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
    }

    #[test]
    fn attached_sign_and_pricing_side_depend_on_parent_selector() {
        let parent = OrderPrice {
            side: PriceSide::Buy,
            limit: 100.0,
            uses_limit: true,
            ..OrderPrice::default()
        };
        let child = OrderPrice {
            side: PriceSide::Sell,
            limit: 120.0,
            uses_limit: true,
            ..OrderPrice::default()
        };
        let context = PriceContext { rule: Some(rule(&[(0.0, 0.01)])), ..PriceContext::default() };
        close(
            resolve_attached_price(&spec(11, 2.0, PriceUnit::Amount), child, parent, &context),
            102.0,
        );
        close(
            resolve_attached_price(&spec(37, 2.0, PriceUnit::Amount), child, parent, &context),
            122.0,
        );
        let parent = OrderPrice { side: PriceSide::Sell, ..parent };
        let child = OrderPrice { side: PriceSide::Buy, ..child };
        close(
            resolve_attached_price(&spec(11, 2.0, PriceUnit::Amount), child, parent, &context),
            98.0,
        );
        close(
            resolve_attached_price(&spec(37, 2.0, PriceUnit::Amount), child, parent, &context),
            118.0,
        );
        close(
            resolve_attached_price(&spec(4, 12.0, PriceUnit::Amount), child, parent, &context),
            12.0,
        );
    }

    #[test]
    fn attached_regular_view_matches_transformed_primary_defaults() {
        let context = PriceContext {
            quotes: QuoteViews {
                current: quote(99.0, 101.0),
                regular: quote(89.0, 91.0),
                ..QuoteViews::default()
            },
            ..PriceContext::default()
        };
        let child = OrderPrice { side: PriceSide::Sell, ..OrderPrice::default() };
        // Reversing BID zero produces the primary ASK zero default.
        assert_eq!(
            resolve_attached_price(
                &spec(0, 0.0, PriceUnit::Amount),
                child,
                OrderPrice::default(),
                &context
            ),
            99.0
        );
        assert_eq!(
            resolve_attached_price(
                &spec(0, 1.0, PriceUnit::Amount),
                child,
                OrderPrice::default(),
                &context
            ),
            90.0
        );
        let mut parent_trade = spec(0, 0.0, PriceUnit::Amount);
        parent_trade.use_parent_trade_price = true;
        assert_eq!(
            resolve_attached_price(&parent_trade, child, OrderPrice::default(), &context),
            89.0
        );
    }

    #[test]
    fn adjusted_prices_use_parent_sign_and_child_rounding_side() {
        let context = PriceContext {
            rule: Some(rule(&[(0.0, 0.1)])),
            quotes: QuoteViews { current: quote(99.02, 100.02), ..QuoteViews::default() },
            ..PriceContext::default()
        };
        let child = OrderPrice { side: PriceSide::Buy, ..OrderPrice::default() };
        let parent = OrderPrice {
            side: PriceSide::Sell,
            limit: 101.02,
            uses_limit: true,
            ..OrderPrice::default()
        };
        close(
            resolve_adjusted_price(&spec(11, 1.0, PriceUnit::Amount), child, parent, &context),
            100.1,
        );
        close(
            resolve_adjusted_price(&spec(0, 1.0, PriceUnit::Amount), child, parent, &context),
            99.1,
        );
    }

    #[test]
    fn remaining_selectors_and_unset_offsets_keep_their_distinct_rules() {
        let mut context = PriceContext {
            rule: Some(rule(&[(0.0, 0.1)])),
            allow_portfolio_fallback: true,
            portfolio_price: Some(15.03),
            ..PriceContext::default()
        };
        // A clicked, quoted or charted price is never stated for an API order.
        assert_eq!(resolve_price(&spec(6, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        assert_eq!(
            resolve_price(&spec(7, UNSET_PRICE, PriceUnit::Unavailable), &context),
            UNSET_PRICE
        );
        assert_eq!(resolve_price(&spec(9, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        close(resolve_price(&spec(8, 0.0, PriceUnit::Amount), &context), 15.1);
        context.quotes.current = quote(14.01, 14.03);
        close(resolve_price(&spec(9, 0.0, PriceUnit::Amount), &context), 14.1);
        context.quotes.current = QuoteRecord::default();
        assert_eq!(resolve_price(&spec(25, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        assert_eq!(resolve_price(&spec(16, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        assert_eq!(resolve_price(&spec(3, 4.0, PriceUnit::Amount), &context), UNSET_PRICE);
        assert_eq!(resolve_price(&spec(38, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        context.rule = Some(rule_with(&[(0.0, 0.1)], true, 1));
        assert_eq!(resolve_price(&spec(38, 0.0, PriceUnit::Amount), &context), 0.0);
        context.quotes.current.vwap = Some(16.03);
        assert_eq!(resolve_price(&spec(15, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
    }

    #[test]
    fn profit_uses_parent_order_for_order_price_and_side_reversal() {
        let parent = OrderPrice {
            side: PriceSide::Buy,
            limit: 100.0,
            uses_limit: true,
            ..OrderPrice::default()
        };
        let context = PriceContext::default();
        assert_eq!(
            resolve_profit_price(&spec(37, 2.0, PriceUnit::Amount), parent, &context),
            102.0
        );
        let parent = OrderPrice { side: PriceSide::Sell, ..parent };
        assert_eq!(resolve_profit_price(&spec(37, 2.0, PriceUnit::Amount), parent, &context), 98.0);
    }

    #[test]
    fn cached_quote_presence_keeps_zero_negative_and_unstated_prices_distinct() {
        let shared = crate::bridge::SharedState::new();
        let market = &shared.market;
        let mut quote = crate::types::Quote {
            bid: 0,
            ask: -crate::types::PRICE_SCALE,
            bid_size: crate::types::QTY_SCALE,
            ask_size: crate::types::QTY_SCALE,
            ..crate::types::Quote::default()
        };
        market.push_pricing_quote(1, 0, &quote, crate::bridge::PRICING_BID, 0);
        let prices = cached_quote_views(market, 1, true, false, false, false, false);
        assert_eq!(prices.current.bid, Some(0.0));
        assert_eq!(prices.current.ask, None);
        assert!(!prices.current.bid_usable);
        market.push_pricing_quote(
            1,
            0,
            &quote,
            crate::bridge::PRICING_ASK
                | crate::bridge::PRICING_BID_SIZE
                | crate::bridge::PRICING_ASK_SIZE,
            0,
        );
        let prices = cached_quote_views(market, 1, true, false, false, false, false);
        assert_eq!(prices.current.ask, Some(-1.0));
        assert!(prices.current.bid_available && prices.current.ask_available);
        assert_eq!(prices.last_midpoint, Some(-0.5));
        assert_eq!(
            cached_quote_views(market, 1, false, false, false, false, false).last_midpoint,
            None
        );
        quote.bid_size = 0;
        market.push_pricing_quote(1, 0, &quote, crate::bridge::PRICING_BID_SIZE, 0);
        assert!(
            !cached_quote_views(market, 1, true, false, false, false, false).current.bid_available
        );
        market.forget_series_ticks(1);
        assert_eq!(
            cached_quote_views(market, 1, true, false, false, false, false).current.bid,
            None
        );
    }

    #[test]
    fn cached_views_isolate_frozen_prices_and_ignore_regulatory_snapshots() {
        let shared = crate::bridge::SharedState::new();
        let market = &shared.market;
        let regular = crate::types::Quote {
            bid: 10 * crate::types::PRICE_SCALE,
            ask: 12 * crate::types::PRICE_SCALE,
            bid_size: crate::types::QTY_SCALE,
            ask_size: crate::types::QTY_SCALE,
            ..crate::types::Quote::default()
        };
        market.push_pricing_quote(2, 0, &regular, 0xff, 0);
        let frozen = crate::types::Quote { bid: 20 * crate::types::PRICE_SCALE, ..regular };
        market.push_pricing_quote(2, 2, &frozen, crate::bridge::PRICING_BID, 0);
        let views = cached_quote_views(market, 2, false, false, false, false, false);
        assert_eq!(views.current.bid, Some(20.0));
        assert_eq!(views.current.ask, None);
        assert_eq!(views.regular.bid, Some(10.0));
        assert_eq!(views.last_midpoint, Some(11.0));
        assert!(views.allow_secondary);
        market.push_snapshot_answer(
            2,
            vec![crate::types::SeriesTick {
                instrument: 2,
                tick_type: 1,
                value: crate::types::SeriesValue::Price(999.0),
            }],
        );
        assert_eq!(
            cached_quote_views(market, 2, false, false, false, false, false).current.bid,
            Some(20.0)
        );
        market.zero_all_quotes();
        assert_eq!(
            cached_quote_views(market, 2, false, false, false, false, false).current.bid,
            None
        );
    }

    #[test]
    fn preopen_can_supply_preferred_price_but_not_actual_midpoint() {
        let shared = crate::bridge::SharedState::new();
        let market = &shared.market;
        let quote = crate::types::Quote {
            bid: 10 * crate::types::PRICE_SCALE,
            ask: 12 * crate::types::PRICE_SCALE,
            bid_size: crate::types::QTY_SCALE,
            ask_size: crate::types::QTY_SCALE,
            ..crate::types::Quote::default()
        };
        market.push_pricing_quote(3, 0, &quote, 0xff, 1);
        let context = PriceContext {
            quotes: cached_quote_views(market, 3, false, false, false, false, false),
            ..PriceContext::default()
        };
        assert_eq!(resolve_price(&spec(0, 0.0, PriceUnit::Amount), &context), 10.0);
        assert_eq!(resolve_price(&spec(12, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        assert_eq!(resolve_price(&spec(2, 0.0, PriceUnit::Amount), &context), 11.0);
    }

    #[test]
    fn quote_sides_accept_negative_size_but_last_requires_positive_size() {
        let shared = crate::bridge::SharedState::new();
        let market = &shared.market;
        let mut quote = crate::types::Quote {
            bid: 2 * crate::types::PRICE_SCALE,
            ask: 4 * crate::types::PRICE_SCALE,
            last: 0,
            bid_size: -crate::types::QTY_SCALE,
            ask_size: -crate::types::QTY_SCALE,
            last_size: -crate::types::QTY_SCALE,
            ..crate::types::Quote::default()
        };
        market.push_pricing_quote(1, 0, &quote, 127, 0);
        let views = cached_quote_views(market, 1, false, false, false, false, false);
        assert!(views.current.bid_available && views.current.ask_available);
        assert_eq!(views.last_midpoint, Some(3.0));
        assert_eq!(views.current.last, None);
        quote.last_size = crate::types::QTY_SCALE;
        market.push_pricing_quote(1, 0, &quote, crate::bridge::PRICING_LAST_SIZE, 0);
        assert_eq!(
            cached_quote_views(market, 1, false, false, false, false, false).current.last,
            Some(0.0)
        );
    }

    #[test]
    fn close_availability_keeps_contract_and_feed_exceptions() {
        let shared = crate::bridge::SharedState::new();
        let market = &shared.market;
        let quote = crate::types::Quote { close: -crate::types::PRICE_SCALE, ..Default::default() };
        market.push_pricing_quote(1, 0, &quote, crate::bridge::PRICING_CLOSE, 0);
        assert_eq!(
            cached_quote_views(market, 1, false, false, false, false, false).current.close,
            None
        );
        assert_eq!(
            cached_quote_views(market, 1, false, false, false, true, false).current.close,
            Some(-1.0)
        );
        market.note_pricing_close_metadata(1, 0, Some(1), Some(20260924));
        assert_eq!(
            cached_quote_views(market, 1, true, false, true, false, false).current.close,
            None
        );
        market.push_pricing_quote(1, 2, &quote, crate::bridge::PRICING_CLOSE, 0);
        market.note_pricing_close_metadata(1, 2, Some(1), Some(20260924));
        assert_eq!(
            cached_quote_views(market, 1, true, false, true, false, false).current.close,
            Some(-1.0)
        );
        market.note_pricing_close_metadata(1, 2, Some(0), Some(19700101));
        assert_eq!(
            cached_quote_views(market, 1, true, false, true, false, false).current.close,
            None
        );
    }

    #[test]
    fn stock_loan_auction_sides_preserve_selector_specific_choice() {
        let mut context = PriceContext {
            quotes: QuoteViews { current: quote(10.0, 12.0), ..Default::default() },
            loan_fee_sides: Some((Some(11.0), Some(11.5))),
            ..Default::default()
        };
        assert_eq!(resolve_price(&spec(0, 0.0, PriceUnit::Amount), &context), 11.0);
        assert_eq!(resolve_price(&spec(1, 0.0, PriceUnit::Amount), &context), 11.5);
        assert_eq!(resolve_price(&spec(12, 0.0, PriceUnit::Amount), &context), UNSET_PRICE);
        context.loan_fee_sides = Some((None, None));
        assert_eq!(resolve_price(&spec(12, 0.0, PriceUnit::Amount), &context), 11.0);
        context.quotes.current.bid_available = false;
        context.loan_fee_sides = Some((Some(11.0), None));
        assert_eq!(resolve_price(&spec(0, 0.0, PriceUnit::Amount), &context), 11.0);
    }

    #[test]
    fn portfolio_mark_presence_survives_lean_updates_and_resets() {
        let shared = crate::bridge::SharedState::new();
        let position =
            || crate::types::PositionInfo { con_id: 1, position: 1.0, ..Default::default() };
        shared.portfolio.set_position_info(position());
        assert!(!shared.portfolio.position_info(1).unwrap().market_price_stated);
        shared.portfolio.set_position_marks(1, Some(0), None, None, None);
        shared.portfolio.set_position_info(position());
        assert!(shared.portfolio.position_info(1).unwrap().market_price_stated);
        shared
            .portfolio
            .set_position_info(crate::types::PositionInfo { position: 0.0, ..position() });
        assert!(!shared.portfolio.position_info(1).unwrap().market_price_stated);
        shared.portfolio.set_position_marks(1, Some(0), None, None, None);
        shared.portfolio.account_download_is_pending();
        assert!(!shared.portfolio.position_info(1).unwrap().market_price_stated);
    }

    #[test]
    fn stock_lender_midpoint_allows_zero_ask_size_without_changing_preferred_side() {
        let shared = crate::bridge::SharedState::new();
        let market = &shared.market;
        let quote = crate::types::Quote {
            bid: 10 * crate::types::PRICE_SCALE,
            ask: 12 * crate::types::PRICE_SCALE,
            bid_size: crate::types::QTY_SCALE,
            ask_size: 0,
            ..Default::default()
        };
        market.push_pricing_quote(1, 0, &quote, 0xff, 0);
        assert_eq!(
            cached_quote_views(market, 1, false, false, false, false, false).last_midpoint,
            None
        );
        let views = cached_quote_views(market, 1, false, false, false, false, true);
        assert_eq!(views.last_midpoint, Some(11.0));
        assert!(!views.current.ask_available);
        let context = PriceContext { quotes: views, ..Default::default() };
        assert_eq!(resolve_price(&spec(1, 0.0, PriceUnit::Amount), &context), 10.0);
    }
}
