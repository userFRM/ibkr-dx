use super::attached_checks::UNSET_ID;
use super::attached_prices::{
    OrderPrice, PriceContext, PriceSide, UNSET_PRICE, resolve_adjusted_price,
    resolve_attached_price, resolve_profit_price,
};
use crate::control::attached_presets::{AttachedPreset, PriceUnit};
use crate::types::model::Order;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChildPrices {
    pub ord_type: String,
    pub limit: f64,
    pub stop: f64,
    pub offset: f64,
    pub trailing_amount: f64,
    pub trailing_unit: PriceUnit,
    pub adjusted_type: Option<String>,
    pub adjusted_trigger: f64,
    pub adjusted_stop: f64,
    pub adjusted_limit: f64,
    pub adjusted_trailing_amount: f64,
    pub adjusted_trailing_unit: PriceUnit,
    pub non_amount_units_allowed: bool,
}

impl ChildPrices {
    pub(crate) fn wire_order_type(&self, trail_as_t: bool) -> &str {
        match self.ord_type.as_str() {
            "E" | "PEGMKT" | "PEGMID" => "P",
            "PEGBEST" => "E2M",
            "SNAPMKT" => "SMKT",
            "SNAPMID" => "SMID",
            "RM" | "LM" => "J",
            "RL" => "2",
            "TRM" | "TLM" => "TMIT",
            "T" | "TRAIL" if !trail_as_t => "P",
            "TRAIL" => "T",
            other => other,
        }
    }

    pub(crate) fn exec_inst(&self, trail_as_t: bool) -> &str {
        match self.ord_type.as_str() {
            "E" | "PB" | "SREL" => "R",
            "PEGMKT" | "SNAPMKT" => "P",
            "PEGMID" | "SNAPMID" => "M",
            "F" => "s",
            "T" | "TRAIL" if !trail_as_t => "a",
            _ => "",
        }
    }

    fn from_parent(
        parent: &Order,
        pricing: OrderPrice,
        ord_type: &str,
        non_amount_units_allowed: bool,
    ) -> Self {
        Self {
            ord_type: ord_type.into(),
            limit: pricing.limit,
            stop: pricing.stop,
            offset: parent.aux_price,
            trailing_amount: if matches!(parent.order_type_named(), Some("TRAIL" | "TRAIL LIMIT")) {
                if parent.trailing_percent != 0.0 && parent.trailing_percent != UNSET_PRICE {
                    parent.trailing_percent
                } else {
                    parent.aux_price
                }
            } else {
                UNSET_PRICE
            },
            trailing_unit: if parent.trailing_percent != 0.0
                && parent.trailing_percent != UNSET_PRICE
            {
                PriceUnit::Percent
            } else {
                PriceUnit::Amount
            },
            adjusted_type: match parent.adjusted_order_type.as_str() {
                "STP" => Some("3".into()),
                "STP LMT" => Some("4".into()),
                "TRAIL" => Some("T".into()),
                "TRAIL LIMIT" => Some("TSL".into()),
                _ => None,
            },
            adjusted_trigger: parent.trigger_price,
            adjusted_stop: parent.adjusted_stop_price,
            adjusted_limit: parent.adjusted_stop_limit_price,
            adjusted_trailing_amount: parent.adjusted_trailing_amount,
            non_amount_units_allowed,
            adjusted_trailing_unit: match parent.adjustable_trailing_unit {
                0 => PriceUnit::Amount,
                1 => PriceUnit::Ticks,
                100 => PriceUnit::Percent,
                _ => PriceUnit::Unavailable,
            },
        }
    }

    /// Price fields preserve unset values as absence and absolute trailing limits.
    pub(crate) fn wire_fields(&self) -> Vec<(u32, String)> {
        let mut fields = Vec::new();
        let kind = api_type(&self.ord_type);
        if uses_limit(kind) {
            push_price(&mut fields, 44, self.limit);
        }
        if matches!(kind, "TRAIL" | "TRAIL LIMIT") {
            push_price(&mut fields, 99, self.trailing_amount);
            push_price(&mut fields, 211, self.trailing_amount);
            if self.trailing_unit == PriceUnit::Amount || self.non_amount_units_allowed {
                fields.push((6268, unit_id(self.trailing_unit).to_string()));
            }
            push_price(&mut fields, 6117, self.stop);
        } else if uses_stop(kind) || matches!(kind, "MIT" | "LIT") {
            push_price(&mut fields, 99, self.stop);
        } else if matches!(kind, "REL" | "RPI" | "PASSV REL" | "PEG MKT" | "PEG MID") {
            push_price(&mut fields, 99, self.offset);
            push_price(&mut fields, 211, self.offset);
        }
        if let Some(adjusted) = &self.adjusted_type {
            fields.push((6257, "1".into()));
            fields.push((6261, adjusted.clone()));
            push_price(&mut fields, 6258, self.adjusted_trigger);
            push_price(&mut fields, 6259, self.adjusted_stop);
            push_price(&mut fields, 6262, self.adjusted_limit);
            push_price(&mut fields, 6260, self.adjusted_trailing_amount);
            if matches!(adjusted.as_str(), "T" | "TSL")
                && (self.adjusted_trailing_unit == PriceUnit::Amount
                    || self.non_amount_units_allowed)
            {
                fields.push((6269, unit_id(self.adjusted_trailing_unit).to_string()));
            }
        }
        fields
    }
}

fn push_price(fields: &mut Vec<(u32, String)>, tag: u32, price: f64) {
    if price != UNSET_PRICE {
        fields.push((tag, wire_number(price)));
    }
}

/// A number as an order field writes one: `#0.00######`, with that
/// formatter's symbols for what is not a number.
pub(crate) fn wire_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value.is_sign_negative() { "-∞".into() } else { "∞".into() }
    } else {
        let mut written = format!("{value:.8}");
        let minimum = written.len() - 6;
        while written.len() > minimum && written.ends_with('0') {
            written.pop();
        }
        written
    }
}

pub(crate) fn unit_id(unit: PriceUnit) -> i32 {
    match unit {
        PriceUnit::Amount => 0,
        PriceUnit::Ticks => 1,
        PriceUnit::Percent => 100,
        PriceUnit::Unavailable => 99,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AttachedChild {
    pub order: Order,
    pub prices: ChildPrices,
    pub use_parent_trade_price: bool,
    pub profit_offset: Option<f64>,
    pub tif_override: Option<u8>,
}

pub(crate) struct ChildContext<'a> {
    pub parent_api_id: i64,
    pub parent_price: OrderPrice,
    pub prices: PriceContext<'a>,
    /// The numeric part of the parent's venue order identifier.
    pub oca_group: &'a str,
    pub parent_is_scale_or_scale_child: bool,
    /// The execution-derived price, when the parent can no longer supply its order price.
    pub parent_trade_price: Option<f64>,
    pub parent_finished: bool,
    pub exchange: &'a str,
    pub supports_order_type: &'a dyn Fn(&str) -> bool,
    pub defaults: ChildDefaults,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChildDefaults {
    pub regular_hours: bool,
    pub late_hours: bool,
    pub market_orders_regular_only: bool,
    pub is_combo: bool,
    pub combo_regular_hours: bool,
    pub cancel_parent_enabled: bool,
    pub opening_at_arca: bool,
    pub percent_prices_restricted: bool,
}

pub(crate) fn market_orders_regular_only(
    sec_type: &str,
    currency: &str,
    classification: &str,
    underlying_sec_type: &str,
    underlying_classification: &str,
) -> bool {
    (sec_type == "STK" && classification == "USSTK")
        || (sec_type == "WAR" && classification == "USWAR")
        || (sec_type == "OPT"
            && underlying_sec_type == "STK"
            && underlying_classification == "USSTK")
        || (matches!(sec_type, "OPT" | "FOP" | "IOPT") && currency == "USD")
}

pub(crate) fn is_scale_order(order: &Order) -> bool {
    matches!(
        order.order_type_named(),
        Some(
            "LMT"
                | "REL + LMT"
                | "REL"
                | "PASSV REL"
                | "RPI"
                | "PEG MKT"
                | "PEG MID"
                | "PEG BEST"
                | "MIT"
                | "REL + MKT"
                | "LMT + MKT"
        )
    ) && (order.scale_init_level_size != i32::MAX || order.scale_subs_level_size != i32::MAX)
}

fn apply_child_defaults(child: &mut Order, preset: &AttachedPreset, context: &ChildContext<'_>) {
    let defaults = context.defaults;
    if preset.primary_use_price_mgmt_algo {
        child.use_price_mgmt_algo = Some(1);
    }
    if preset.primary_auto_cancel_parent && defaults.cancel_parent_enabled {
        child.auto_cancel_parent = true;
    }
    if !preset.primary_outside_rth || child.outside_rth || !defaults.late_hours {
        return;
    }
    let kind = child.order_type.as_str();
    if !matches!(
        kind,
        "STP"
            | "STP LMT"
            | "STP PRT"
            | "TRAIL"
            | "TRAIL LIMIT"
            | "MIT"
            | "LIT"
            | "TRAIL MIT"
            | "TRAIL LIT"
            | "REL + MKT"
            | "LMT + MKT"
            | "TRAIL REL + MKT"
            | "TRAIL LMT + MKT"
    ) {
        return;
    }
    let regular_session = !matches!(child.tif.as_str(), "?" | "FOK" | "IOC")
        && (child.tif != "OPG" || defaults.opening_at_arca);
    if defaults.regular_hours && regular_session {
        return;
    }
    let market_type = matches!(kind, "STP" | "STP PRT" | "TRAIL" | "MIT" | "TRAIL MIT");
    let restricted = if defaults.is_combo {
        defaults.combo_regular_hours
            && !matches!(kind, "STP" | "STP LMT" | "STP PRT" | "TRAIL" | "TRAIL LIMIT")
    } else {
        defaults.market_orders_regular_only
    };
    if !market_type || !restricted {
        child.outside_rth = true;
    }
}

/// Construct stop loss before profit taker, preserving the parent's transmission choice.
pub(crate) fn build_children(
    parent: &Order,
    preset: &AttachedPreset,
    context: &ChildContext<'_>,
) -> Vec<AttachedChild> {
    let mut children = Vec::with_capacity(2);
    if !parent.sl_order_type.is_empty()
        && let Some(kind) = stop_kind(preset)
        && let Some(child) = build_stop(parent, preset, context, kind)
    {
        children.push(child);
    }
    if !parent.pt_order_type.is_empty() && preset.profit_order_type != "-1" {
        children.push(build_profit(parent, preset, context));
    }
    if children.len() > 1 && !(context.supports_order_type)("OCA") {
        children.clear();
    }
    children
}

fn clone_child(parent: &Order, api_id: i64, context: &ChildContext<'_>) -> Order {
    let mut child = parent.clone();
    child.order_id = if api_id == UNSET_ID { 0 } else { api_id };
    child.parent_id = context.parent_api_id;
    child.action = match parent.side() {
        Ok(crate::types::Side::Buy) => "SELL".into(),
        Ok(crate::types::Side::Sell | crate::types::Side::ShortSell) => "BUY".into(),
        Err(_) => parent.action.clone(),
    };
    child.perm_id = 0;
    child.parent_perm_id = 0;
    child.filled_quantity = 0.0;
    child.manual_order_time.clear();
    child.discretionary_amt = 0.0;
    child.all_or_none = false;
    child.conditions.clear();
    child.conditions_ignore_rth = false;
    child.conditions_cancel_order = false;
    child.conditions_include_overnight = false;
    child.oca_group = context.oca_group.into();
    child.oca_type = if parent.oca_type == 0 { 3 } else { parent.oca_type };
    child.sl_order_id = UNSET_ID;
    child.pt_order_id = UNSET_ID;
    child.sl_order_type.clear();
    child.pt_order_type.clear();
    child
}

fn clear_scale(child: &mut Order) {
    child.scale_init_level_size = i32::MAX;
    child.scale_subs_level_size = i32::MAX;
    child.scale_price_increment = UNSET_PRICE;
    child.scale_profit_offset = UNSET_PRICE;
    child.scale_price_adjust_interval = i32::MAX;
    child.scale_price_adjust_value = UNSET_PRICE;
    child.scale_init_position = i32::MAX;
    child.scale_init_fill_qty = i32::MAX;
    child.scale_auto_reset = false;
    child.scale_random_percent = false;
    child.scale_table.clear();
}

fn stop_kind(preset: &AttachedPreset) -> Option<&'static str> {
    match preset.auto_stop {
        1 | 5 | 7 => Some("3"),
        4 => Some("4"),
        2 => Some("T"),
        11 => Some("TSL"),
        9 => Some(adjustable_kind(preset.stop_limit.price_type, preset.trailing_amount)),
        _ => None,
    }
}

fn adjustable_kind(limit_type: i32, trail: f64) -> &'static str {
    let trailing = trail != UNSET_PRICE && trail != 0.0;
    match (limit_type != 3, trailing) {
        (false, false) => "3",
        (true, false) => "4",
        (false, true) => "T",
        (true, true) => "TSL",
    }
}

fn build_stop(
    parent: &Order,
    preset: &AttachedPreset,
    context: &ChildContext<'_>,
    kind: &str,
) -> Option<AttachedChild> {
    let mut child = clone_child(parent, parent.sl_order_id, context);
    let kind = if (context.supports_order_type)(api_type(kind)) { kind } else { "-3" };
    child.order_type = api_type(kind).into();
    let mut prices = ChildPrices::from_parent(
        parent,
        context.parent_price,
        kind,
        !context.defaults.percent_prices_restricted,
    );
    let trailing = matches!(kind, "T" | "TSL");
    if trailing {
        (prices.trailing_amount, prices.trailing_unit) =
            if preset.trailing_amount == UNSET_PRICE || preset.trailing_amount == 0.0 {
                (preset.primary_trailing_amount, preset.primary_trailing_unit)
            } else {
                (preset.trailing_amount, preset.trailing_unit)
            };
    }
    let configured_trail = matches!(preset.auto_stop, 2 | 9 | 11);
    let trailing_unit = if trailing { prices.trailing_unit } else { preset.trailing_unit };
    if configured_trail
        && trailing_unit == PriceUnit::Percent
        && context.defaults.percent_prices_restricted
    {
        return None;
    }
    let mut pricing = price_order(&child, &prices);
    if uses_stop(api_type(kind)) {
        prices.stop =
            resolve_attached_price(&preset.stop, pricing, context.parent_price, &context.prices);
        pricing.stop = prices.stop;
    }
    if uses_limit(api_type(kind)) {
        prices.limit = resolve_attached_price(
            &preset.stop_limit,
            pricing,
            context.parent_price,
            &context.prices,
        );
    }
    if preset.auto_stop == 9 {
        let adjusted =
            adjustable_kind(preset.adjusted_stop_limit.price_type, preset.adjusted_trailing_amount);
        prices.adjusted_type = Some(if (context.supports_order_type)(api_type(adjusted)) {
            adjusted.into()
        } else {
            "-3".into()
        });
        pricing = price_order(&child, &prices);
        prices.adjusted_trigger = resolve_adjusted_price(
            &preset.adjusted_trigger,
            pricing,
            context.parent_price,
            &context.prices,
        );
        prices.adjusted_stop = resolve_adjusted_price(
            &preset.adjusted_stop,
            pricing,
            context.parent_price,
            &context.prices,
        );
        prices.adjusted_limit = resolve_adjusted_price(
            &preset.adjusted_stop_limit,
            pricing,
            context.parent_price,
            &context.prices,
        );
        prices.adjusted_trailing_amount = preset.adjusted_trailing_amount;
        prices.adjusted_trailing_unit = preset.adjusted_trailing_unit;
    }
    let use_parent_trade_price =
        preset.stop.use_parent_trade_price && eligible_parent_price(&child, &prices);
    let profit_offset = use_parent_trade_price
        .then(|| saved_parent_offset(parent, &child, &prices, context))
        .flatten();
    if context.parent_is_scale_or_scale_child {
        clear_scale(&mut child);
    }
    apply_prices(&mut child, &prices);
    let tif_override = (parent.tif == "OPG").then_some(b'?');
    if tif_override.is_some() {
        child.tif = "?".into();
    }
    apply_child_defaults(&mut child, preset, context);
    Some(AttachedChild {
        order: child,
        prices,
        use_parent_trade_price,
        profit_offset,
        tif_override,
    })
}

fn build_profit(
    parent: &Order,
    preset: &AttachedPreset,
    context: &ChildContext<'_>,
) -> AttachedChild {
    let mut child = clone_child(parent, parent.pt_order_id, context);
    let scaled = !matches!(preset.scale_initial_size, 0 | i32::MIN | i32::MAX)
        && !matches!(preset.scale_price_increment, 0.0 | f64::MAX)
        && preset.scale_price_increment != f64::from_bits(1);
    let selected_kind = preset_kind(&preset.profit_order_type);
    let relative = matches!(selected_kind, "E" | "RPI");
    let kind = if scaled { if relative { "E" } else { "2" } } else { selected_kind };
    child.order_type = api_type(kind).into();
    child.total_quantity = if !scaled && preset.profit_uses_exact_parent_quantity {
        parent.total_quantity
    } else {
        let quantity = parent.total_quantity;
        let rounded = if quantity.fract() == -0.5 { quantity.ceil() } else { quantity.round() };
        f64::from(rounded as i64 as i32)
    };
    let mut prices = ChildPrices::from_parent(
        parent,
        context.parent_price,
        kind,
        !context.defaults.percent_prices_restricted,
    );
    prices.limit = resolve_profit_price(&preset.limit, context.parent_price, &context.prices);
    prices.offset = if relative { preset.profit_offset } else { UNSET_PRICE };
    clear_scale(&mut child);
    if scaled {
        child.scale_init_level_size = preset.scale_initial_size;
        child.scale_subs_level_size = preset.scale_subsequent_size;
        child.scale_price_increment = preset.scale_price_increment;
    }
    let use_parent_trade_price =
        !scaled && !context.parent_is_scale_or_scale_child && preset.limit.use_parent_trade_price;
    let profit_offset = use_parent_trade_price
        .then(|| saved_parent_offset(parent, &child, &prices, context))
        .flatten();
    apply_prices(&mut child, &prices);
    apply_child_defaults(&mut child, preset, context);
    AttachedChild {
        order: child,
        prices,
        use_parent_trade_price,
        profit_offset,
        tif_override: None,
    }
}

fn apply_prices(order: &mut Order, prices: &ChildPrices) {
    order.lmt_price = prices.limit;
    order.aux_price = if matches!(order.order_type.as_str(), "TRAIL" | "TRAIL LIMIT") {
        prices.trailing_amount
    } else if uses_stop(&order.order_type) {
        prices.stop
    } else {
        prices.offset
    };
    order.trail_stop_price = prices.stop;
    order.trailing_percent = if prices.trailing_unit == PriceUnit::Percent {
        prices.trailing_amount
    } else {
        UNSET_PRICE
    };
    if let Some(kind) = &prices.adjusted_type {
        order.adjusted_order_type = api_type(kind).into();
        order.trigger_price = prices.adjusted_trigger;
        order.adjusted_stop_price = prices.adjusted_stop;
        order.adjusted_stop_limit_price = prices.adjusted_limit;
        order.adjusted_trailing_amount = prices.adjusted_trailing_amount;
        order.adjustable_trailing_unit = unit_id(prices.adjusted_trailing_unit);
    }
}

pub(crate) fn api_type(kind: &str) -> &str {
    match kind {
        "E" => "REL",
        "PEGMKT" => "PEG MKT",
        "PEGMID" => "PEG MID",
        "PEGBEST" => "PEG BEST",
        "SNAPMKT" => "SNAP MKT",
        "SNAPMID" => "SNAP MID",
        "RL" => "REL + LMT",
        "RM" => "REL + MKT",
        "LM" => "LMT + MKT",
        other => crate::types::orders::ord_type_api_name(other, ""),
    }
}

fn preset_kind(kind: &str) -> &str {
    match kind {
        "-3" | "-2" | "-1" | "0" | "1" | "2" | "3" | "4" | "5" | "A" | "B" | "E" | "F" | "I"
        | "J" | "K" | "P" | "Q" | "T" | "U" | "SP" | "LT" | "TSL" | "MIDPX" | "RPI" | "PSVR"
        | "PB" | "PEGMKT" | "PEGMID" | "PEGBEST" | "SNAPMKT" | "SNAPMID" | "SREL" | "RM" | "LM"
        | "RL" | "TRM" | "TLM" | "TMIT" | "TLIT" | "PPV" | "PDV" | "PMV" | "PSV" | "IBALGO" => kind,
        "SMKT" => "SNAPMKT",
        "SMID" => "SNAPMID",
        "E2M" => "PEGBEST",
        "PMID2" => "PEGMID",
        _ => "2",
    }
}

fn uses_limit(kind: &str) -> bool {
    matches!(
        kind,
        "LMT"
            | "STP LMT"
            | "TRAIL LIMIT"
            | "LOC"
            | "LIT"
            | "REL"
            | "PASSV REL"
            | "PEG MKT"
            | "PEG MID"
            | "PEG BEST"
            | "PEG BENCH"
            | "MIDPRICE"
            | "SNAP MID"
            | "SNAP MKT"
            | "SNAP PRIM"
            | "RPI"
            | "REL + LMT"
    )
}

fn uses_stop(kind: &str) -> bool {
    matches!(kind, "STP" | "STP LMT" | "TRAIL LIMIT" | "STP PRT")
}

fn price_order(order: &Order, prices: &ChildPrices) -> OrderPrice {
    OrderPrice {
        side: if order.action == "BUY" { PriceSide::Buy } else { PriceSide::Sell },
        limit: prices.limit,
        stop: prices.stop,
        touched_trigger: order.aux_price,
        uses_limit: uses_limit(&order.order_type),
        uses_stop: uses_stop(&order.order_type),
        is_market: order.order_type == "MKT",
        is_touched: matches!(order.order_type.as_str(), "MIT" | "LIT"),
    }
}

fn eligible_parent_price(order: &Order, prices: &ChildPrices) -> bool {
    if order.order_type == "TRAIL LIMIT" {
        return false;
    }
    uses_stop(&order.order_type) || (uses_limit(&order.order_type) && prices.limit != UNSET_PRICE)
}

fn saved_parent_offset(
    parent: &Order,
    order: &Order,
    prices: &ChildPrices,
    context: &ChildContext<'_>,
) -> Option<f64> {
    if !eligible_parent_price(order, prices) {
        return None;
    }
    let parent_kind = parent.order_type_named().unwrap_or_default();
    let parent_eligible = context.parent_price.limit != UNSET_PRICE
        || (context.parent_price.uses_stop && parent_kind != "TRAIL LIMIT")
        || matches!(parent_kind, "LMT" | "LOC" | "FUNARI" | "LIT" | "TRAIL LIT")
        || (matches!(parent_kind, "PEG MKT" | "PEG MID" | "PEG BEST")
            && context.exchange != "IBUSOPT");
    if !parent_eligible {
        return None;
    }
    let scale_child = order.scale_price_increment != UNSET_PRICE
        || (order.scale_profit_offset != UNSET_PRICE
            && order.scale_profit_offset > 0.0
            && is_scale_order(parent));
    let parent = if context.parent_finished || scale_child {
        context.parent_trade_price.unwrap_or(UNSET_PRICE)
    } else {
        context.parent_price.selected()
    };
    let price = if uses_stop(&order.order_type) { prices.stop } else { prices.limit };
    (parent != UNSET_PRICE && price != UNSET_PRICE).then(|| (price - parent).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> Order {
        Order {
            order_id: 20,
            action: "BUY".into(),
            order_type: "LMT".into(),
            total_quantity: 10.25,
            lmt_price: 100.0,
            tif: "GTC".into(),
            sl_order_id: 21,
            sl_order_type: "PRESET".into(),
            pt_order_id: 22,
            pt_order_type: "PRESET".into(),
            account: "allocation".into(),
            model_code: "model".into(),
            discretionary_amt: 0.5,
            all_or_none: true,
            manual_order_time: "time".into(),
            ..Order::default()
        }
    }

    fn context() -> ChildContext<'static> {
        ChildContext {
            parent_api_id: 20,
            parent_price: OrderPrice { limit: 100.0, uses_limit: true, ..OrderPrice::default() },
            prices: PriceContext::default(),
            oca_group: "3456",
            parent_is_scale_or_scale_child: false,
            parent_trade_price: None,
            parent_finished: false,
            exchange: "SMART",
            supports_order_type: &|_| true,
            defaults: ChildDefaults::default(),
        }
    }

    #[test]
    fn child_wire_prices_omit_only_the_unset_sentinel() {
        let mut prices = ChildPrices::from_parent(&parent(), context().parent_price, "2", true);
        for (price, expected) in [
            (f64::INFINITY, "∞"),
            (f64::NEG_INFINITY, "-∞"),
            (f64::NAN, "NaN"),
            (1.234567891, "1.23456789"),
            (1.234567899, "1.2345679"),
            (-0.0, "-0.00"),
            (12.5, "12.50"),
        ] {
            prices.limit = price;
            assert_eq!(prices.wire_fields(), vec![(44, expected.into())]);
        }
        prices.limit = UNSET_PRICE;
        assert!(prices.wire_fields().is_empty());
    }

    #[test]
    fn primary_flags_are_applied_to_each_eligible_child() {
        let preset = AttachedPreset {
            auto_stop: 1,
            profit_order_type: "2".into(),
            primary_outside_rth: true,
            primary_use_price_mgmt_algo: true,
            primary_auto_cancel_parent: true,
            ..AttachedPreset::default()
        };
        let mut context = context();
        context.defaults.late_hours = true;
        context.defaults.cancel_parent_enabled = true;
        let children = build_children(&parent(), &preset, &context);
        assert!(children[0].order.outside_rth);
        assert!(!children[1].order.outside_rth);
        for child in children {
            assert_eq!(child.order.use_price_mgmt_algo, Some(1));
            assert!(child.order.auto_cancel_parent);
        }
        context.defaults.regular_hours = true;
        context.defaults.cancel_parent_enabled = false;
        let children = build_children(&parent(), &preset, &context);
        assert!(!children[0].order.outside_rth);
        assert!(!children[0].order.auto_cancel_parent);
    }

    #[test]
    fn late_session_defaults_respect_market_type_and_inherited_tif() {
        let mut preset =
            AttachedPreset { auto_stop: 1, primary_outside_rth: true, ..AttachedPreset::default() };
        let mut context = context();
        context.defaults.late_hours = true;
        context.defaults.market_orders_regular_only = true;
        assert!(!build_children(&parent(), &preset, &context)[0].order.outside_rth);
        preset.auto_stop = 4;
        assert!(build_children(&parent(), &preset, &context)[0].order.outside_rth);
        context.defaults.regular_hours = true;
        assert!(!build_children(&parent(), &preset, &context)[0].order.outside_rth);
        let mut parent = parent();
        parent.tif = "OPG".into();
        let children = build_children(&parent, &preset, &context);
        assert_eq!(children[0].order.tif, "?");
        assert!(children[0].order.outside_rth);
        parent.outside_rth = true;
        parent.tif = "DAY".into();
        assert!(build_children(&parent, &preset, &context)[0].order.outside_rth);
    }

    #[test]
    fn market_restrictions_use_stated_classification_and_option_currency() {
        assert!(market_orders_regular_only("STK", "USD", "USSTK", "", ""));
        assert!(!market_orders_regular_only("STK", "USD", "", "", ""));
        assert!(market_orders_regular_only("OPT", "CAD", "", "STK", "USSTK"));
        assert!(market_orders_regular_only("FOP", "USD", "", "FUT", ""));
        assert!(!market_orders_regular_only("FUT", "USD", "USFUT", "", ""));
    }

    #[test]
    fn scale_parent_classification_depends_on_components_and_type() {
        let mut order = parent();
        order.scale_price_increment = 1.0;
        assert!(!is_scale_order(&order));
        order.scale_init_level_size = 0;
        order.scale_price_increment = UNSET_PRICE;
        assert!(is_scale_order(&order));
        order.order_type = "STP LMT".into();
        assert!(!is_scale_order(&order));
        order.order_type = "MIT".into();
        assert!(is_scale_order(&order));
    }

    #[test]
    fn children_keep_allocation_and_reset_transient_fields_in_stop_first_order() {
        let preset = AttachedPreset {
            auto_stop: 7,
            profit_order_type: "2".into(),
            profit_uses_exact_parent_quantity: true,
            ..AttachedPreset::default()
        };
        let mut parent = parent();
        parent.conditions = vec![crate::types::OrderCondition::Time {
            time: "20260724-12:00:00".into(),
            is_more: true,
            is_conjunction_connection: false,
        }];
        parent.conditions_ignore_rth = true;
        parent.conditions_cancel_order = true;
        parent.conditions_include_overnight = true;
        let children = build_children(&parent, &preset, &context());
        assert_eq!(children.iter().map(|c| c.order.order_id).collect::<Vec<_>>(), [21, 22]);
        assert_eq!(children[0].prices.stop, 99.0);
        assert_eq!(children[1].prices.limit, 101.0);
        for child in children {
            assert_eq!(child.order.action, "SELL");
            assert_eq!(child.order.total_quantity, 10.25);
            assert_eq!(child.order.account, "allocation");
            assert_eq!(child.order.model_code, "model");
            assert_eq!(child.order.parent_id, 20);
            assert_eq!(child.order.oca_group, "3456");
            assert_eq!(child.order.oca_type, 3);
            assert_eq!(child.order.discretionary_amt, 0.0);
            assert!(!child.order.all_or_none);
            assert!(child.order.manual_order_time.is_empty());
            assert!(child.order.conditions.is_empty());
            assert!(!child.order.conditions_ignore_rth && !child.order.conditions_cancel_order);
            assert!(!child.order.conditions_include_overnight);
            assert!(child.order.pt_order_type.is_empty());
            assert!(child.order.sl_order_type.is_empty());
        }
    }

    #[test]
    fn multiple_children_require_oca_support() {
        let preset = AttachedPreset {
            auto_stop: 7,
            profit_order_type: "2".into(),
            ..AttachedPreset::default()
        };
        let mut context = context();
        context.supports_order_type = &|kind| kind != "OCA";
        assert!(build_children(&parent(), &preset, &context).is_empty());
        let mut parent = parent();
        parent.pt_order_type.clear();
        let children = build_children(&parent, &preset, &context);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].order.order_id, 21);
    }

    #[test]
    fn absent_types_and_none_configuration_build_no_children() {
        assert!(build_children(&parent(), &AttachedPreset::default(), &context()).is_empty());
        let parent = Order { sl_order_id: 5, pt_order_id: 6, ..Order::default() };
        let preset = AttachedPreset {
            auto_stop: 7,
            profit_order_type: "2".into(),
            ..AttachedPreset::default()
        };
        assert!(build_children(&parent, &preset, &context()).is_empty());
    }

    #[test]
    fn explicit_types_use_configured_children_without_the_preset_enable_flags() {
        let mut parent = parent();
        parent.sl_order_type = "OTHER".into();
        parent.pt_order_type = "OTHER".into();
        parent.sl_order_id = UNSET_ID;
        parent.transmit = false;
        parent.tif = "OPG".into();
        parent.oca_type = 1;
        let preset = AttachedPreset {
            auto_stop: 7,
            profit_order_type: "2".into(),
            ..AttachedPreset::default()
        };
        let children = build_children(&parent, &preset, &context());
        assert_eq!(children[0].order.order_id, 0);
        assert_eq!(children[0].tif_override, Some(b'?'));
        assert_eq!(children[1].order.tif, "OPG");
        assert_eq!(children[1].order.total_quantity, 10.0);
        assert!(children.iter().all(|c| !c.order.transmit && c.order.oca_type == 1));
    }

    #[test]
    fn parent_trade_price_offsets_are_saved_without_a_fill_subscription() {
        let mut preset = AttachedPreset {
            auto_stop: 7,
            profit_order_type: "2".into(),
            ..AttachedPreset::default()
        };
        preset.stop.use_parent_trade_price = true;
        preset.limit.use_parent_trade_price = true;
        let children = build_children(&parent(), &preset, &context());
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|c| c.use_parent_trade_price));
        assert!(children.iter().all(|c| c.profit_offset == Some(1.0)));
    }

    #[test]
    fn parent_offsets_use_order_prices_until_completion_unless_scale_terms_apply() {
        let mut preset = AttachedPreset { auto_stop: 7, ..AttachedPreset::default() };
        preset.stop.use_parent_trade_price = true;
        let mut context = context();
        context.parent_trade_price = Some(110.0);
        assert_eq!(build_children(&parent(), &preset, &context)[0].profit_offset, Some(1.0));
        context.parent_finished = true;
        assert_eq!(build_children(&parent(), &preset, &context)[0].profit_offset, Some(11.0));
        context.parent_finished = false;
        let mut parent = parent();
        parent.scale_price_increment = 0.5;
        assert_eq!(build_children(&parent, &preset, &context)[0].profit_offset, Some(11.0));
    }

    #[test]
    fn parent_offsets_skip_only_unset_prices() {
        let mut context = context();
        context.parent_finished = true;
        let child = Order { order_type: "STP".into(), ..Order::default() };
        let prices = ChildPrices::from_parent(
            &child,
            OrderPrice { stop: 99.0, ..OrderPrice::default() },
            "3",
            true,
        );
        context.parent_trade_price = Some(UNSET_PRICE);
        assert_eq!(saved_parent_offset(&parent(), &child, &prices, &context), None);
        context.parent_trade_price = Some(f64::INFINITY);
        assert_eq!(saved_parent_offset(&parent(), &child, &prices, &context), Some(f64::INFINITY));
    }

    #[test]
    fn an_ineligible_parent_does_not_supply_a_trade_price_offset() {
        let mut preset = AttachedPreset { auto_stop: 7, ..AttachedPreset::default() };
        preset.stop.use_parent_trade_price = true;
        preset.stop.price_type = 4;
        preset.stop.offset = 99.0;
        let mut parent = parent();
        parent.order_type = "MKT".into();
        parent.lmt_price = UNSET_PRICE;
        let mut context = context();
        context.parent_price = OrderPrice { is_market: true, ..OrderPrice::default() };
        context.parent_finished = true;
        context.parent_trade_price = Some(110.0);
        let children = build_children(&parent, &preset, &context);
        assert!(children[0].use_parent_trade_price);
        assert_eq!(children[0].profit_offset, None);
        context.parent_price.limit = 100.0;
        assert_eq!(build_children(&parent, &preset, &context)[0].profit_offset, Some(11.0));
    }

    #[test]
    fn a_trailing_limit_keeps_its_absolute_limit_and_configured_unit() {
        let mut preset = AttachedPreset { auto_stop: 11, ..AttachedPreset::default() };
        preset.trailing_amount = 0.0;
        preset.primary_trailing_amount = 2.5;
        preset.primary_trailing_unit = PriceUnit::Percent;
        preset.stop_limit.price_type = 11;
        preset.stop_limit.offset = -2.0;
        let children = build_children(&parent(), &preset, &context());
        let fields = children[0].prices.wire_fields();
        assert!(fields.contains(&(44, "98.00".into())));
        assert!(fields.contains(&(6268, "100".into())));
        assert!(fields.contains(&(6117, "99.00".into())));
        assert!(!fields.iter().any(|(tag, _)| *tag == 6370));
    }

    #[test]
    fn trailing_percent_requires_an_eligible_price_rule() {
        let mut preset = AttachedPreset {
            auto_stop: 11,
            profit_order_type: "2".into(),
            trailing_amount: 2.0,
            trailing_unit: PriceUnit::Percent,
            ..AttachedPreset::default()
        };
        let mut context = context();
        context.defaults.percent_prices_restricted = true;
        let children = build_children(&parent(), &preset, &context);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].order.order_id, 22);
        preset.trailing_unit = PriceUnit::Amount;
        assert_eq!(build_children(&parent(), &preset, &context).len(), 2);
        preset.trailing_amount = 0.0;
        preset.primary_trailing_unit = PriceUnit::Percent;
        assert_eq!(build_children(&parent(), &preset, &context).len(), 1);
        context.defaults.percent_prices_restricted = false;
        assert_eq!(build_children(&parent(), &preset, &context).len(), 2);
    }

    #[test]
    fn restricted_price_rules_omit_non_amount_unit_attributes() {
        let preset = AttachedPreset {
            auto_stop: 11,
            trailing_amount: 2.0,
            trailing_unit: PriceUnit::Ticks,
            ..AttachedPreset::default()
        };
        let mut parent = parent();
        parent.adjusted_order_type = "TRAIL".into();
        parent.adjusted_trailing_amount = 1.0;
        parent.adjustable_trailing_unit = 100;
        let mut context = context();
        context.defaults.percent_prices_restricted = true;
        let children = build_children(&parent, &preset, &context);
        assert_eq!(children.len(), 1);
        let fields = children[0].prices.wire_fields();
        assert!(fields.contains(&(99, "2.00".into())));
        assert!(fields.contains(&(6260, "1.00".into())));
        assert!(!fields.iter().any(|(tag, _)| matches!(tag, 6268 | 6269)));
        parent.adjustable_trailing_unit = 0;
        let children = build_children(&parent, &preset, &context);
        assert!(children[0].prices.wire_fields().contains(&(6269, "0".into())));
    }

    #[test]
    fn percentage_size_rounds_half_toward_positive_infinity() {
        let preset = AttachedPreset { profit_order_type: "2".into(), ..AttachedPreset::default() };
        for (quantity, expected) in [(10.5, 11.0), (-10.5, -10.0), (-10.6, -11.0)] {
            let mut parent = parent();
            parent.total_quantity = quantity;
            assert_eq!(
                build_children(&parent, &preset, &context())[0].order.total_quantity,
                expected
            );
        }
    }

    #[test]
    fn unsupported_stop_selection_stays_invalid() {
        let preset = AttachedPreset { auto_stop: 7, ..AttachedPreset::default() };
        let mut context = context();
        context.supports_order_type = &|_| false;
        let children = build_children(&parent(), &preset, &context);
        assert_eq!(children[0].prices.ord_type, "-3");
        assert!(children[0].prices.wire_fields().is_empty());
    }

    #[test]
    fn signed_child_ids_and_parent_side_aliases_survive_construction() {
        let mut parent = parent();
        parent.sl_order_id = -10;
        parent.pt_order_id = -11;
        parent.action = "s".into();
        let preset = AttachedPreset {
            auto_stop: 7,
            profit_order_type: "2".into(),
            ..AttachedPreset::default()
        };
        let mut context = context();
        context.parent_price.side = PriceSide::Sell;
        let children = build_children(&parent, &preset, &context);
        assert_eq!(children[0].order.order_id, -10);
        assert_eq!(children[1].order.order_id, -11);
        assert!(children.iter().all(|child| child.order.action == "BUY"));
        assert_eq!(children[0].prices.stop, 101.0);
        assert_eq!(children[1].prices.limit, 99.0);
    }

    #[test]
    fn adjustable_types_follow_each_limit_and_trailing_specification() {
        let mut preset = AttachedPreset { auto_stop: 9, ..AttachedPreset::default() };
        preset.stop_limit.price_type = 11;
        preset.stop_limit.offset = -2.0;
        preset.adjusted_trigger.price_type = 11;
        preset.adjusted_trigger.offset = 3.0;
        preset.adjusted_stop.price_type = 11;
        preset.adjusted_stop.offset = -3.0;
        preset.adjusted_trailing_amount = 0.5;
        preset.adjusted_trailing_unit = PriceUnit::Ticks;
        let children = build_children(&parent(), &preset, &context());
        let prices = &children[0].prices;
        assert_eq!(prices.ord_type, "4");
        assert_eq!(prices.adjusted_type.as_deref(), Some("T"));
        assert_eq!(prices.adjusted_trigger, 103.0);
        assert_eq!(prices.adjusted_stop, 97.0);
        assert_eq!(prices.adjusted_limit, UNSET_PRICE);
        assert!(prices.wire_fields().contains(&(6269, "1".into())));
    }

    #[test]
    fn inherited_adjustments_keep_the_stated_trailing_unit() {
        let preset = AttachedPreset { auto_stop: 7, ..AttachedPreset::default() };
        for (unit, expected) in [(0, "0"), (1, "1"), (100, "100"), (7, "99")] {
            let mut parent = parent();
            parent.adjusted_order_type = "TRAIL".into();
            parent.adjusted_trailing_amount = 2.0;
            parent.adjustable_trailing_unit = unit;
            let children = build_children(&parent, &preset, &context());
            assert!(children[0].prices.wire_fields().contains(&(6269, expected.into())));
        }
    }

    #[test]
    fn profit_scale_terms_replace_inherited_scale_state() {
        let mut parent = parent();
        parent.scale_profit_offset = 7.0;
        parent.scale_table = "1,5,x".into();
        parent.scale_auto_reset = true;
        parent.scale_init_fill_qty = 20;
        parent.total_quantity = 10.5;
        let preset = AttachedPreset {
            profit_order_type: "E".into(),
            profit_offset: 0.5,
            scale_initial_size: 2,
            scale_subsequent_size: 3,
            scale_price_increment: 0.1,
            profit_uses_exact_parent_quantity: true,
            ..AttachedPreset::default()
        };
        let children = build_children(&parent, &preset, &context());
        let child = &children[0];
        assert_eq!(child.order.total_quantity, 11.0);
        assert_eq!(child.order.scale_init_level_size, 2);
        assert_eq!(child.order.scale_subs_level_size, 3);
        assert_eq!(child.order.scale_price_increment, 0.1);
        assert_eq!(child.order.scale_profit_offset, UNSET_PRICE);
        assert_eq!(child.order.scale_init_fill_qty, i32::MAX);
        assert!(child.order.scale_table.is_empty());
        assert!(!child.order.scale_auto_reset);
        assert!(!child.use_parent_trade_price);
        assert_eq!(child.prices.wire_order_type(false), "P");
        assert_eq!(child.prices.exec_inst(false), "R");
        assert!(child.prices.wire_fields().contains(&(211, "0.50".into())));
    }

    #[test]
    fn profit_type_resolution_keeps_none_and_defaults_only_unknown_keys() {
        assert!(build_children(&parent(), &AttachedPreset::default(), &context()).is_empty());
        let unknown =
            AttachedPreset { profit_order_type: "unknown".into(), ..AttachedPreset::default() };
        let children = build_children(&parent(), &unknown, &context());
        assert_eq!(children[0].prices.ord_type, "2");
        let peg =
            AttachedPreset { profit_order_type: "PEGMID".into(), ..AttachedPreset::default() };
        let children = build_children(&parent(), &peg, &context());
        assert_eq!(children[0].prices.wire_order_type(false), "P");
        assert_eq!(children[0].prices.exec_inst(false), "M");
        let trailing = AttachedPreset { auto_stop: 2, ..AttachedPreset::default() };
        let children = build_children(&parent(), &trailing, &context());
        assert_eq!(children[0].prices.wire_order_type(false), "P");
        assert_eq!(children[0].prices.exec_inst(false), "a");
        assert_eq!(children[0].prices.wire_order_type(true), "T");
        assert_eq!(children[0].prices.exec_inst(true), "");
    }
}
