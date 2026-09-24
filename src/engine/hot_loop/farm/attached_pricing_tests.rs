use super::super::*;
use super::decode_publish_tests::framed_35p;
use crate::client_core::attached_prices::cached_quote_views;

#[test]
fn attached_pricing_retains_stated_fields_and_close_attributes() {
    let mut farm = FarmState::new();
    let mut context = Context::new();
    let shared = SharedState::new();
    let id = context.market.register(41);
    context.market.register_server_tag(9, id);
    context.market.set_min_tick(id, 0.01);
    farm.handle_tick_data(
        &framed_35p(9, &[(0, 1, 0), (4, 1, 2), (3, 2, 1000)]),
        &mut context,
        &shared,
        &None,
    );
    let quote = cached_quote_views(&shared.market, id, true, false, false, false, false).current;
    assert_eq!(quote.bid, Some(0.0));
    assert!(quote.bid_usable);
    assert_eq!(quote.ask, None);
    assert_eq!(quote.close, Some(10.0));
    farm.handle_tick_data(&framed_35p(9, &[(12, 1, 1)]), &mut context, &shared, &None);
    assert_eq!(
        cached_quote_views(&shared.market, id, true, false, false, false, false).current.close,
        None
    );
    farm.handle_tick_data(&framed_35p(9, &[(18, 1, 2), (23, 1, 0)]), &mut context, &shared, &None);
    assert_eq!(
        cached_quote_views(&shared.market, id, true, false, false, false, false).current.close,
        Some(10.0)
    );
    shared.market.zero_all_quotes();
    assert_eq!(
        cached_quote_views(&shared.market, id, true, false, false, false, false).current.bid,
        None
    );
}

#[test]
fn attached_pricing_combo_close_date_uses_field_order() {
    let mut farm = FarmState::new();
    let mut context = Context::new();
    let shared = SharedState::new();
    let id = context.market.register(42);
    context.set_routing(id, "BAG", "SMART");
    context.market.register_server_tag(9, id);
    context.market.set_min_tick(id, 0.01);
    farm.handle_tick_data(&framed_35p(9, &[(3, 2, 1000)]), &mut context, &shared, &None);
    assert_eq!(
        cached_quote_views(&shared.market, id, false, false, true, false, false).current.close,
        None
    );
    farm.handle_tick_data(&framed_35p(9, &[(20, 4, 20260924)]), &mut context, &shared, &None);
    assert_eq!(
        cached_quote_views(&shared.market, id, false, false, true, false, false).current.close,
        Some(10.0)
    );
    farm.handle_tick_data(
        &framed_35p(9, &[(13, 1, 0), (20, 4, 1800000000)]),
        &mut context,
        &shared,
        &None,
    );
    assert_eq!(shared.market.pricing_quote_views(id).records[0].close_date, Some(20260924));
    farm.handle_tick_data(&framed_35p(9, &[(20, 4, 19700101)]), &mut context, &shared, &None);
    assert_eq!(
        cached_quote_views(&shared.market, id, false, false, true, false, false).current.close,
        None
    );
}

#[test]
fn attached_pricing_auction_uses_nonzero_sizes_and_missing_side_fallback() {
    let mut farm = FarmState::new();
    let shared = SharedState::new();
    let mut context = Context::new();
    farm.generic_tick_tags.push((29, 225, 1));
    let mut payload = Vec::new();
    payload.extend_from_slice(&(-5_i32).to_be_bytes());
    payload.extend_from_slice(&0_i32.to_be_bytes());
    payload.extend_from_slice(&3.0_f32.to_be_bytes());
    payload.extend_from_slice(&0_i32.to_be_bytes());
    payload.extend_from_slice(&0_i32.to_be_bytes());
    payload.extend_from_slice(&4.0_f32.to_be_bytes());
    payload.extend_from_slice(&2.0_f32.to_be_bytes());
    payload.extend_from_slice(&(-1_i32).to_be_bytes());
    payload.extend_from_slice(&0_i32.to_be_bytes());
    let mut body = 29_u32.to_be_bytes().to_vec();
    body.push(payload.len() as u8);
    body.extend_from_slice(&payload);
    let mut message = b"35=G\x01".to_vec();
    message.extend_from_slice(&((body.len() * 8) as u16).to_be_bytes());
    message.extend_from_slice(&body);
    farm.handle_generic_tick(&message, &mut context, &shared, &None);
    assert_eq!(shared.market.pricing_loan_sides(1), (Some(4.0), Some(3.0)));
    payload[20..24].copy_from_slice(&f32::MAX.to_be_bytes());
    farm.deliver_pricing_auction(1, &payload, &shared);
    assert_eq!(shared.market.pricing_loan_sides(1), (Some(3.0), Some(3.0)));
    payload[0..4].copy_from_slice(&0_i32.to_be_bytes());
    farm.deliver_pricing_auction(1, &payload, &shared);
    assert_eq!(shared.market.pricing_loan_sides(1), (None, None));
    shared.market.forget_series_ticks(1);
    assert_eq!(shared.market.pricing_loan_sides(1), (None, None));
}

#[test]
fn attached_pricing_vwap_includes_first_total_and_ignores_trade_report_totals() {
    let mut farm = FarmState::new();
    let shared = SharedState::new();
    let payload = |value: f64, shares: i64| {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&value.to_be_bytes());
        bytes.extend_from_slice(&shares.to_be_bytes());
        bytes.extend_from_slice(&1_i32.to_be_bytes());
        bytes
    };
    farm.deliver_running_volume(1, 48, &payload(550.0, 5), &shared);
    assert_eq!(
        cached_quote_views(&shared.market, 1, false, false, false, false, false).current.vwap,
        Some(110.0)
    );
    assert!(shared.market.drain_series_ticks(1).is_empty());
    farm.deliver_running_volume(1, 77, &payload(50.0, 5), &shared);
    assert_eq!(shared.market.pricing_quote_views(1).vwap, Some(110.0));
    farm.deliver_running_volume(1, 48, &payload(600.0, 5), &shared);
    assert_eq!(shared.market.pricing_quote_views(1).vwap, Some(120.0));
    farm.deliver_running_volume(1, 48, &payload(600.0, 0), &shared);
    assert_eq!(shared.market.pricing_quote_views(1).vwap, None);
}
