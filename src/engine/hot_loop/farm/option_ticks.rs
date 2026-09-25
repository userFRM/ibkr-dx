//! What a gateway's option model publishes for an option on its own tick.
//!
//! The venue states the greeks and its model's price for an option on one
//! series, and the volatilities they are worked from on others. A gateway
//! publishes the greeks and the price as the venue states them, the first of
//! the model's, the mid's and the last's volatility that stands, over a year of
//! trading days, and the underlying's price the chain parameters on the
//! underlying state for the option's expiry. It rebuilds that at most once a
//! second, on the clock's own seconds, for the options something new was
//! stated for.

use super::{
    A_YEAR_OF_TRADING_DAYS, CHAIN_MODEL_SERIES, Connection, FarmState, GREEKS_VENUE,
    HeartbeatState, InstrumentId, ModelledOption, OPTION_VOLATILITY_SERIES, OptionTerms,
    SharedState, UnderlyingModel, build_model_subscribe_tags, chrono_free_timestamp, fix,
    series_f64, series_i32,
};
use crate::bridge::OptionTick;
use crate::protocol::chain_model::ChainModelParameters;
use std::time::Instant;

/// A figure not stated.
const UNSTATED: f64 = f64::MAX;

fn stated(figure: f64) -> bool {
    figure != UNSTATED && figure.is_finite()
}

/// A volatility one of the model's series states: its attributes, then the
/// figure per trading day. `None` where it does not stand — attributes that do
/// not mark it valid, or a figure not above nought or not finite — which
/// leaves the one before it standing.
fn stated_volatility(payload: &[u8]) -> Option<(f64, i32)> {
    let attributes = series_i32(payload, 0)?;
    let volatility = series_f64(payload, 4)?;
    (attributes > 0 && attributes & 1 != 0 && volatility > 0.0 && volatility.is_finite())
        .then_some((volatility, attributes))
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
            set.classes.iter().any(|(named, _)| named == class)
                && set
                    .classes
                    .iter()
                    .flat_map(|(_, at)| at)
                    .any(|m| m.to_bits() == multiplier.to_bits())
        })
        .filter(|set| set.terms.iter().any(|term| term.last_trade_date == last_trading_day))
        .and_then(|set| set.underlying_price)
        .unwrap_or(UNSTATED)
}

impl FarmState {
    /// Whether a series is one an option's own model asks for, and so no
    /// caller's to ask for or to give up.
    pub(super) fn asked_for_the_model(&self, instrument: InstrumentId, series: u32) -> bool {
        OPTION_VOLATILITY_SERIES.contains(&series)
            && self.modelled_options.contains_key(&instrument)
    }

    /// Keep one of the model's volatilities for an option, where it stands.
    pub(super) fn note_option_volatility(
        &mut self,
        instrument: InstrumentId,
        series: u32,
        payload: &[u8],
    ) {
        let Some(option) = self.modelled_options.get_mut(&instrument) else { return };
        let Some(volatility) = stated_volatility(payload) else { return };
        let at = match series {
            734 => 0,
            735 => 1,
            _ => 2,
        };
        option.vols[at] = Some(volatility);
        self.option_ticks_due.insert(instrument);
    }

    /// Build the model tick of every option something new was stated for,
    /// once in each second of the clock, and ask for the chain parameters on
    /// the underlying of each option modelled without them.
    ///
    /// The seconds are this machine's clock's, standing in for the clock a
    /// gateway's model keeps.
    pub(super) fn publish_option_ticks(
        &mut self,
        second: u64,
        farm_conn: &mut Option<Connection>,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        if second == self.option_ticks_built_in {
            return;
        }
        self.option_ticks_built_in = second;
        self.ask_for_underlying_models(farm_conn, shared, hb);
        for instrument in std::mem::take(&mut self.option_ticks_due) {
            if let Some(option) = self.modelled_options.get(&instrument) {
                shared.market.push_option_tick(self.option_tick(instrument, option, shared));
            }
        }
    }

    /// An option's model tick as a gateway builds it.
    ///
    /// Nothing is stated until the venue's greeks state a delta. The present
    /// value of dividends is not stated.
    fn option_tick(
        &self,
        instrument: InstrumentId,
        option: &ModelledOption,
        shared: &SharedState,
    ) -> OptionTick {
        let mut tick = OptionTick { instrument, figures: [UNSTATED; 8], price_based: false };
        let Some(model) =
            shared.market.option_model(instrument).filter(|model| stated(model.delta))
        else {
            return tick;
        };
        let implied_vol = option
            .vols
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
        let und_price = terms
            .and_then(|terms| {
                let held = self
                    .underlying_models
                    .iter()
                    .find(|held| Some(held.con_id) == terms.underlying)?;
                Some(underlying_price(
                    &held.sets,
                    &terms.trading_class,
                    terms.multiplier,
                    &terms.last_trading_day,
                ))
            })
            .unwrap_or(UNSTATED);
        tick.figures = [
            implied_vol,
            model.delta,
            opt_price,
            UNSTATED,
            model.gamma,
            model.vega,
            model.theta,
            und_price,
        ]
        .map(|figure| if stated(figure) { figure } else { UNSTATED });
        tick.price_based = option.vols[1].is_some_and(|(_, attributes)| attributes & 2 != 0);
        tick
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
            option.terms = Some(OptionTerms {
                multiplier: definition.multiplier,
                last_trading_day: definition
                    .last_trade_date
                    .get(..8)
                    .unwrap_or_default()
                    .to_string(),
                trading_class: definition.trading_class,
                underlying,
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
        self.option_ticks_due.extend(held.options.iter().copied());
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
