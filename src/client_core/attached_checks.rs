//! Checks on the ids and preset selectors of attached orders.

use crate::error_codes::{DUPLICATE_ORDER_ID, REQUEST_NOT_READ, Refusal};
use crate::types::model::Order;

/// `PRESET`, compared without case as a gateway compares it: the long s (`ſ`)
/// upper-cases to `S` as well.
fn is_preset(kind: &str) -> bool {
    kind.chars().count() == 6
        && kind
            .chars()
            .zip("PRESET".chars())
            .all(|(c, p)| c.to_ascii_uppercase() == p || (c, p) == ('ſ', 'S'))
}

pub(crate) fn requested(order: &Order) -> bool {
    order.sl_order_id != i32::MAX
        || !order.sl_order_type.is_empty()
        || order.pt_order_id != i32::MAX
        || !order.pt_order_type.is_empty()
}

pub(crate) fn check_ids(
    parent: i64,
    order: &Order,
    highest: i64,
    mut existing: impl FnMut(i64) -> bool,
    mut reusable: impl FnMut(i64) -> bool,
) -> Result<(), Refusal> {
    let profit = (order.pt_order_id != i32::MAX).then_some(i64::from(order.pt_order_id));
    let stop = (order.sl_order_id != i32::MAX).then_some(i64::from(order.sl_order_id));
    for id in [Some(parent), profit, stop].into_iter().flatten() {
        if existing(id) {
            continue;
        }
        if id == 0 || id == i64::from(i32::MAX) || id == i64::from(i32::MIN) {
            return Err(Refusal::stated(10149, format!("Invalid order id: {id}")));
        }
        if id <= highest && !reusable(id) {
            return Err(Refusal::stated(DUPLICATE_ORDER_ID, format!("Duplicate order id: {id}")));
        }
    }
    if profit == Some(parent) || stop == Some(parent) || profit.is_some() && profit == stop {
        return Err(Refusal::stated(DUPLICATE_ORDER_ID, "Duplicate order id"));
    }
    Ok(())
}

/// The attached fields as a gateway reads them. Both refusals are raised
/// while the request is read, so they come back as a request that could not be
/// read, ahead of anything the order is validated for.
pub(crate) fn check_selectors(order: &Order, disabled: bool) -> Result<(), Refusal> {
    let unread =
        |text: &str| Refusal::stated(REQUEST_NOT_READ, format!("Error reading request: {text}"));
    if disabled {
        return Err(unread(
            "Attaching stop-loss or profit-taker is not allowed as part of a single placeOrder request, please submit such orders separately.",
        ));
    }
    for (name, id, kind) in [
        ("Stop Loss", order.sl_order_id, &order.sl_order_type),
        ("Profit Taker", order.pt_order_id, &order.pt_order_type),
    ] {
        if (id != i32::MAX) != is_preset(kind) {
            return Err(unread(&format!("Invalid value for {name} order-id or order-type")));
        }
    }
    Ok(())
}

pub(crate) fn check_presets(order: &Order, profit: bool, stop: bool) -> Result<(), Refusal> {
    for (name, kind, enabled) in
        [("Profit Taker", &order.pt_order_type, profit), ("Stop Loss", &order.sl_order_type, stop)]
    {
        if is_preset(kind) && !enabled {
            return Err(Refusal::stated(
                10355,
                format!("Cannot auto-attach {name}. Preset is not defined."),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family() -> Order {
        Order {
            pt_order_id: 12,
            pt_order_type: "PRESET".into(),
            sl_order_id: 11,
            sl_order_type: "PRESET".into(),
            ..Order::default()
        }
    }

    #[test]
    fn ids_are_checked_parent_then_profit_then_stop() {
        let mut order = family();
        order.pt_order_id = 0;
        order.sl_order_id = i32::MIN;
        for (parent, expected) in [(0, "Invalid order id: 0"), (10, "Invalid order id: 0")] {
            let refusal = check_ids(parent, &order, 0, |_| false, |_| false).unwrap_err();
            assert_eq!((refusal.code, refusal.message.as_str()), (10149, expected));
        }
        order.pt_order_id = 12;
        assert_eq!(
            check_ids(10, &order, 0, |_| false, |_| false).unwrap_err().message,
            "Invalid order id: -2147483648"
        );
    }

    #[test]
    fn existing_ids_bypass_numeric_and_sequence_checks_but_must_be_distinct() {
        let mut order = family();
        order.pt_order_id = 0;
        order.sl_order_id = -2;
        assert!(check_ids(10, &order, 20, |_| true, |_| false).is_ok());
        order.pt_order_id = 10;
        let refusal = check_ids(10, &order, 20, |_| true, |_| false).unwrap_err();
        assert_eq!((refusal.code, refusal.message.as_str()), (103, "Duplicate order id"));
    }

    #[test]
    fn an_unknown_negative_id_fails_sequence_not_numeric_validity() {
        let mut order = family();
        order.pt_order_id = -2;
        let refusal = check_ids(10, &order, 0, |_| false, |_| false).unwrap_err();
        assert_eq!((refusal.code, refusal.message.as_str()), (103, "Duplicate order id: -2"));
        assert!(check_ids(10, &order, 0, |_| false, |id| id == -2).is_ok());
    }

    #[test]
    fn each_id_is_compared_with_the_prior_mark() {
        assert!(check_ids(10, &family(), 9, |_| false, |_| false).is_ok());
        let refusal = check_ids(10, &family(), 10, |_| false, |_| false).unwrap_err();
        assert_eq!((refusal.code, refusal.message.as_str()), (103, "Duplicate order id: 10"));
        let mut order = family();
        order.pt_order_id = order.sl_order_id;
        assert_eq!(check_ids(10, &order, 9, |_| false, |_| false).unwrap_err().code, 103);
    }

    #[test]
    fn feature_denial_precedes_selector_pairing_and_stop_precedes_profit() {
        let mut order = family();
        order.sl_order_type.clear();
        order.pt_order_type.clear();
        let refusal = check_selectors(&order, true).unwrap_err();
        assert_eq!(
            (refusal.code, refusal.message.as_str()),
            (
                320,
                "Error reading request: Attaching stop-loss or profit-taker is not allowed as part of a single placeOrder request, please submit such orders separately."
            )
        );
        let refusal = check_selectors(&order, false).unwrap_err();
        assert_eq!(
            (refusal.code, refusal.message.as_str()),
            (320, "Error reading request: Invalid value for Stop Loss order-id or order-type")
        );
        order.sl_order_id = i32::MAX;
        assert_eq!(
            check_selectors(&order, false).unwrap_err().message,
            "Error reading request: Invalid value for Profit Taker order-id or order-type"
        );
    }

    #[test]
    fn only_preset_requires_an_id() {
        let mut order = Order::default();
        assert!(!requested(&order));
        order.sl_order_type = "anything".into();
        assert!(requested(&order));
        assert!(check_selectors(&order, false).is_ok());
        order.sl_order_type = "pReSeT".into();
        assert_eq!(check_selectors(&order, false).unwrap_err().code, 320);
        order.sl_order_id = 11;
        assert!(check_selectors(&order, false).is_ok());
    }

    #[test]
    fn preset_is_compared_without_case_as_a_gateway_compares_it() {
        for kind in ["preset", "PREſET", "preſet"] {
            assert!(is_preset(kind), "{kind}");
            let order = Order { pt_order_type: kind.into(), ..Order::default() };
            assert_eq!(check_presets(&order, false, true).unwrap_err().code, 10355, "{kind}");
            assert_eq!(check_selectors(&order, false).unwrap_err().code, 320, "{kind}");
        }
        for kind in ["PRESETS", "PRESE", "PRÉSET", "PRESET\u{0}"] {
            assert!(!is_preset(kind), "{kind}");
        }
    }

    #[test]
    fn preset_enable_checks_profit_before_stop() {
        let order = family();
        let refusal = check_presets(&order, false, false).unwrap_err();
        assert_eq!(
            (refusal.code, refusal.message.as_str()),
            (10355, "Cannot auto-attach Profit Taker. Preset is not defined.")
        );
        assert_eq!(
            check_presets(&order, true, false).unwrap_err().message,
            "Cannot auto-attach Stop Loss. Preset is not defined."
        );
        assert!(check_presets(&order, true, true).is_ok());
        assert!(
            check_presets(
                &Order { pt_order_type: "other".into(), ..Order::default() },
                false,
                false
            )
            .is_ok()
        );
    }
}
