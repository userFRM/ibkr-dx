//! What a gateway's option model publishes for an option on its own ticks.
//!
//! The venue states the greeks and its model's price for an option on one
//! series, and the volatilities they are worked from on others. A gateway
//! publishes the greeks and the price as the venue states them, the first of
//! the model's, the mid's and the last's volatility that stands, over a year of
//! trading days, and the underlying's price the chain parameters on the
//! underlying state for the option's expiry. It rebuilds that at most once a
//! second, on the clock's own seconds, for the options something new was
//! stated for.
//!
//! Beside it, it publishes one for each of the option's bid, ask and last: at
//! the volatility the venue states for that side, with that side's price, and
//! greeks worked out by a model of its own. Those are rebuilt at most once
//! every two seconds.

use super::{
    A_YEAR_OF_TRADING_DAYS, CHAIN_MODEL_SERIES, Connection, FarmState, GREEKS_VENUE,
    HeartbeatState, InstrumentId, ModelledOption, OPTION_VOLATILITY_SERIES, OptionTerms,
    SharedState, UnderlyingModel, build_model_subscribe_tags, chrono_free_timestamp, fix,
    series_f64, series_i32,
};
use crate::bridge::{OptionTick, OptionTickKind};
use crate::control::contracts::ContractSchedule;
use crate::options::{Dividend, RatePoint};
use crate::protocol::chain_model::ChainModelParameters;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use std::time::Instant;

/// A figure not stated.
const UNSTATED: f64 = f64::MAX;

/// How often the model reads the clock: once a minute, counted from when it
/// started.
const MODEL_CLOCK_MILLIS: i64 = 60_000;

/// How many options' bid, ask and last ticks are built on one turn of the
/// engine's loop.
const OPTIONS_BUILT_PER_TURN: usize = 64;

/// Where each side's volatility is kept among an option's volatilities, and
/// the side it is.
const SIDES: [(usize, OptionTickKind); 3] =
    [(3, OptionTickKind::Bid), (4, OptionTickKind::Ask), (2, OptionTickKind::Last)];

fn stated(figure: f64) -> bool {
    figure != UNSTATED && figure.is_finite()
}

/// A volatility the venue states, where it stands: attributes that mark it
/// valid, and a figure above nought and finite. One that does not stand
/// leaves the one before it standing.
fn standing(volatility: f64, attributes: i32) -> Option<(f64, i32)> {
    (attributes > 0 && attributes & 1 != 0 && volatility > 0.0 && volatility.is_finite())
        .then_some((volatility, attributes))
}

/// The volatilities one of the model's series states, each with where it is
/// kept. The model's, the mid's and the last's state their attributes and then
/// the figure per trading day; the bid's and the ask's are stated together,
/// the two figures and then the attributes they share.
fn stated_volatilities(series: u32, payload: &[u8]) -> Vec<(usize, Option<(f64, i32)>)> {
    let one = |at: usize| {
        let volatility = series_i32(payload, 0)
            .zip(series_f64(payload, 4))
            .and_then(|(attributes, volatility)| standing(volatility, attributes));
        vec![(at, volatility)]
    };
    match series {
        734 => one(0),
        735 => one(1),
        737 => one(2),
        _ => match (series_f64(payload, 0), series_f64(payload, 8), series_i32(payload, 16)) {
            (Some(bid), Some(ask), Some(attributes)) => {
                vec![(3, standing(bid, attributes)), (4, standing(ask, attributes))]
            }
            _ => Vec::new(),
        },
    }
}

/// The underlying's price the chain parameters state for an option: that of
/// the first set covering the option's trading class at its multiplier, where
/// the set has a term for the option's last trading day and says its price
/// stands.
///
/// Where they state none a gateway takes the underlying's mark. The rule it
/// marks the underlying by is not carried here, so no price is stated in its
/// place.
fn underlying_price(
    sets: &[ChainModelParameters],
    class: &str,
    multiplier: f64,
    last_trading_day: &str,
) -> f64 {
    sets.iter()
        .find(|set| {
            set.classes.iter().any(|(named, _)| named == class) && at_multiplier(set, multiplier)
        })
        .filter(|set| set.terms.iter().any(|term| term.last_trade_date == last_trading_day))
        .and_then(|set| set.underlying_price)
        .unwrap_or(UNSTATED)
}

/// Whether a set of chain parameters covers a multiplier, compared as the
/// figure it is.
fn at_multiplier(set: &ChainModelParameters, multiplier: f64) -> bool {
    set.classes.iter().flat_map(|(_, at)| at).any(|m| m.to_bits() == multiplier.to_bits())
}

/// The underlying's price the chain parameters state for an option's bid,
/// ask and last, as a gateway's model takes it where it holds no quote of the
/// underlying's: that of the first set covering the option's trading class at
/// its multiplier; where no set names the class, that of the only set there
/// is, or else of the first with a term for the option's last trading day at
/// its multiplier; where the set says its price stands.
fn modelled_underlying_price(
    sets: &[ChainModelParameters],
    class: &str,
    multiplier: f64,
    last_trading_day: &str,
) -> f64 {
    let names = |set: &ChainModelParameters| set.classes.iter().any(|(named, _)| named == class);
    let set = if sets.iter().any(names) {
        sets.iter().find(|set| names(set) && at_multiplier(set, multiplier))
    } else if let [only] = sets {
        Some(only)
    } else {
        sets.iter().find(|set| {
            at_multiplier(set, multiplier)
                && set.terms.iter().any(|term| term.last_trade_date == last_trading_day)
        })
    };
    set.and_then(|set| set.underlying_price).unwrap_or(UNSTATED)
}

/// The chain parameters held for an option's underlying.
fn chain_of<'a>(held: &'a [UnderlyingModel], terms: &OptionTerms) -> &'a [ChainModelParameters] {
    held.iter()
        .find(|held| Some(held.con_id) == terms.underlying)
        .map_or(&[], |held| held.sets.as_slice())
}

/// A side's tick as last built, with the volatility it was built at.
pub(super) type BuiltSide = (OptionTick, Option<(f64, i32)>);

/// What an option's own model is worked from beyond the volatility, the
/// underlying's price and the clock.
pub(super) struct ModelInputs {
    /// The moment the option's time runs out, in milliseconds since the
    /// epoch, and whether that moment is a date alone.
    expiry: i64,
    date_only: bool,
    /// Whether the currency's rates were in hand when the rate was worked out.
    rated: bool,
    /// The rate at the option's term: not a number before the currency's
    /// rates are in hand.
    rate: f64,
    /// The present value of the dividends the option's life covers, at that
    /// rate.
    pv_dividend: f64,
    /// Each payment the underlying's schedule states: its ex-date, the
    /// midnight that date begins at on the model's calendar, and its amount.
    payments: Vec<(Date, i64, f64)>,
    /// What a payment is multiplied by for tax: one where the session does
    /// not adjust for it.
    tax: f64,
}

/// The calendar the model states its dates on.
fn model_calendar() -> Option<TimeZone> {
    crate::protocol::datetime::clock_named("EST5EDT")
}

/// A date as the venue writes it, `YYYYMMDD`.
fn date_of(stated: &str) -> Option<Date> {
    let at = |from: usize, to: usize| stated.get(from..to)?.parse::<i16>().ok();
    Date::new(at(0, 4)?, at(4, 6)? as i8, at(6, 8)? as i8).ok()
}

/// The midnight a date begins at on a calendar, in milliseconds.
fn midnight(date: Date, zone: &TimeZone) -> Option<i64> {
    Some(date.to_zoned(zone.clone()).ok()?.timestamp().as_millisecond())
}

/// The day a moment falls on on a calendar, and the midnight it began at.
fn day_of(at: i64, zone: &TimeZone) -> Option<(Date, i64)> {
    let date = jiff::Timestamp::from_millisecond(at).ok()?.to_zoned(zone.clone()).date();
    Some((date, midnight(date, zone)?))
}

/// Whole days from one date to another.
fn days_between(from: Date, to: Date) -> Option<i32> {
    Some(from.until(to).ok()?.get_days())
}

/// The moment an option's time runs out, and whether it is a date alone, as a
/// gateway resolves it on the zone the option's sessions are stated on: its
/// real expiry, where that is another day than its last trading day, as that
/// date; else its last trading day at the time of day its definition states;
/// else at the end of that day's last liquid session, where its sessions
/// state that day; else that date.
fn expiry_of(terms: &OptionTerms, schedule: &ContractSchedule) -> Option<(i64, bool)> {
    let zone = crate::protocol::datetime::clock_named(&schedule.timezone).unwrap_or(TimeZone::UTC);
    let last_day = terms.last_trading_day.as_str();
    let real = terms.real_expiration.get(..8).unwrap_or_default();
    if !real.is_empty() && real != last_day {
        return Some((midnight(date_of(real)?, &zone)?, true));
    }
    let time = terms.last_trade_time.as_str();
    if time.len() == 4 && time.bytes().all(|b| b.is_ascii_digit()) {
        // Read as a gateway reads it, leniently: an hour or a minute past its
        // range runs on into the next, so 2400 is the next day's midnight.
        let (hour, minute) = (time[..2].parse::<i64>().ok()?, time[2..].parse::<i64>().ok()?);
        let at = date_of(last_day)?
            .at(0, 0, 0, 0)
            .checked_add(jiff::Span::new().hours(hour).minutes(minute))
            .ok()?
            .to_zoned(zone)
            .ok()?;
        return Some((at.timestamp().as_millisecond(), false));
    }
    let close = schedule
        .liquid_hours
        .iter()
        .rfind(|session| session.trade_date == last_day && session.start != session.end)
        .and_then(|session| crate::protocol::datetime::ib_datetime_to_unix_millis(&session.end));
    match close {
        Some(close) => Some((close, false)),
        None => Some((midnight(date_of(last_day)?, &zone)?, true)),
    }
}

/// A currency's rates as the model keeps them once taken in: each point
/// counted from midnight of the day the model's clock read then, for every
/// option priced in the currency. A date that cannot be read leaves the
/// currency with no rates, which states no rate at all.
fn curve_of(rates: &[(String, f64)], now: i64) -> Vec<RatePoint> {
    model_calendar()
        .and_then(|calendar| {
            let (_, today) = day_of(now, &calendar)?;
            rates
                .iter()
                .map(|(date, percent)| {
                    let at = midnight(date_of(date)?, &calendar)?;
                    Some(RatePoint {
                        years: ((at - today) as f64 / 3.1536e10).max(0.0),
                        rate: crate::options::continuous_rate(*percent),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The payments as the model reads them from a moment on its clock: counted
/// from midnight of that day, and, for whether the tree takes one, in whole
/// days to the end of its ex-date on this machine's own calendar.
fn dividends_from(
    payments: &[(Date, i64, f64)],
    today: Date,
    midnight_today: i64,
    now: i64,
    host: &TimeZone,
) -> Vec<Dividend> {
    payments
        .iter()
        .map(|&(date, ex_date, amount)| {
            let ends =
                date.tomorrow().ok().and_then(|next| midnight(next, host)).unwrap_or(ex_date);
            Dividend {
                ex_day: days_between(today, date).unwrap_or(i32::MIN),
                millis_to_ex_date: ex_date - now,
                millis_from_today: ex_date - midnight_today,
                days_to_end_of_ex_date: (ends - now) / 86_400_000,
                amount,
            }
        })
        .collect()
}

impl ModelInputs {
    /// What a gateway works an option's model from once the moment its time
    /// runs out and its underlying's dividends are in hand: at the clock's
    /// reading now, the rate at its term and the present value of the
    /// dividends its life covers, where the currency's rates are in hand.
    fn read(
        terms: &OptionTerms,
        (expiry, date_only): (i64, bool),
        dividends: &crate::control::dividends::Schedule,
        curve: Option<&[RatePoint]>,
        adjusts_for_tax: bool,
        now: i64,
    ) -> Option<Self> {
        let calendar = model_calendar()?;
        let (today, midnight_today) = day_of(now, &calendar)?;
        let payments: Vec<(Date, i64, f64)> = dividends
            .payments
            .iter()
            .filter_map(|payment| {
                let date = date_of(&payment.ex_date)?;
                Some((date, midnight(date, &calendar)?, payment.amount))
            })
            .collect();
        let tax = if adjusts_for_tax { dividends.tax_adjustment } else { 1.0 };
        let years = crate::options::time_to_expiry(expiry - now, date_only);
        let rate = curve.map_or(f64::NAN, |curve| {
            crate::options::rate_at(curve, crate::options::rate_term(years))
        });
        let pv_dividend = match days_between(today, date_of(&terms.last_trading_day)?) {
            Some(expiry_day) if !rate.is_nan() => crate::options::dividend_present_value(
                &dividends_from(&payments, today, midnight_today, now, &TimeZone::system()),
                expiry_day,
                rate,
                tax,
                false,
            ),
            _ => f64::NAN,
        };
        Some(Self { expiry, date_only, rated: curve.is_some(), rate, pv_dividend, payments, tax })
    }
}

/// A figure as a tick carries it: a figure that is not a number, or not
/// finite, is not stated.
fn as_stated(figures: [f64; 8]) -> [f64; 8] {
    figures.map(|figure| if stated(figure) { figure } else { UNSTATED })
}

impl FarmState {
    /// Whether a series is one an option's own model asks for, and so no
    /// caller's to ask for or to give up.
    pub(super) fn asked_for_the_model(&self, instrument: InstrumentId, series: u32) -> bool {
        OPTION_VOLATILITY_SERIES.contains(&series)
            && self.modelled_options.contains_key(&instrument)
    }

    /// Keep the model's volatilities for an option, where they stand.
    pub(super) fn note_option_volatility(
        &mut self,
        instrument: InstrumentId,
        series: u32,
        payload: &[u8],
    ) {
        let Some(option) = self.modelled_options.get_mut(&instrument) else { return };
        let mut kept = false;
        for (at, volatility) in stated_volatilities(series, payload) {
            if let Some(volatility) = volatility {
                option.vols[at] = Some(volatility);
                kept = true;
            }
        }
        if !kept {
            return;
        }
        option.sides_due = true;
        if series != 736 {
            self.option_ticks_due.insert(instrument);
        }
    }

    /// Note that the venue stated something of an option's quote: what its
    /// bid's, ask's and last's ticks are worked from has moved, and each of
    /// them is put to whoever watches the option again, as a gateway puts
    /// them on every change to the quote.
    pub(super) fn note_option_quote(
        &mut self,
        instrument: InstrumentId,
        present: u8,
        shared: &SharedState,
    ) {
        let Some(option) = self.modelled_options.get_mut(&instrument) else { return };
        let prices = present
            & (crate::bridge::PRICING_BID
                | crate::bridge::PRICING_ASK
                | crate::bridge::PRICING_LAST);
        option.quoted |= prices;
        option.sides_due |= prices != 0;
        for (tick, _) in option.sides.iter().flatten() {
            shared.market.push_option_tick(*tick);
        }
    }

    /// The moment the model's clock reads.
    fn model_clock(&self, now: i64) -> i64 {
        let since = now - self.model_clock_origin;
        self.model_clock_origin + since.div_euclid(MODEL_CLOCK_MILLIS) * MODEL_CLOCK_MILLIS
    }

    /// Build the model tick of every option something new was stated for,
    /// once in each second of the clock, and the bid's, ask's and last's once
    /// in each two; ask for the chain parameters on the underlying of each
    /// option modelled without them, and note what else the model waits on.
    ///
    /// The seconds are this machine's clock's, standing in for the clock a
    /// gateway's model keeps.
    pub(super) fn publish_option_ticks(
        &mut self,
        now: i64,
        context: &crate::engine::context::Context,
        farm_conn: &mut Option<Connection>,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        let second = now.div_euclid(1_000) as u64;
        if second != self.option_ticks_built_in {
            self.option_ticks_built_in = second;
            self.ask_for_underlying_models(farm_conn, shared, hb);
            self.read_model_inputs(now, shared);
            for instrument in std::mem::take(&mut self.option_ticks_due) {
                if let Some(option) = self.modelled_options.get(&instrument) {
                    shared.market.push_option_tick(self.option_tick(instrument, option, shared));
                }
            }
            let pair = now.div_euclid(2_000);
            if pair != self.option_sides_built_in {
                self.option_sides_built_in = pair;
                let mut owed: Vec<InstrumentId> = self
                    .modelled_options
                    .iter_mut()
                    .filter_map(|(instrument, option)| {
                        std::mem::take(&mut option.sides_due).then_some(*instrument)
                    })
                    .collect();
                if !self.option_sides_owed.is_empty() {
                    owed.append(&mut self.option_sides_owed);
                    owed.sort_unstable();
                    owed.dedup();
                }
                self.option_sides_owed = owed;
            }
        }
        self.build_option_sides(now, context, shared);
    }

    /// An option's model tick as a gateway builds it.
    ///
    /// Nothing is stated until the venue's greeks state a delta.
    fn option_tick(
        &self,
        instrument: InstrumentId,
        option: &ModelledOption,
        shared: &SharedState,
    ) -> OptionTick {
        let mut tick = OptionTick {
            instrument,
            kind: OptionTickKind::Model,
            figures: [UNSTATED; 8],
            price_based: false,
        };
        let Some(model) =
            shared.market.option_model(instrument).filter(|model| stated(model.delta))
        else {
            return tick;
        };
        let implied_vol = option.vols[..3]
            .iter()
            .flatten()
            .next()
            .map_or(UNSTATED, |(volatility, _)| volatility * A_YEAR_OF_TRADING_DAYS.sqrt());
        let terms = option.terms.as_ref();
        // A warrant's price is stated per unit and published per contract.
        let opt_price = match terms {
            Some(terms) if option.per_contract => model.opt_price * terms.multiplier,
            _ => model.opt_price,
        };
        let und_price = terms.map_or(UNSTATED, |terms| {
            let sets = chain_of(&self.underlying_models, terms);
            underlying_price(sets, &terms.trading_class, terms.multiplier, &terms.last_trading_day)
        });
        let pv_dividend = option.inputs.as_ref().map_or(UNSTATED, |inputs| inputs.pv_dividend);
        tick.figures = as_stated([
            implied_vol,
            model.delta,
            opt_price,
            pv_dividend,
            model.gamma,
            model.vega,
            model.theta,
            und_price,
        ]);
        tick.price_based = option.vols[1].is_some_and(|(_, attributes)| attributes & 2 != 0);
        tick
    }

    /// Take in what an option's own model is worked from as it comes to hand:
    /// its sessions, its underlying's dividends and its currency's rates. What
    /// is not in hand yet is noted for the security definition connection to
    /// ask for.
    fn read_model_inputs(&mut self, now: i64, shared: &SharedState) {
        let clock = self.model_clock(now);
        let adjusts_for_tax = shared.reference.enables("TAXADJDVD");
        for (instrument, option) in &mut self.modelled_options {
            let Some(terms) = option.terms.as_ref() else { continue };
            // A gateway works the options on a share this way; an index's
            // payments and a future's carry are stated otherwise.
            let (Some(underlying), "STK") = (terms.underlying, terms.under_sec_type.as_str())
            else {
                continue;
            };
            if option.inputs.as_ref().is_some_and(|inputs| inputs.rated) {
                continue;
            }
            if !self.currency_curves.contains_key(&terms.currency) {
                match shared.reference.currency_rates(&terms.currency) {
                    Some(rates) => {
                        self.currency_curves
                            .insert(terms.currency.clone(), curve_of(&rates, clock));
                    }
                    None => {
                        if !self.rates_wanted.contains(&terms.currency) {
                            self.rates_wanted.push(terms.currency.clone());
                        }
                        if option.inputs.is_some() {
                            continue;
                        }
                    }
                }
            }
            let Some(expiry) = shared
                .reference
                .contract_schedule(option.con_id as u32, |schedule| expiry_of(terms, schedule))
            else {
                if !self.schedules_wanted.contains(&(option.con_id as u32)) {
                    self.schedules_wanted.push(option.con_id as u32);
                }
                continue;
            };
            let Some(dividends) = shared.reference.dividend_schedule(underlying as u32) else {
                continue;
            };
            option.inputs = expiry.and_then(|expiry| {
                ModelInputs::read(
                    terms,
                    expiry,
                    &dividends,
                    self.currency_curves.get(&terms.currency).map(Vec::as_slice),
                    adjusts_for_tax,
                    clock,
                )
            });
            option.sides_due = true;
            self.option_ticks_due.insert(*instrument);
        }
    }

    /// Build the bid's, ask's and last's ticks of every option something new
    /// was stated for, and put each whose volatility moved to whoever watches
    /// the option.
    ///
    /// Built as a gateway builds them: at the volatility the venue states for
    /// the side, over a year of trading days, with the side's price, where
    /// the quote states that side, narrowed as a gateway narrows the prices
    /// its model takes in, the underlying's price the chain parameters state,
    /// and greeks worked out by the model at that volatility. A side's greeks
    /// are replaced only by four finite ones. Nothing is built until the
    /// option's time, its underlying's dividends and the underlying's price
    /// are in hand.
    fn build_option_sides(
        &mut self,
        now: i64,
        context: &crate::engine::context::Context,
        shared: &SharedState,
    ) {
        if self.option_sides_owed.is_empty() {
            return;
        }
        // ponytail: a bounded batch on each turn of the engine's loop, which
        // serves orders too; a thread of the model's own if that ever falls
        // behind.
        let batch = self.option_sides_owed.len().min(OPTIONS_BUILT_PER_TURN);
        let owed: Vec<InstrumentId> = self.option_sides_owed.drain(..batch).collect();
        let clock = self.model_clock(now);
        let Some((today, midnight_today)) = model_calendar().and_then(|c| day_of(clock, &c)) else {
            return;
        };
        let host = TimeZone::system();
        for instrument in &owed {
            let Some(option) = self.modelled_options.get_mut(instrument) else { continue };
            let (Some(terms), Some(inputs)) = (option.terms.as_ref(), option.inputs.as_ref())
            else {
                continue;
            };
            let sets = chain_of(&self.underlying_models, terms);
            let spot = modelled_underlying_price(
                sets,
                &terms.trading_class,
                terms.multiplier,
                &terms.last_trading_day,
            );
            if !stated(spot) {
                continue;
            }
            let price_based = option.vols.iter().flatten().any(|(_, a)| a & 2 != 0)
                || sets.iter().any(|set| set.attributes & 2 != 0);
            let dividends = dividends_from(&inputs.payments, today, midnight_today, clock, &host);
            let years = crate::options::time_to_expiry(inputs.expiry - clock, inputs.date_only);
            let quote = context.quote(*instrument);
            // A side the quote states is taken in where it stands, as a
            // gateway's model takes it: the bid and the ask with a size, the
            // last where it or its size is above nought. A frozen record
            // states a side it has nothing on as a price with no size, -1 for
            // the bid and the ask and nought for the last.
            let price = |flag: u8, stands: bool, price: i64| {
                if option.quoted & flag == 0 || !stands {
                    return f64::NAN;
                }
                // As a gateway's model takes the quote in: to single precision.
                (price as f64 / crate::types::model::PRICE_SCALE_F) as f32 as f64
            };
            let prices = [
                price(crate::bridge::PRICING_BID, quote.bid_size != 0, quote.bid),
                price(crate::bridge::PRICING_ASK, quote.ask_size != 0, quote.ask),
                price(
                    crate::bridge::PRICING_LAST,
                    quote.last > 0 || quote.last_size > 0,
                    quote.last,
                ),
            ];
            // Never sent as worked from prices: a gateway sends a side with
            // the attribute of the last of that side it sent the request,
            // which starts unset and so stays.
            let mut built = option.sides.unwrap_or_else(|| {
                SIDES.map(|(_, kind)| {
                    let tick = OptionTick {
                        instrument: *instrument,
                        kind,
                        figures: [UNSTATED; 8],
                        price_based: false,
                    };
                    (tick, None)
                })
            });
            for (side, &(at, _)) in SIDES.iter().enumerate() {
                let (tick, was) = &mut built[side];
                let volatility = option.vols[at];
                let annual = volatility.map(|(v, _)| v * A_YEAR_OF_TRADING_DAYS.sqrt());
                let greeks = annual.and_then(|volatility| {
                    let input = crate::options::model::Inputs {
                        is_call: terms.call,
                        american: terms.american,
                        price_based,
                        spot,
                        strike: terms.strike,
                        years,
                        rate: inputs.rate,
                        yield_rate: 0.0,
                        volatility,
                        dividend_pv: inputs.pv_dividend,
                        dividends: &dividends,
                        index_dividends: false,
                        tax_adjustment: inputs.tax,
                        forward: f64::NAN,
                        futures_style: false,
                        quanto: false,
                    };
                    // A side with no price is taken in as a price that is not
                    // a number, as a gateway's model takes it: a falling
                    // theta is then held to a time value that cannot be
                    // worked out, and the side keeps the greeks it had.
                    let value = crate::options::model::calculate(&input, Some(prices[side]));
                    let greeks = [value.delta, value.gamma, value.vega, value.theta];
                    greeks.iter().all(|g| g.is_finite()).then_some(greeks)
                });
                let [delta, gamma, vega, theta] = greeks.unwrap_or([
                    tick.figures[1],
                    tick.figures[4],
                    tick.figures[5],
                    tick.figures[6],
                ]);
                let per_contract = if option.per_contract { terms.multiplier } else { 1.0 };
                tick.figures = as_stated([
                    annual.unwrap_or(UNSTATED),
                    delta,
                    prices[side] * per_contract,
                    inputs.pv_dividend,
                    gamma,
                    vega,
                    theta,
                    spot,
                ]);
                // Put to its watchers where its volatility moved; otherwise it
                // waits for the next change to the quote.
                if *was != volatility {
                    *was = volatility;
                    shared.market.push_option_tick(*tick);
                }
            }
            option.sides = Some(built);
        }
    }

    /// Read what each option's definition states once it is in hand, and ask
    /// for the chain parameters on the underlying it names: one subscription
    /// per underlying, on the model's name, on each connection.
    fn ask_for_underlying_models(
        &mut self,
        farm_conn: &mut Option<Connection>,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        // Read once. An option whose definition is not in hand is looked for
        // again each second, which costs one lookup and copies nothing.
        for (instrument, option) in &mut self.modelled_options {
            if option.terms.is_some() {
                continue;
            }
            let Some(definition) = shared.reference.contract_definition(option.con_id as u32, "")
            else {
                continue;
            };
            let underlying = (definition.under_con_id != 0
                && !definition.under_sec_type.is_empty())
            .then_some(i64::from(definition.under_con_id));
            // Stated on the definition under numbers this client does not
            // otherwise name: how it may be exercised (1 before expiry) and
            // the time of day it last trades.
            let unnamed = |tag: u32| {
                definition.unnamed_fields.iter().find(|(at, _)| *at == tag).map(|(_, v)| v.as_str())
            };
            option.terms = Some(OptionTerms {
                multiplier: definition.multiplier,
                last_trading_day: definition
                    .last_trade_date
                    .get(..8)
                    .unwrap_or_default()
                    .to_string(),
                trading_class: definition.trading_class.clone(),
                underlying,
                under_sec_type: definition.under_sec_type.clone(),
                strike: definition.strike,
                call: definition.right == Some(crate::control::contracts::OptionRight::Call),
                american: unnamed(6659) == Some("1"),
                currency: definition.currency.clone(),
                last_trade_time: unnamed(6850).unwrap_or_default().to_string(),
                real_expiration: definition.real_expiration_date.clone(),
            });
            let Some(underlying) = underlying else { continue };
            match self.underlying_models.iter_mut().find(|held| held.con_id == underlying) {
                Some(held) => held.options.push(*instrument),
                None => self.underlying_models.push(UnderlyingModel {
                    con_id: underlying,
                    sec_type: crate::control::contracts::sec_type_to_fix(
                        &definition.under_sec_type,
                    )
                    .to_string(),
                    req_id: None,
                    server_tag: None,
                    options: vec![*instrument],
                    sets: Vec::new(),
                }),
            }
        }
        let Some(conn) = farm_conn.as_mut() else { return };
        for held in self.underlying_models.iter_mut().filter(|held| held.req_id.is_none()) {
            let req_id = self.next_md_req_id;
            self.next_md_req_id += 1;
            // The standing set, and not the set as the chain closed.
            let tags = build_model_subscribe_tags(
                req_id,
                held.con_id,
                &held.sec_type,
                CHAIN_MODEL_SERIES[0],
                &chrono_free_timestamp(),
            );
            let refs: Vec<(u32, &str)> =
                tags.iter().map(|(tag, value)| (*tag, value.as_str())).collect();
            let _ = conn.send_fixcomp(&refs);
            hb.last_farm_sent = Instant::now();
            held.req_id = Some(req_id);
        }
    }

    /// Keep what the chain parameters on an underlying state, for the options
    /// modelled on it.
    pub(super) fn note_underlying_model(&mut self, server_tag: u32, payload: &[u8]) {
        let Some(held) =
            self.underlying_models.iter_mut().find(|held| held.server_tag == Some(server_tag))
        else {
            return;
        };
        let Some(sets) = crate::protocol::chain_model::parse(payload) else {
            log::warn!(
                "the chain parameters on {} could not be read; what was last stated stands",
                held.con_id,
            );
            return;
        };
        held.sets = sets;
        for instrument in &held.options {
            self.option_ticks_due.insert(*instrument);
            if let Some(option) = self.modelled_options.get_mut(instrument) {
                option.sides_due = true;
            }
        }
    }

    /// Stop modelling an option from its underlying's chain parameters, and
    /// withdraw them with the last option modelled on them.
    pub(super) fn release_underlying_model(
        &mut self,
        instrument: InstrumentId,
        underlying: i64,
        farm_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        let Some(at) = self.underlying_models.iter().position(|held| held.con_id == underlying)
        else {
            return;
        };
        self.underlying_models[at].options.retain(|held| *held != instrument);
        if !self.underlying_models[at].options.is_empty() {
            return;
        }
        let held = self.underlying_models.remove(at);
        let (Some(conn), Some(req_id)) = (farm_conn.as_mut(), held.req_id) else { return };
        let req_id = req_id.to_string();
        let con_id = (held.con_id as u32).to_string();
        let series = CHAIN_MODEL_SERIES[0].to_string();
        // Withdrawn the way it was asked for, as every entry of a
        // subscription is.
        let _ = conn.send_fixcomp(&[
            (fix::TAG_MSG_TYPE, fix::MSG_MARKET_DATA_REQ),
            (263, "2"),
            (146, "1"),
            (262, &req_id),
            (6008, &con_id),
            (207, GREEKS_VENUE),
            (167, &held.sec_type),
            (264, &series),
            (6088, "Socket"),
            (9830, "1"),
            (9839, "1"),
        ]);
        hb.last_farm_sent = Instant::now();
    }
}
