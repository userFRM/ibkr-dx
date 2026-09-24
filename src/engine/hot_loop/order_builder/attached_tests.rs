use super::*;
use crate::types::{OrderAttrs, OrderKind, ScaleAttrs};
use std::io::Read;

fn sent(
    kind: OrderKind,
    attrs: OrderAttrs,
    tif: u8,
    parent_revision: Option<&str>,
) -> Vec<(u32, String)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    let mut connection = Connection::new_raw(stream).unwrap();
    let mut context = Context::new();
    let instrument = context.market.register_contract(17, "ABC", "STK", "SMART", "");
    context.set_symbol(instrument, "ABC".into());
    if let Some(revision) = parent_revision {
        context.last_clord.insert(10, revision.into());
    }
    let shared = Arc::new(SharedState::new());
    send_order_ex(
        &mut connection,
        &mut context,
        &shared,
        "DU1",
        11,
        instrument,
        Side::Sell,
        100 * crate::types::QTY_SCALE,
        kind,
        tif,
        &attrs,
    )
    .unwrap();
    let mut bytes = vec![0; 16384];
    let count = peer.read(&mut bytes).unwrap();
    String::from_utf8_lossy(&bytes[..count])
        .split('\x01')
        .filter_map(|field| {
            let (tag, value) = field.split_once('=')?;
            Some((tag.parse().ok()?, value.to_string()))
        })
        .collect()
}

fn field(fields: &[(u32, String)], tag: u32) -> Option<&str> {
    fields.iter().find(|(key, _)| *key == tag).map(|(_, value)| value.as_str())
}

#[test]
fn attached_metadata_survives_repricing_and_explicit_profit_zero_replaces_it() {
    let mut resting = crate::types::OrderSpec {
        kind: OrderKind::Market,
        attrs: OrderAttrs {
            attached: Some(Box::new(crate::types::AttachedAttrs {
                api_identity: Some((0, 23)),
                contract_id: Some(91),
                ratio_factor: Some(2.0),
                family_key: "7/1/-9934747".into(),
                parent: "10.7".into(),
                use_parent_price: true,
                profit_offset: Some(2.5),
                combo: None,
            })),
            ..OrderAttrs::default()
        },
    };
    let held = resting.attrs.attached.clone();
    let statement =
        |attrs| crate::types::OrderSpec { kind: OrderKind::Limit { price: 100 }, attrs };
    merge_statement(&mut resting, statement(OrderAttrs::default()));
    assert_eq!(resting.attrs.attached, held);
    let order = crate::types::model::Order { scale_profit_offset: 0.0, ..Default::default() };
    merge_statement(&mut resting, statement(order.attrs()));
    assert_eq!(resting.attrs.attached.as_ref().unwrap().profit_offset, Some(0.0));
}

#[test]
fn a_resolved_combination_id_survives_submission_replacement_and_cancellation() {
    let (connection, mut peer) = Connection::for_test();
    let mut connection = Some(connection);
    let mut context = Context::new();
    let instrument = context.market.register_contract(0, "ABC", "BAG", "SMART", "");
    context.set_symbol(instrument, "ABC".into());
    context.market.set_order_identity(instrument, "|||100");
    let shared = Arc::new(SharedState::new());
    let mut receive = || {
        let mut bytes = [0; 16384];
        let count = peer.read(&mut bytes).unwrap();
        String::from_utf8_lossy(&bytes[..count])
            .split('\x01')
            .filter_map(|field| {
                let (tag, value) = field.split_once('=')?;
                Some((tag.parse::<u32>().ok()?, value.to_string()))
            })
            .collect::<Vec<_>>()
    };
    let attrs = OrderAttrs {
        attached: Some(Box::new(crate::types::AttachedAttrs {
            contract_id: Some(91),
            ratio_factor: Some(2.0),
            combo: Some(crate::types::AttachedComboFrame {
                multiplier: Some(50.0),
                price_mode: 1,
                combo_type: 2,
                ..Default::default()
            }),
            ..Default::default()
        })),
        combo_legs: vec![crate::types::ComboLegSpec { con_id: 18, ..Default::default() }],
        ..Default::default()
    };
    send_order_ex(
        connection.as_mut().unwrap(),
        &mut context,
        &shared,
        "DU1",
        11,
        instrument,
        Side::Buy,
        crate::types::QTY_SCALE,
        OrderKind::Limit { price: 100 },
        b'0',
        &attrs,
    )
    .unwrap();
    let submitted = receive();
    assert_eq!(field(&submitted, 6008), Some("91"));
    assert_eq!(field(&submitted, 6724), Some("2.00"));
    assert_eq!(field(&submitted, 231), Some("50.00"));
    assert_eq!(submitted.iter().filter(|(tag, _)| *tag == 231).count(), 1);
    assert_eq!(field(&submitted, 6175), Some("2"));
    assert_eq!(field(&submitted, 6134), Some("2"));
    assert_eq!(field(&submitted, 6080), Some("18"));
    assert_eq!(submitted.iter().filter(|(tag, _)| *tag == 6008).count(), 1);
    assert_eq!(context.market.con_id(instrument), Some(0));
    context.pending_orders.push(OrderRequest::Modify {
        order_id: 11,
        price: 101,
        qty: crate::types::QTY_SCALE,
        outside_rth: false,
        ord_type: b'2',
        tif: b'0',
        stop_price: 0,
        spec: Some(Box::new(crate::types::OrderSpec {
            kind: OrderKind::Limit { price: 101 },
            attrs: OrderAttrs { combo_legs: attrs.combo_legs.clone(), ..Default::default() },
        })),
    });
    drain_and_send_orders(
        &mut connection,
        &mut context,
        "DU1",
        &mut HeartbeatState::new(),
        false,
        &shared,
        false,
        &None,
        &mut { usize::MAX },
    );
    let replaced = receive();
    assert_eq!(field(&replaced, 35), Some("G"));
    assert_eq!(field(&replaced, 6008), Some("91"));
    assert_eq!(field(&replaced, 6724), Some("2.00"));
    assert_eq!(field(&replaced, 231), Some("50.00"));
    assert_eq!(field(&replaced, 6175), Some("2"));
    assert_eq!(field(&replaced, 6134), Some("2"));
    send_cancel(
        connection.as_mut().unwrap(),
        &mut context,
        &shared,
        "DU1",
        11,
        &crate::types::model::OrderCancel::default(),
    )
    .unwrap();
    let cancelled = receive();
    assert_eq!(field(&cancelled, 35), Some("F"));
    assert_eq!(field(&cancelled, 6008), Some("91"));
}

#[test]
fn resolved_combination_terms_control_destinations_and_underlying_prices() {
    use crate::types::{AttachedComboFrame, ComboLegSpec, DeltaNeutralContractSpec};
    for (include, separate, mode) in [(false, false, 0), (true, true, 1)] {
        let attrs = OrderAttrs {
            attached: Some(Box::new(crate::types::AttachedAttrs {
                combo: Some(AttachedComboFrame {
                    multiplier: None,
                    price_mode: mode,
                    combo_type: 11,
                    include_leg_exchanges: include,
                    market_data_generic: false,
                    separate_delta_neutral: separate,
                    delta_neutral_contract: Some(DeltaNeutralContractSpec {
                        con_id: 31,
                        delta: 0.5,
                        price: 12.5,
                    }),
                }),
                ..Default::default()
            })),
            delta_neutral_contract: Some(Box::new(DeltaNeutralContractSpec {
                con_id: 31,
                delta: 0.5,
                price: 1250.0,
            })),
            combo_legs: vec![
                ComboLegSpec {
                    con_id: 18,
                    ratio: 1,
                    exchange: "ARCA".into(),
                    ..Default::default()
                },
                ComboLegSpec {
                    con_id: 19,
                    ratio: 2,
                    exchange: "SMART".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let fields = sent(OrderKind::Market, attrs, b'0', None);
        let exchanges: Vec<_> =
            fields.iter().filter(|(tag, _)| *tag == 616).map(|(_, value)| value.as_str()).collect();
        assert_eq!(exchanges, if include { vec!["ARCA", "SMART"] } else { vec![] });
        assert_eq!(field(&fields, 231), None);
        assert_eq!(field(&fields, 6175), Some(if mode == 0 { "0" } else { "2" }));
        assert_eq!(field(&fields, 6134), Some("11"));
        assert_eq!(field(&fields, 6147), separate.then_some("1"));
        assert_eq!(field(&fields, 6150), Some("31"));
        assert_eq!(field(&fields, 6148), Some("0.500000"));
        assert_eq!(field(&fields, 6149), Some("12.500000"));
        for tag in [6150, 6148, 6149] {
            assert_eq!(fields.iter().filter(|(key, _)| *key == tag).count(), 1);
        }
    }
}

#[test]
fn a_recovered_child_keeps_attachment_metadata_on_its_next_replace() {
    let (connection, mut peer) = Connection::for_test();
    let mut connection = Some(connection);
    let mut context = Context::new();
    let instrument = context.register_instrument(17);
    context.set_symbol(instrument, "ABC".into());
    context.insert_order(crate::types::Order {
        order_id: 11,
        instrument,
        side: Side::Sell,
        price: 100 * crate::types::PRICE_SCALE,
        qty: crate::types::QTY_SCALE,
        filled: 0,
        status: OrderStatus::Submitted,
        ord_type: b'2',
        tif: b'0',
        stop_price: 0,
    });
    context.last_clord.insert(11, "11.0".into());
    let shared = Arc::new(SharedState::new());
    shared.orders.note_attached_order_metadata(
        11,
        crate::bridge::AttachedOrderMetadata {
            family_key: Some("7/1/-9934747".into()),
            parent: Some("10.7".into()),
            use_parent_price: Some(true),
            profit_offset: Some(2.5),
            api_order_id: Some(0),
            api_client_id: Some(23),
        },
    );
    context.pending_orders.push(OrderRequest::Modify {
        order_id: 11,
        price: 101 * crate::types::PRICE_SCALE,
        qty: crate::types::QTY_SCALE,
        outside_rth: false,
        ord_type: b'2',
        tif: b'0',
        stop_price: 0,
        spec: Some(Box::new(crate::types::OrderSpec {
            kind: OrderKind::Limit { price: 101 * crate::types::PRICE_SCALE },
            attrs: OrderAttrs::default(),
        })),
    });
    drain_and_send_orders(
        &mut connection,
        &mut context,
        "DU1",
        &mut HeartbeatState::new(),
        false,
        &shared,
        false,
        &None,
        &mut { usize::MAX },
    );
    let mut bytes = vec![0; 16384];
    let count = peer.read(&mut bytes).unwrap();
    let fields: Vec<_> = String::from_utf8_lossy(&bytes[..count])
        .split('\x01')
        .filter_map(|field| {
            let (tag, value) = field.split_once('=')?;
            Some((tag.parse().ok()?, value.to_string()))
        })
        .collect();
    for (tag, value) in
        [(35, "G"), (6531, "7/1/-9934747"), (6107, "10.7"), (6704, "1"), (6446, "2.50")]
    {
        assert_eq!(field(&fields, tag), Some(value), "field {tag}");
    }
    assert_eq!(field(&fields, 6121), None);
    assert_eq!(field(&fields, 6119), None);
}

#[test]
fn attached_trailing_limit_carries_absolute_limit_and_all_adjustments() {
    let prices = [
        (44, "98.5"),
        (99, "2.5"),
        (211, "2.5"),
        (6268, "100"),
        (6117, "99"),
        (6257, "1"),
        (6261, "TSL"),
        (6258, "105"),
        (6259, "101"),
        (6262, "100.5"),
        (6260, "1"),
        (6269, "0"),
    ]
    .into_iter()
    .map(|(tag, value)| (tag, value.into()))
    .collect();
    let fields = sent(
        OrderKind::Attached { ord_type: "TSL".into(), exec_inst: String::new(), prices },
        OrderAttrs::default(),
        b'1',
        None,
    );
    assert_eq!(field(&fields, 40), Some("TSL"));
    for (tag, value) in [
        (44, "98.5"),
        (99, "2.5"),
        (211, "2.5"),
        (6268, "100"),
        (6117, "99"),
        (6257, "1"),
        (6261, "TSL"),
        (6258, "105"),
        (6259, "101"),
        (6262, "100.5"),
        (6260, "1"),
        (6269, "0"),
    ] {
        assert_eq!(field(&fields, tag), Some(value), "field {tag}");
    }
    assert_eq!(field(&fields, 6370), None);
    assert_eq!(field(&fields, 18), None);
}

#[test]
fn attached_parent_price_and_family_metadata_are_stated_once() {
    let attrs = OrderAttrs {
        attached: Some(Box::new(crate::types::AttachedAttrs {
            api_identity: Some((0, 23)),
            family_key: "7/1/-9934747".into(),
            use_parent_price: true,
            profit_offset: Some(0.0),
            ..Default::default()
        })),
        parent_id: 10,
        oca_group_str: "10".into(),
        oca_type: 3,
        scale: Some(Box::new(ScaleAttrs {
            profit_offset: 20 * crate::types::PRICE_SCALE,
            ..ScaleAttrs::default()
        })),
        ..OrderAttrs::default()
    };
    let fields = sent(
        OrderKind::Attached {
            ord_type: "3".into(),
            exec_inst: String::new(),
            prices: vec![(99, "99".into())],
        },
        attrs,
        b'?',
        Some("10.7"),
    );
    for (tag, value) in [
        (6121, "0"),
        (6119, "23"),
        (6107, "10.7"),
        (6531, "7/1/-9934747"),
        (6704, "1"),
        (6446, "0.00"),
        (583, "10"),
        (6209, "ReduceOnFillNonBlock"),
        (59, "?"),
    ] {
        assert_eq!(field(&fields, tag), Some(value));
        assert_eq!(fields.iter().filter(|(key, _)| *key == tag).count(), 1);
    }
}

#[test]
fn attached_relative_and_trailing_instructions_are_separate_from_the_order_type() {
    for (kind, instruction) in [("P", "R"), ("P", "a"), ("T", "")] {
        let fields = sent(
            OrderKind::Attached {
                ord_type: kind.into(),
                exec_inst: instruction.into(),
                prices: vec![(211, "1".into())],
            },
            OrderAttrs::default(),
            b'0',
            None,
        );
        assert_eq!(field(&fields, 40), Some(kind));
        assert_eq!(field(&fields, 18), (!instruction.is_empty()).then_some(instruction));
    }
}

/// A child is tracked under the type its own name on the wire states, as a
/// report of it would be read: a later replace restates that type.
#[test]
fn an_attached_child_is_tracked_under_the_type_it_is_sent_as() {
    for (sent, exec_inst, tracked) in [
        ("LT", "", crate::types::ORD_LIT),
        ("SP", "", crate::types::ORD_STP_PRT),
        ("SMKT", "", crate::types::ORD_SNAP_MKT),
        ("SMID", "", crate::types::ORD_SNAP_MID),
        ("SREL", "", crate::types::ORD_SNAP_PRI),
        ("P", "P", crate::types::ORD_PEG_MKT),
        ("P", "M", crate::types::ORD_PEG_MID),
        ("TSL", "", crate::types::ORD_TRAIL_LIMIT),
        ("3", "", b'3'),
    ] {
        let kind = OrderKind::Attached {
            ord_type: sent.into(),
            exec_inst: exec_inst.into(),
            prices: vec![(44, "10.5".into()), (99, "9".into())],
        };
        assert_eq!(
            tracked_shape(&kind),
            (tracked, crate::types::price_from_f64(10.5), crate::types::price_from_f64(9.0)),
            "{sent} {exec_inst}"
        );
    }
}

/// Any child names its parent by the parent's full venue name, revision
/// included: the name the venue last took the parent under.
#[test]
fn a_child_names_its_parent_by_the_parents_current_revision() {
    for (revision, named) in [(Some("10.3"), "10.3"), (Some("10"), "10"), (None, "10.0")] {
        let attrs = OrderAttrs { parent_id: 10, ..OrderAttrs::default() };
        let fields = sent(OrderKind::Limit { price: 100 }, attrs, b'0', revision);
        assert_eq!(field(&fields, 6107), Some(named), "{revision:?}");
    }
    let fields = sent(OrderKind::Limit { price: 100 }, OrderAttrs::default(), b'0', Some("10.3"));
    assert_eq!(field(&fields, 6107), None);
}
