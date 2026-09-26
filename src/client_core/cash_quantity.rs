//! Orders stated by the cash they spend: what a gateway refuses of one before
//! sending it, and the size it works out for its own record of it.

use super::{ApiOrder, Refusal, SharedState};
use crate::bridge::MoneyOrderTerms;
use crate::control::contracts::{ContractDefinition, SecurityType};
use crate::types::Side;

/// Refused where the contract and order cannot carry a cash amount.
const CASH_QUANTITY_NOT_FOR_THIS_ORDER: i32 = 10244;
/// A fund bought otherwise than by an amount alone.
const FUND_BUY_BY_AMOUNT: i32 = 10203;
/// A fund sold otherwise than by a quantity alone.
const FUND_SELL_BY_QUANTITY: i32 = 10204;
/// An amount with more places than a cash amount is stated to.
const CASH_QUANTITY_PLACES: i32 = 10206;
/// An amount finer than its currency is dealt in.
const CASH_QUANTITY_VARIATION: i32 = 10317;
/// A fraction of a unit where the order takes none.
const NO_FRACTIONAL_QUANTITY: i32 = 10318;
/// A modification of an order stated by an amount.
const CASH_QUANTITY_NOT_MODIFIED: i32 = 10241;
/// A stop to buy where the instrument takes none.
const CRYPTO_STOP_BUY: i32 = 10292;
/// A fund sold by a size with more places than a fund's size is stated to.
const FUND_SIZE_PLACES: i32 = 10207;
/// A crypto stated by an amount and a size both.
const CRYPTO_CASH_AND_SIZE: i32 = 10293;

/// The order types a logon's lists name for orders at market, at a limit, on
/// a stop and on a stop with a limit.
const BASIC_TYPES: [&str; 4] = ["MKT", "LMT", "STP", "STPLMT"];

/// What a gateway refuses, before sending it, of an order stated by the cash
/// it spends, and of an order for a fund: the refusals it states, in the order
/// it states them, and nothing sent. Empty sends the order.
///
/// In the order a gateway asks. A fund's amount is held apart from any other
/// order's from the start. A stop to buy a crypto is refused for what it is. A
/// currency pair's size must be whole, and its amount, where it has parts of a
/// unit, a multiple of the least amount its currency moves in where that is
/// itself a part of a unit and the logon does not waive it, and whole
/// otherwise. Then the contract has to take an amount for the order (see
/// [`takes_an_amount`]). Then a fund is bought by an amount and no quantity,
/// and sold by a quantity and no amount of no more than three places; its
/// amount, like any other order's, is a multiple of its currency's least
/// amount, or, where that is not held, has no more than two places. A refusal
/// among these last is stated and the checks go on unstated: the order is
/// dropped all the same. Last, a crypto stated by an amount states no size.
pub(crate) fn refusals(
    shared: &SharedState,
    order: &ApiOrder,
    definition: &ContractDefinition,
) -> Vec<Refusal> {
    let terms = shared.reference.money_orders();
    let amount = stated_amount(order);
    let fund = definition.sec_type == SecurityType::Fund;
    let (cash, fund_cash) = if fund { (None, amount) } else { (amount, None) };
    let buy = order.side() == Ok(Side::Buy);
    let kind = order.order_type_named().unwrap_or_default();
    let quantity = order.total_quantity;
    let increment = currency_increment(&terms, &definition.currency);
    let by_increment =
        !shared.reference.enables("NOCASHQTYPRECISION") && !increment.fraction().is_empty();
    let off_increment = |cash: f64| {
        (!Decimal::of(cash, 6).multiple_of(&increment)).then(|| {
            Refusal::stated(
                CASH_QUANTITY_VARIATION,
                format!(
                    "The Cash Quantity size of {} does not conform to minimum variation of {} for \
                     this contract",
                    cash_text(cash),
                    increment.text(),
                ),
            )
        })
    };
    if definition.sec_type == SecurityType::Crypto && kind == "STP" && buy {
        return vec![Refusal::stated(
            CRYPTO_STOP_BUY,
            "Stop Buy order is not allowed for this instrument",
        )];
    }
    let fractional = || {
        Refusal::stated(
            NO_FRACTIONAL_QUANTITY,
            "This order doesn't support fractional quantity trading",
        )
    };
    if definition.sec_type == SecurityType::Forex {
        if quantity.fract() != 0.0 {
            return vec![fractional()];
        }
        if let Some(cash) = cash
            && !Decimal::of(cash, 16).fraction().is_empty()
        {
            let refused = if by_increment { off_increment(cash) } else { Some(fractional()) };
            if let Some(refused) = refused {
                return vec![refused];
            }
        }
    }
    if amount.is_some() && !takes_an_amount(shared, &terms, order, definition) {
        return vec![Refusal::stated(
            CASH_QUANTITY_NOT_FOR_THIS_ORDER,
            "Cash Quantity cannot be used for this order",
        )];
    }
    let places = |cash: f64| {
        (Decimal::of(cash, 18).fraction().len() > 2).then(|| {
            Refusal::stated(
                CASH_QUANTITY_PLACES,
                "Non-zero cash quantity cannot contain more than 2 decimals.",
            )
        })
    };
    let mut dropped = None;
    if fund {
        if buy && (fund_cash.is_none() || quantity > 0.0) {
            return vec![Refusal::stated(
                FUND_BUY_BY_AMOUNT,
                "A buy order for FUND must contain non-zero cashQty, and may not contain non-zero \
                 totalQuantity.",
            )];
        }
        if !buy && (fund_cash.is_some() || quantity <= 0.0) {
            return vec![Refusal::stated(
                FUND_SELL_BY_QUANTITY,
                "A sell order for FUND must contain non-zero totalQuantity, and may not contain \
                 non-zero cashQty.",
            )];
        }
        // ponytail: a fund whose rule deals in parts of a unit, where the
        // logon does not keep funds to whole units, is held to the places
        // its rule's size format states, which this client does not read;
        // those sales are not checked here.
        let whole_units = shared.reference.enables("NOMFFRACMR")
            || finest_size(shared, definition).is_none_or(|finest| finest >= 1.0);
        if !buy && whole_units && Decimal::of(quantity, 18).fraction().len() > 3 {
            dropped = Some(Refusal::stated(
                FUND_SIZE_PLACES,
                "For funds non-zero Order Size cannot contain more than 3 decimals.",
            ));
        }
        if let Some(cash) = fund_cash {
            // The least amount the fund itself is bought in, where the logon
            // holds it to one: the places are compared, not the multiple.
            let tick = definition.unnamed_fields.iter().find(|(tag, _)| *tag == FUND_TICK);
            if let Some((_, tick)) = tick
                && buy
                && shared.reference.enables("MFCASHQTYINCR")
                && let Some(tick) = Decimal::parse(tick)
                && Decimal::of(cash, 4).fraction().len() > tick.fraction().len()
            {
                return vec![Refusal::stated(
                    CASH_QUANTITY_VARIATION,
                    format!(
                        "The Cash Quantity size of {} does not conform to minimum variation of {} \
                         for this contract",
                        cash_text(cash),
                        tick.text(),
                    ),
                )];
            }
            dropped =
                dropped.or_else(|| if by_increment { off_increment(cash) } else { places(cash) });
        }
    }
    if let Some(cash) = cash {
        dropped = dropped.or_else(|| if by_increment { off_increment(cash) } else { places(cash) });
    }
    let mut refused: Vec<_> = dropped.into_iter().collect();
    // Asked once the order is being made, and stated whatever came before.
    if definition.sec_type == SecurityType::Crypto && cash.is_some() && quantity != 0.0 {
        refused.push(Refusal::stated(
            CRYPTO_CASH_AND_SIZE,
            "Cryptocurrency Cash Quantity order cannot specify size",
        ));
    }
    refused
}

/// The amount an order states it spends: a positive number, not the unset
/// mark.
pub(crate) fn stated_amount(order: &ApiOrder) -> Option<f64> {
    let cash = order.cash_qty;
    (cash > 0.0 && cash.is_finite() && cash != f64::MAX).then_some(cash)
}

/// The field of a fund's definition stating the least amount it is bought in.
const FUND_TICK: u32 = 8482;

/// Whether a gateway lets an order carry a cash amount: a fund only to buy; a
/// crypto except as a stop, a limit or a sale at market; a currency pair where
/// its order types list `CASHQTY`; a share where nothing it is allocated to
/// needs a permission the logon withholds, the venue would size it by an
/// amount ([`sized_by_amount`]), the logon takes amounts on shares for the
/// order's type, and the order goes through an algorithm; nothing else.
fn takes_an_amount(
    shared: &SharedState,
    terms: &MoneyOrderTerms,
    order: &ApiOrder,
    definition: &ContractDefinition,
) -> bool {
    let kind = order.order_type_named().unwrap_or_default();
    let buy = order.side() == Ok(Side::Buy);
    match definition.sec_type {
        SecurityType::Fund => buy,
        SecurityType::Crypto => !matches!(kind, "STP" | "LMT") && !(kind == "MKT" && !buy),
        SecurityType::Forex if takes(definition, "CASHQTY") => true,
        SecurityType::Stock => {
            let allocation = [&order.fa_group, &order.fa_method, &order.fa_percentage];
            let allocated = !order.fa_group.is_empty()
                || (allocation.iter().any(|field| !field.is_empty())
                    && !order.model_code.is_empty());
            !(allocated && !listed(&terms.order_types, "ALLOC"))
                && sized_by_amount(shared, terms, definition)
                && listed(&terms.order_types, list_name(kind))
                && !order.algo_strategy.is_empty()
        }
        _ => false,
    }
}

/// Whether the venue would size an order stated by an amount on this
/// contract itself, as a gateway works it out: not a combination, no rule on
/// its economic value, not a fund traded at its settlement, not on the
/// settlement venue; and either the logon or the contract opens amounts on it.
pub(crate) fn sized_by_amount(
    shared: &SharedState,
    terms: &MoneyOrderTerms,
    definition: &ContractDefinition,
) -> bool {
    let share = definition.sec_type == SecurityType::Stock;
    let crypto = definition.sec_type == SecurityType::Crypto;
    let fund = definition.sec_type == SecurityType::Fund;
    let in_parts = finest_size(shared, definition).is_some_and(|finest| finest < 1.0);
    let open = takes(definition, "CASHQTY");
    let basic = |list: &str| BASIC_TYPES.iter().any(|name| listed(list, name));
    let opened = shared.reference.order_permissions().contains_key("CRYPTO")
        || ((terms.account || shared.reference.refusals_told()) && basic(&terms.types));
    let whole_units = fund && shared.reference.enables("NOMFFRACMR");
    let by_logon = opened
        && ((!share && in_parts && !whole_units)
            || (share && definition.min_size_stated && listed(&terms.types, "CASHQTY")));
    let by_contract = in_parts && open;
    let by_type = (crypto || (share && basic(&terms.order_types))) && open;
    definition.sec_type != SecurityType::Combo
        && definition.ev_rule.trim().is_empty()
        && definition.stock_type != "ETMF"
        && definition.exchange != "CFETAS"
        && (by_logon || by_contract || by_type)
}

/// The finest size the contract's market rule states, across its bands.
pub(crate) fn finest_size(shared: &SharedState, definition: &ContractDefinition) -> Option<f64> {
    let rule = shared.reference.market_rule(definition.market_rule_id? as i32)?;
    rule.size_increments
        .iter()
        .map(|band| band.increment)
        .filter(|size| *size > 0.0)
        .min_by(f64::total_cmp)
}

/// What a gateway refuses of a replace for an order stated by an amount: all
/// of them, where the account may trade crypto or the logon takes amounts on
/// market, limit, stop or stop-limit orders, whether the order working or the
/// replace states the amount. A fund's amount is not held where this looks.
pub(crate) fn modify_refusal(
    shared: &SharedState,
    fund: bool,
    working: Option<&ApiOrder>,
    incoming: &ApiOrder,
) -> Option<Refusal> {
    let by_amount = |order: &ApiOrder| stated_amount(order).is_some();
    let terms = shared.reference.money_orders();
    let takes_amounts = shared.reference.order_permissions().contains_key("CRYPTO")
        || BASIC_TYPES.iter().any(|name| listed(&terms.order_types, name));
    (!fund && takes_amounts && (working.is_some_and(by_amount) || by_amount(incoming))).then(|| {
        Refusal::stated(
            CASH_QUANTITY_NOT_MODIFIED,
            "Order Quantity is expressed in monetary terms. Modification is not supported via \
             API. Please use desktop version to revise this order.",
        )
    })
}

/// One of a gateway's records of a contract's market: the live prices, or the
/// delayed or frozen ones.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Record {
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub last: Option<f64>,
    pub close: Option<f64>,
    pub bid_size: Option<f64>,
    pub ask_size: Option<f64>,
    pub last_size: Option<f64>,
    /// A side stated past its limit is not a side to deal on.
    pub bid_past_low: bool,
    pub ask_past_high: bool,
    /// The close stated as not a close to use.
    pub close_invalid: bool,
}

/// What a gateway's record of a contract's market holds, as far as the size
/// it works out for an amount reads it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Market {
    pub live: Record,
    /// The delayed or frozen record, where the subscription serves one.
    pub late: Option<Record>,
    pub halted: bool,
    /// Whether a subscription feeds the record.
    pub streaming: bool,
    /// The mark the venue states apart from the quote (generic tick 232).
    pub mark: Option<f64>,
    /// What the account's own holding of the contract is marked at.
    pub holding: Option<f64>,
    /// Whether the contract's rule allows prices of nought and below.
    pub negative: bool,
}

impl Market {
    /// The record as this client holds it for an instrument: the live view,
    /// and the delayed or frozen one the subscription is served.
    pub(crate) fn held(
        views: &crate::bridge::PricingQuoteViews,
        halted: bool,
        holding: Option<f64>,
        negative: bool,
    ) -> Self {
        let record = |index: usize| {
            let quote = views.records[index];
            let price = |value: Option<crate::types::Price>| {
                value.map(|value| value as f64 / crate::types::PRICE_SCALE as f64)
            };
            let size = |value: Option<crate::types::Qty>| value.map(crate::types::qty_to_f64);
            Record {
                bid: price(quote.bid),
                ask: price(quote.ask),
                last: price(quote.last),
                close: price(quote.close),
                bid_size: size(quote.bid_size),
                ask_size: size(quote.ask_size),
                last_size: size(quote.last_size),
                bid_past_low: quote.state_mask & 0x02 != 0,
                ask_past_high: quote.state_mask & 0x04 != 0,
                close_invalid: quote.close_attributes != -1 && quote.close_attributes & 1 != 0,
            }
        };
        let mode = usize::try_from(views.mode).unwrap_or(0).min(3);
        Market {
            live: record(0),
            late: (mode != 0).then(|| record(mode)),
            halted,
            streaming: views.confirmed,
            mark: views.mark_price,
            holding,
            negative,
        }
    }
}

/// Whether a price is one: stated, and a number.
fn valid(price: f64) -> bool {
    price != f64::MAX && price.is_finite()
}

/// A stated price, `MAX` where none is.
fn stated(price: Option<f64>) -> f64 {
    price.filter(|price| valid(*price)).unwrap_or(f64::MAX)
}

impl Record {
    fn sized(size: Option<f64>) -> bool {
        size.is_some_and(|size| size != 0.0 && size.is_finite())
    }

    /// Whether each side is one a midpoint is taken from: sized, and above
    /// nought where the rule allows nothing lower.
    fn sides_for_midpoint(&self, negative: bool) -> (bool, bool) {
        let side = |price: Option<f64>, size| {
            Record::sized(size) && valid(stated(price)) && (stated(price) > 0.0 || negative)
        };
        (side(self.bid, self.bid_size), side(self.ask, self.ask_size))
    }

    fn midpoint(&self, negative: bool) -> f64 {
        match self.sides_for_midpoint(negative) {
            (true, true) => (stated(self.bid) + stated(self.ask)) / 2.0,
            _ => f64::MAX,
        }
    }

    fn last(&self) -> f64 {
        let last = stated(self.last);
        let traded = self.last_size.is_some_and(|size| size > 0.0);
        if valid(last) && (traded || last > 0.0) { last } else { f64::MAX }
    }

    fn close(&self, negative: bool) -> f64 {
        let close = stated(self.close);
        if valid(close) && (close >= 0.0 || negative) && !self.close_invalid {
            close
        } else {
            f64::MAX
        }
    }

    fn bid_usable(&self) -> bool {
        Record::sized(self.bid_size) && valid(stated(self.bid)) && !self.bid_past_low
    }

    fn ask_usable(&self) -> bool {
        Record::sized(self.ask_size) && valid(stated(self.ask)) && !self.ask_past_high
    }
}

/// How a price is asked of the record: to value a holding, which states
/// nothing on the settlement venue, or as the record stands.
#[derive(Clone, Copy, PartialEq)]
enum Usage {
    ValuePosition,
    AsIs,
}

/// The record's price as a gateway reads it for a use, from the live prices
/// alone: the venue's own mark; the holding's mark where the contract is
/// halted or a subscription states nothing; the last trade, or the midpoint
/// on a currency pair or where the contract's order types ask for it, or the
/// close; held inside the bid and ask; the midpoint of one side and nought
/// where only one is stated, as a gateway works it; the holding's mark.
fn price_as(market: &Market, definition: &ContractDefinition, usage: Usage) -> f64 {
    if usage == Usage::ValuePosition && definition.exchange == "CFETAS" {
        return f64::MAX;
    }
    if valid(stated(market.mark)) {
        return stated(market.mark);
    }
    let live = &market.live;
    let holding = stated(market.holding);
    let midpoint = live.midpoint(market.negative);
    let silent = market.streaming
        && !valid(live.last())
        && live.sides_for_midpoint(market.negative) == (false, false);
    if (market.halted || silent) && valid(holding) {
        return holding;
    }
    let midpoint_first =
        matches!(definition.sec_type, SecurityType::Forex | SecurityType::Commodity)
            || takes(definition, "USEMID");
    let base = if valid(live.last()) {
        live.last()
    } else if midpoint_first && valid(midpoint) {
        midpoint
    } else {
        live.close(market.negative)
    };
    let (bid, ask) = (stated(live.bid), stated(live.ask));
    let (bid_usable, ask_usable) = (live.bid_usable(), live.ask_usable());
    if valid(base) {
        let crossed = bid_usable && ask_usable && bid > ask;
        return if bid_usable && base < bid && !crossed {
            bid
        } else if ask_usable && base > ask && !crossed {
            ask
        } else {
            base
        };
    }
    if ask_usable {
        return (if bid_usable { bid } else { 0.0 } + ask) / 2.0;
    }
    if bid_usable {
        return bid;
    }
    holding
}

/// The price a gateway converts an amount at: the live midpoint, last trade
/// or close; with `later`, the delayed or frozen ones after each; then the
/// record as it stands; then, for a currency pair, the rate between its two
/// currencies.
fn conversion(market: &Market, definition: &ContractDefinition, later: bool, cross: f64) -> f64 {
    let live = &market.live;
    let late = market.late.filter(|_| later).unwrap_or_default();
    let late_last = stated(late.last);
    let chain = [
        live.midpoint(market.negative),
        late.midpoint(market.negative),
        live.last(),
        stated(live.close),
        if valid(late_last) && (late_last > 0.0 || market.negative) { late_last } else { f64::MAX },
        stated(late.close),
        price_as(market, definition, Usage::AsIs),
    ];
    match chain.into_iter().find(|price| valid(*price)) {
        Some(price) => price,
        None if definition.sec_type == SecurityType::Forex && valid(cross) && cross > 0.0 => cross,
        None => f64::MAX,
    }
}

/// The rate a currency pair's two currencies convert at, from the account's
/// ledger, or the logon's fixed rates where the ledger states none; `MAX`
/// where either is not held.
pub(crate) fn cross_rate(
    ledger: &[(String, f64)],
    fixed_rates: &str,
    definition: &ContractDefinition,
) -> f64 {
    // ponytail: the fixed rates are into dollars and a gateway converts them
    // into the account's base currency, which is not held here; the base
    // cancels where both currencies come from the same source.
    let fixed = |currency: &str| {
        if currency == "USD" && !fixed_rates.is_empty() {
            return Some(1.0);
        }
        fixed_rates.split(',').find_map(|rate| {
            let (name, value) = rate.split_once(':')?;
            (name == currency).then(|| value.trim().parse::<f64>().ok()).flatten()
        })
    };
    let rate = |currency: &str| {
        ledger
            .iter()
            .find(|(name, _)| name == currency)
            .map(|(_, rate)| *rate)
            .or_else(|| fixed(currency))
    };
    match (rate(&definition.symbol), rate(&definition.currency)) {
        (Some(base), Some(quote)) if base > 0.0 && valid(base) && quote > 0.0 && valid(quote) => {
            base / quote
        }
        _ => f64::MAX,
    }
}

/// Whether a gateway works out a size for an order stated by an amount that
/// it takes, for its own record and, where it states one, for tag 38: where
/// the amount is not a fund's and the caller's size does not pass through —
/// a currency pair's does wherever one is stated, any other's where the venue
/// sizes it by an amount.
pub(crate) fn works_out_size(
    shared: &SharedState,
    order: &ApiOrder,
    definition: &ContractDefinition,
) -> bool {
    let cash = stated_amount(order).is_some();
    let terms = shared.reference.money_orders();
    let passes = order.total_quantity > 0.0
        && (definition.sec_type == SecurityType::Forex
            || (definition.sec_type != SecurityType::Fund
                && sized_by_amount(shared, &terms, definition)));
    let typed = match definition.sec_type {
        SecurityType::Stock => BASIC_TYPES.iter().any(|name| listed(&terms.order_types, name)),
        SecurityType::Crypto => true,
        SecurityType::Forex => takes(definition, "CASHQTY"),
        _ => false,
    };
    typed && cash && !passes
}

/// Whether a gateway sends no size for an order stated by an amount and the
/// venue works it out: a crypto's always, a share's where the logon offers
/// `DISABLECASHQTYOVEREST` and a currency pair's where it offers
/// `DISABLEFXCASHQTYOVEREST`. These are the orders it works a size out for
/// at the amount as stated; any other it works out at a preset's margin.
pub(crate) fn sized_by_the_venue(shared: &SharedState, sec_type: &str, cash: bool) -> bool {
    cash && match sec_type {
        "CRYPTO" => true,
        "CS" | "STK" => shared.reference.enables("DISABLECASHQTYOVEREST"),
        "CASH" => shared.reference.enables("DISABLEFXCASHQTYOVEREST"),
        _ => false,
    }
}

/// The margin a gateway works a size out at from a preset's percentage:
/// five to a hundred percent over the amount, a quarter otherwise.
pub(crate) fn margin(percent: i32) -> f64 {
    if (5..=100).contains(&percent) { (100.0 + f64::from(percent)) / 100.0 } else { 1.25 }
}

/// The size a gateway works out for an order stated by an amount, as its own
/// record of the order holds it until the venue states the order's size.
///
/// The amount is converted at the market's price (see [`conversion`]), a
/// buy's no higher than its limit and a sale's no lower, where the order type
/// states one. On a contract dealt in parts of a unit, the amount to four
/// places over the order's own price to eight — its limit, its stop, its
/// trailing stop or its starting price, by type, or the record's for an
/// order at market, or the market's where it states none — at the margin,
/// rounded half to even to the places of the finest size, and nought where
/// either is not a positive number. Otherwise the amount over the price at
/// the margin, rounded to the places of the contract's least size and then up
/// to a whole unit — unless parts of a unit are open to the order — and to a
/// lot where the order's venue deals in them.
pub(crate) fn estimate(
    shared: &SharedState,
    order: &ApiOrder,
    venue: &str,
    definition: &ContractDefinition,
    market: &Market,
    cross: f64,
    margin: f64,
) -> f64 {
    let cash = order.cash_qty;
    let kind = order.order_type_named().unwrap_or_default();
    let buy = order.side() == Ok(Side::Buy);
    let limit =
        if valid(order.lmt_price) && order.lmt_price != 0.0 { order.lmt_price } else { f64::MAX };
    let mut rate = conversion(market, definition, false, cross);
    if caps_at_the_limit(kind) {
        rate = if buy { rate.min(limit) } else { rate.max(limit) };
    }
    if !valid(rate) {
        log::error!("Contract conversion rate is unknown for {}", definition.con_id);
    }
    let finest = finest_size(shared, definition);
    if let Some(finest) = finest.filter(|finest| *finest < 1.0) {
        let stated = order_price(order, kind, market, definition);
        let mark =
            if stated == f64::MAX { conversion(market, definition, true, cross) } else { stated };
        if !(valid(cash) && cash > 0.0 && valid(mark) && mark > 0.0) {
            return 0.0;
        }
        let places = Decimal::of(finest, 18).fraction().len() as u32;
        let amount = Decimal::of(cash, 4);
        let mark = Decimal::of(mark, 8);
        let spent = if margin == 1.0 { amount } else { amount.times(&Decimal::of(margin, 17)) };
        let size = if mark == Decimal::unit() { spent } else { spent.over(&mark, places) };
        return if size.mantissa > 0 { size.truncated_to_its_magnitude().value() } else { 0.0 };
    }
    let size = cash / rate * margin;
    let step = least_size(shared, definition);
    let size = half_up(size, -(step.log10() as i32));
    let whole = size.ceil().min(f64::from(i32::MAX)).max(f64::from(i32::MIN));
    let in_parts = fractional_allowed(shared, order, venue, definition)
        && !held_to_regular_hours(shared, order);
    let size = if in_parts { size } else { whole };
    if lots(shared, venue, definition) { to_a_lot(shared, definition, whole) } else { size }
}

/// The size a gateway resizes its own record of a working order to when a
/// replace changes the amount, where it worked the order's size out: the
/// amount at the margin over the order's price, less what has filled, up to
/// the places of the finest size on a contract dealt in parts, up to a whole
/// unit where parts of a unit are not open to it, and to a lot. `None` leaves
/// the record as it is: the order's price is not one. The replace states the
/// caller's size on the wire all the same.
///
/// The order's price is its limit, or, on a buy, the live ask where the limit
/// is above it, and on a sale the live bid where the limit is below it; its
/// stop, trailing stop or starting price, by type; the record's for an order
/// at market.
// ponytail: a gateway rounds a size dealt in parts up to its rule's size
// format, which this client does not read; the finest size's places stand in.
pub(crate) fn resized(
    shared: &SharedState,
    order: &ApiOrder,
    venue: &str,
    definition: &ContractDefinition,
    market: &Market,
    margin: f64,
    filled: f64,
) -> Option<f64> {
    let kind = order.order_type_named().unwrap_or_default();
    let buy = order.side() == Ok(Side::Buy);
    let limit =
        if valid(order.lmt_price) && order.lmt_price != 0.0 { order.lmt_price } else { f64::MAX };
    let live = &market.live;
    let price = match kind {
        "LMT" | "LIT" | "LOC" if buy && live.ask_usable() && limit > stated(live.ask) => {
            stated(live.ask)
        }
        "LMT" | "LIT" | "LOC" if !buy && live.bid_usable() && limit < stated(live.bid) => {
            stated(live.bid)
        }
        "LMT" | "LIT" | "LOC" => limit,
        kind => order_price(order, kind, market, definition),
    };
    if !valid(price) {
        return None;
    }
    let size = (margin * order.cash_qty / price - filled).max(0.0);
    let size = match finest_size(shared, definition).filter(|finest| *finest < 1.0) {
        Some(finest) => {
            let places = Decimal::of(finest, 18).fraction().len() as i32;
            let scale = 10_f64.powi(places);
            (Decimal::of(size, 6).value() * scale).ceil() / scale
        }
        None if !fractional_allowed(shared, order, venue, definition)
            || held_to_regular_hours(shared, order) =>
        {
            size.ceil()
        }
        None => half_up(size, -(least_size(shared, definition).log10() as i32)),
    };
    let lotted =
        lots(shared, venue, definition) || !fractional_allowed(shared, order, venue, definition);
    Some(if lotted { to_a_lot(shared, definition, size) } else { size })
}

/// The price an order of a type states for an amount: its limit, stop,
/// trailing stop or starting price, or the record's for an order at market;
/// `MAX` where it states none.
fn order_price(
    order: &ApiOrder,
    kind: &str,
    market: &Market,
    definition: &ContractDefinition,
) -> f64 {
    let limit =
        if valid(order.lmt_price) && order.lmt_price != 0.0 { order.lmt_price } else { f64::MAX };
    match kind {
        "LMT" | "LIT" | "LOC" => limit,
        "STP" | "STP PRT" | "STP LMT" | "MIT" | "LMT + MKT" | "REL + MKT" => order.aux_price,
        "TRAIL" | "TRAIL LIMIT" => order.trail_stop_price,
        "PEG STK" | "PEG BENCH" => order.starting_price,
        kind if at_market(kind) => price_as(market, definition, Usage::ValuePosition),
        _ => f64::MAX,
    }
}

/// The order types whose limit bounds the rate an amount converts at.
fn caps_at_the_limit(kind: &str) -> bool {
    matches!(
        kind,
        "LMT"
            | "LIT"
            | "LOC"
            | "STP LMT"
            | "TRAIL LIMIT"
            | "TRAIL LIT"
            | "REL"
            | "PEG PRIM"
            | "RPI"
            | "REL + LMT"
            | "MIDPRICE"
            | "PASSV REL"
            | "PEG MKT"
            | "PEG MID"
            | "PEG BEST"
            | "PEG PRIM VOL"
            | "PEG MID VOL"
            | "PEG MKT VOL"
            | "PEG SRF VOL"
            | "FUNARI"
            | "QUOTE"
            | "IBALGO"
            | "DEFAULT"
            | "AUTO"
    )
}

/// The order types priced at the market's price.
fn at_market(kind: &str) -> bool {
    matches!(
        kind,
        "MKT"
            | "MKT PRT"
            | "MTL"
            | "PEG MKT"
            | "PEG MID"
            | "PEG BEST"
            | "MIDPRICE"
            | "SNAP MKT"
            | "SNAP MID"
            | "SNAP PRIM"
            | "MOC"
            | "BOX TOP"
            | "PASSV REL"
            | "REL"
            | "PEG PRIM"
            | "RPI"
            | "REL + LMT"
            | "FUNARI"
            | "PEG PRIM VOL"
            | "PEG MID VOL"
            | "PEG MKT VOL"
            | "PEG SRF VOL"
    )
}

/// Rounded half up to `places`, as a gateway rounds a double.
fn half_up(value: f64, places: i32) -> f64 {
    if value == f64::MAX {
        return value;
    }
    let scale = 10_f64.powi(places);
    let rounded = (value * scale + 0.5).floor();
    if rounded.is_nan() { 0.0 } else { rounded.min(i64::MAX as f64) / scale }
}

/// The least size a whole-unit contract states: the one behind its flag, a
/// ten-thousandth where it states the flag alone; otherwise the part of a
/// unit the logon states where it has more than four places, and a
/// ten-thousandth where it has not.
fn least_size(shared: &SharedState, definition: &ContractDefinition) -> f64 {
    let ten_thousandth = 0.0001;
    if definition.min_size_stated {
        return Decimal::parse(&definition.min_size_text)
            .map_or(ten_thousandth, |least| least.value());
    }
    match Decimal::parse(&shared.reference.size_fraction()) {
        Some(stated) if stated.scale > 4 => stated.value(),
        _ => ten_thousandth,
    }
}

/// Whether a gateway allows the order a size in parts of a unit: the
/// contract states its flag or its rule deals in parts; an algorithm, an
/// allocation or a model is one the logon or contract opens parts of a unit
/// to; nothing about the order rules them out (a name among its references
/// that a gateway's own windows use, the accumulate-distribute algorithm, a
/// container, a scale); a share's account, type and time in force take them
/// and it routes through a venue that chooses; and it opens a position only
/// where the logon lets parts of a unit do so.
fn fractional_allowed(
    shared: &SharedState,
    order: &ApiOrder,
    venue: &str,
    definition: &ContractDefinition,
) -> bool {
    let terms = shared.reference.money_orders();
    let share = definition.sec_type == SecurityType::Stock;
    let in_parts = finest_size(shared, definition).is_some_and(|finest| finest < 1.0)
        && !(definition.sec_type == SecurityType::Fund && shared.reference.enables("NOMFFRACMR"));
    let kind = order.order_type_named().unwrap_or_default();
    let fractional_list = |name: &str| listed(&terms.types, name);
    let opened_to =
        |name: &str| if share { fractional_list(name) } else { takes(definition, name) };
    let algo_open = order.algo_strategy.is_empty() || opened_to("ALGO");
    let scale_capable = matches!(
        kind,
        "LMT"
            | "REL + LMT"
            | "MIT"
            | "REL + MKT"
            | "LMT + MKT"
            | "PASSV REL"
            | "PEG MKT"
            | "PEG MID"
            | "PEG BEST"
            | "REL"
            | "RPI"
    );
    let excluded = (!in_parts
        && ["BlotterCrossOrder", "CrossEntry", "Blotter", "IntegratedTicket", "MergerArb"]
            .contains(&order.order_ref.as_str()))
        || order.algo_strategy.eq_ignore_ascii_case("AD")
        || order.is_oms_container
        || (scale_capable
            && order.scale_init_level_size > 0
            && order.scale_init_level_size != i32::MAX);
    let allocated = !order.fa_group.is_empty() || !order.model_code.is_empty();
    let alloc_open = !allocated || opened_to("ALLOC");
    // Parts of a unit on a contract other than a share, dealt in parts.
    let unit_free = !share && in_parts;
    let tif = order.tif.to_uppercase();
    let tif_open =
        !(share && fractional_list("DAY") && fractional_list("GTC")) || fractional_list(&tif);
    let general = (definition.min_size_stated || in_parts)
        && (!share || (terms.account && fractional_list(list_name(kind))))
        && tif_open
        && (unit_free || matches!(venue, "BEST" | "SMART" | "ZERO"))
        && !shared.reference.enables("NOOPENFRAC");
    algo_open && !excluded && alloc_open && general
}

/// Whether the logon holds an amount to regular hours and the order may
/// trade outside them.
fn held_to_regular_hours(shared: &SharedState, order: &ApiOrder) -> bool {
    listed(&shared.reference.money_orders().types, "RTHONLY") && order.outside_rth
}

/// Whether the order's venue deals the contract in lots.
fn lots(shared: &SharedState, venue: &str, definition: &ContractDefinition) -> bool {
    let routed = shared
        .reference
        .contract_definition(definition.con_id, venue)
        .unwrap_or_else(|| definition.clone());
    takes(&routed, "MINLOT") || takes(&routed, "BOARDLOT")
}

/// A whole size brought to the contract's lot: raised to one lot where it is
/// less, and, where the contract deals in board lots and not in minimum lots,
/// up to a whole number of them.
fn to_a_lot(shared: &SharedState, definition: &ContractDefinition, size: f64) -> f64 {
    let minimum = takes(definition, "MINLOT");
    if !minimum && !takes(definition, "BOARDLOT") {
        return size;
    }
    let rule = definition.market_rule_id.and_then(|id| shared.reference.market_rule(id as i32));
    let lot = rule.map_or(0.0, |rule| {
        let band = rule
            .size_increments
            .iter()
            .rev()
            .find(|band| band.low_edge <= 0.0)
            .map_or(0.0, |band| band.increment);
        if band > 0.0 && band < 1.0 {
            return band;
        }
        let us_listed = matches!(
            (&definition.sec_type, definition.market_classification.as_str()),
            (SecurityType::Stock, "USSTK") | (SecurityType::Warrant, "USWAR")
        );
        let whole = if !us_listed {
            1.0
        } else {
            finest_size(shared, definition)
                .map(f64::trunc)
                .filter(|finest| *finest > 0.0)
                .unwrap_or(100.0)
        };
        band.max(whole)
    });
    let suggested = definition.suggested_size;
    let lot = if suggested > 0.0 && matches!(definition.exchange.as_str(), "BEST" | "SMART") {
        suggested
    } else if shared.reference.enables("MINSIZESMART")
        && matches!(definition.exchange.as_str(), "SMART" | "ZERO")
        && suggested == 0.0
    {
        1.0
    } else {
        suggested.max(lot)
    };
    if size < lot {
        lot
    } else if minimum || lot == 0.0 || size % lot == 0.0 {
        size
    } else {
        (size / lot).ceil() * lot
    }
}

/// Whether a list the logon states names an entry, as a gateway reads one:
/// entries separated by commas, each a name with a mark after a slash, a mark
/// of 5 where none is stated; the first entry of the name counts unless
/// marked 4.
fn listed(list: &str, name: &str) -> bool {
    list.split(',')
        .map(|entry| {
            let mut parts = entry.split('/');
            (parts.next().unwrap_or(""), parts.next().map_or(5, |mark| mark.parse().unwrap_or(0)))
        })
        .find(|(named, _)| *named == name)
        .is_some_and(|(_, mark)| mark != 4)
}

/// Whether the contract's own order types list an entry: the first under the
/// name, unless marked 4.
fn takes(definition: &ContractDefinition, name: &str) -> bool {
    definition
        .order_type_rules
        .iter()
        .find(|(named, _)| named == name)
        .is_some_and(|(_, mark)| *mark != 4)
}

/// An order type as the logon's lists name it.
fn list_name(kind: &str) -> &str {
    match kind {
        "STP LMT" => "STPLMT",
        "STP PRT" => "STPPROT",
        "TRAIL LIMIT" => "TRAILLMT",
        "TRAIL LIT" => "TRAILLIT",
        "TRAIL MIT" => "TRAILMIT",
        "MKT PRT" => "MKTPROT",
        kind => kind,
    }
}

/// The least amount a currency moves in, as a gateway holds it. Its own list
/// until the logon's product defaults state currencies under `CASH`, which
/// replace it; each at the precision stated for it, or, where that is none or
/// nought, the one its own list holds for it (a yen whole, a hundredth
/// otherwise). A currency the logon states a fixed rate for and the list does
/// not hold moves in hundredths; any other in whole units.
fn currency_increment(terms: &MoneyOrderTerms, currency: &str) -> Decimal {
    const HELD: [&str; 13] = [
        "AUD", "CAD", "CHF", "EUR", "GBP", "HKD", "JPY", "KRW", "SEK", "MXN", "NOK", "USD", "BASE",
    ];
    let held = |name: &str| if name == "JPY" { Decimal::unit() } else { Decimal::hundredth() };
    let stated: Vec<(&str, Option<&str>)> = terms
        .product_defaults
        .split(';')
        .filter_map(|entry| {
            let mut tokens = entry.split(',').filter(|token| !token.is_empty());
            let kind = tokens.next()?;
            let product = tokens.next().unwrap_or("");
            (kind == "CASH").then(|| (product, tokens.nth(2)))
        })
        .collect();
    let listed = if stated.is_empty() {
        HELD.contains(&currency).then(|| held(currency))
    } else {
        stated.iter().find(|(product, _)| *product == currency).map(|(_, precision)| {
            precision
                .and_then(Decimal::parse)
                .filter(|precision| precision.mantissa != 0)
                .unwrap_or_else(|| held(currency))
        })
    };
    let fixed = !terms.fixed_rates.is_empty()
        && (currency == "USD"
            || terms.fixed_rates.split(',').any(|rate| rate.split(':').next() == Some(currency)));
    listed.unwrap_or(if fixed { Decimal::hundredth() } else { Decimal::unit() })
}

/// An amount as a gateway's refusal states it: at least two places and up to
/// eight, rounded half to even.
fn cash_text(cash: f64) -> String {
    let decimal = Decimal::of(cash, 8);
    format!("{}.{:0<2}", decimal.whole(), decimal.fraction())
}

/// A decimal number held exactly: its digits and how many of them are places.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Decimal {
    mantissa: i128,
    scale: u32,
}

impl Decimal {
    fn unit() -> Self {
        Decimal { mantissa: 1, scale: 0 }
    }

    fn hundredth() -> Self {
        Decimal { mantissa: 1, scale: 2 }
    }

    /// A double's digits as a gateway formats one: the shortest that read back
    /// as the same double, rounded half to even to `places`.
    fn of(value: f64, places: usize) -> Self {
        let text = format!("{}", value.abs());
        let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
        let (whole, fraction) = rounded_half_even(whole, fraction, places);
        let sign = if value < 0.0 { "-" } else { "" };
        Decimal::parse(&format!("{sign}{whole}.{fraction}"))
            .unwrap_or(Decimal { mantissa: 0, scale: 0 })
    }

    /// A decimal as written, its trailing zeros dropped.
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim().replace(',', "");
        let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
        let fraction = fraction.trim_end_matches('0');
        let digits = format!("{whole}{fraction}");
        if digits.is_empty()
            || digits == "-"
            || !digits.trim_start_matches('-').bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some(Decimal { mantissa: digits.parse().ok()?, scale: fraction.len() as u32 })
    }

    fn whole(&self) -> String {
        (self.mantissa / 10_i128.pow(self.scale)).to_string()
    }

    fn fraction(&self) -> String {
        if self.scale == 0 {
            return String::new();
        }
        format!(
            "{:0>width$}",
            (self.mantissa % 10_i128.pow(self.scale)).abs(),
            width = self.scale as usize
        )
    }

    /// As a gateway writes an increment: its digits, no trailing zeros.
    fn text(&self) -> String {
        if self.scale == 0 { self.whole() } else { format!("{}.{}", self.whole(), self.fraction()) }
    }

    fn value(&self) -> f64 {
        self.text().parse().unwrap_or(0.0)
    }

    /// The product, to sixteen significant digits rounded half to even.
    fn times(&self, other: &Decimal) -> Decimal {
        let mut product =
            Decimal { mantissa: self.mantissa * other.mantissa, scale: self.scale + other.scale };
        while product.mantissa.abs() >= 10_i128.pow(16) && product.scale > 0 {
            product = product.over(&Decimal::unit(), product.scale - 1);
        }
        product
    }

    /// The quotient to `places`, rounded half to even.
    fn over(&self, divisor: &Decimal, places: u32) -> Decimal {
        let numerator = self.mantissa * 10_i128.pow(divisor.scale + places);
        let denominator = divisor.mantissa * 10_i128.pow(self.scale);
        let (quotient, remainder) = (numerator / denominator, numerator % denominator);
        let twice = (remainder * 2).abs();
        let up = twice > denominator.abs() || (twice == denominator.abs() && quotient % 2 != 0);
        let sign = if (numerator < 0) != (denominator < 0) { -1 } else { 1 };
        Decimal { mantissa: quotient + if up { sign } else { 0 }, scale: places }
    }

    /// As a gateway turns a size worked out in decimals into its own
    /// fraction: the places kept are the decimal's less the digits of its
    /// whole part beyond the first, truncated.
    fn truncated_to_its_magnitude(&self) -> Decimal {
        let magnitude = self.value().abs().log10().max(0.0) as u32;
        let kept = self.scale as i64 - i64::from(magnitude);
        if kept >= 0 {
            Decimal {
                mantissa: self.mantissa / 10_i128.pow(self.scale - kept as u32),
                scale: kept as u32,
            }
        } else {
            Decimal { mantissa: self.mantissa / 10_i128.pow(self.scale), scale: 0 }
        }
    }

    /// Whether this is a whole number of `step`s.
    // ponytail: exact rational test; a gateway divides to sixteen significant
    // digits, which differs only past sixteen digits of quotient.
    fn multiple_of(&self, step: &Decimal) -> bool {
        step.mantissa != 0
            && (self.mantissa * 10_i128.pow(step.scale)) % (step.mantissa * 10_i128.pow(self.scale))
                == 0
    }
}

/// A decimal's whole and fractional digits rounded half to even to `places`,
/// the fraction's trailing zeros dropped.
pub(crate) fn rounded_half_even(whole: &str, fraction: &str, places: usize) -> (String, String) {
    if fraction.len() <= places {
        return (whole.to_string(), fraction.trim_end_matches('0').to_string());
    }
    let kept = &fraction[..places];
    let dropped = &fraction[places..];
    let first = dropped.as_bytes()[0];
    let beyond = dropped[1..].bytes().any(|b| b != b'0');
    let mut digits: Vec<u8> = format!("0{whole}{kept}").into_bytes();
    let last_odd = digits.last().is_some_and(|d| (d - b'0') % 2 == 1);
    let up = first > b'5' || (first == b'5' && (beyond || last_odd));
    if up {
        let mut i = digits.len();
        loop {
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let text = String::from_utf8(digits).unwrap_or_default();
    let (w, f) = text.split_at(text.len() - places);
    (w.to_string(), f.trim_end_matches('0').to_string())
}
