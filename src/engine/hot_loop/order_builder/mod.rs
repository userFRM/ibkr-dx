use std::sync::Arc;
use std::time::Instant;

use crate::bridge::SharedState;
use crate::protocol::datetime::{chrono_free_timestamp, unix_to_ib_utc_dash};
use crate::engine::context::Context;
use crate::protocol::connection::Connection;
use crate::protocol::fix;
use crate::types::{AlgoParams, OrderCondition, OrderRequest, OrderStatus, OrderUpdate, RiskAversion, Side, qty_to_f64};

use super::{HeartbeatState, format_price, format_qty, format_uint};
use crate::client_core::attached_children::wire_number;

/// Keep an outbound order frame beside the inbound ones, under the same
/// switch, so a live reading shows what went out as well as what came back.
fn note_sent(shared: &SharedState, fields: &[(u32, &str)]) {
    if *crate::engine::hot_loop::CAPTURE_WIRE {
        let text: String = fields.iter().map(|(t, v)| format!("{t}={v}\u{1}")).collect();
        let hex: String = text.bytes().map(|b| format!("{b:02x}")).collect();
        shared.market.note_unread_wire("trading-sent", hex);
    }
}

/// Whether an account's model is one the venue is told about on tag 6700.
///
/// The default model is not. The venue's own gate is a non-empty test that
/// also excludes the default sleeve by name, so an order that names it states
/// no model at all — and this client, stating it, put a tag on the wire that
/// the venue is never sent.
fn states_a_model(code: &str) -> bool {
    !code.is_empty() && code != "Core"
}

/// Merge what a replace states onto the order the venue is already working.
///
/// A replace is not a fresh statement of an order; it is a set of changes to
/// one the venue holds. What the caller states is applied to the resting
/// terms and the result is what goes out, so anything the caller says nothing
/// about keeps the value it already had.
///
/// Two kinds of field are left alone. Some cannot be changed by a replace at
/// all — the OCA group an order belongs to and the way it cancels, the parent
/// it was placed under, whether it may trade outside the regular session, and
/// whether it may join the pre-open auction — and the
/// venue answers a replace that tries by naming them rather than by applying
/// them. The rest are the ones whose absence is not a value: a caller who
/// states no order reference is not asking for the reference to be cleared,
/// and clearing it lost the caller's own name for an order they were only
/// repricing.
fn merge_statement(resting: &mut crate::types::OrderSpec, stated: crate::types::OrderSpec) {
    // The group, the way it cancels and the parent are the order's own: a
    // gateway neither applies nor carries a different one on a replace.
    let keep_oca = resting.attrs.oca_group;
    let keep_oca_str = std::mem::take(&mut resting.attrs.oca_group_str);
    let keep_oca_type = resting.attrs.oca_type;
    let keep_parent = resting.attrs.parent_id;
    let keep_outside_rth = resting.attrs.outside_rth;
    let keep_allow_pre_open = resting.attrs.allow_pre_open;
    let keep_attached = resting.attrs.attached.take();
    // Absence is not a value on these: the caller stating nothing leaves what
    // the order already carries.
    let kept_where_unstated = [
        (std::mem::take(&mut resting.attrs.order_ref), &stated.attrs.order_ref),
        (std::mem::take(&mut resting.attrs.algo_id), &stated.attrs.algo_id),
        (std::mem::take(&mut resting.attrs.mifid2_decision_maker), &stated.attrs.mifid2_decision_maker),
        (std::mem::take(&mut resting.attrs.mifid2_decision_algo), &stated.attrs.mifid2_decision_algo),
        (std::mem::take(&mut resting.attrs.mifid2_execution_trader), &stated.attrs.mifid2_execution_trader),
        (std::mem::take(&mut resting.attrs.mifid2_execution_algo), &stated.attrs.mifid2_execution_algo),
    ]
    .map(|(held, asked)| if asked.is_empty() { held } else { asked.clone() });

    *resting = stated;

    resting.attrs.oca_group = keep_oca;
    resting.attrs.oca_group_str = keep_oca_str;
    resting.attrs.oca_type = keep_oca_type;
    resting.attrs.parent_id = keep_parent;
    resting.attrs.outside_rth = keep_outside_rth;
    resting.attrs.allow_pre_open = keep_allow_pre_open;
    resting.attrs.keep_attached(keep_attached.as_deref());
    let [order_ref, algo_id, decision_maker, decision_algo, execution_trader, execution_algo] =
        kept_where_unstated;
    resting.attrs.order_ref = order_ref;
    resting.attrs.algo_id = algo_id;
    resting.attrs.mifid2_decision_maker = decision_maker;
    resting.attrs.mifid2_decision_algo = decision_algo;
    resting.attrs.mifid2_execution_trader = execution_trader;
    // The fourth of the same set. Left out, a replace of an order placed with
    // one told the venue the order no longer carries it, while the three
    // beside it were restated — and every later replace of that order said so
    // again.
    resting.attrs.mifid2_execution_algo = execution_algo;
}

/// Say that a change did not go, on the channel a refusal already travels on.
///
/// The surfaces restate their record before the command is queued, because the
/// venue can answer a replacement before the call that sent it returns. A
/// refusal from this side of the wire never reached them: the message said the
/// change did not happen and their own book went on stating that it had.
fn the_change_did_not_go(
    order_id: u64,
    instrument: crate::types::InstrumentId,
    stands_as: Option<crate::types::OrderStatus>,
    context: &Context,
    shared: &SharedState,
    event_tx: &Option<crate::engine::hot_loop::EventSink>,
) {
    let reject = crate::types::CancelReject {
        order_id,
        instrument,
        // A modification, and refused here rather than by the venue, which
        // states no reason code of its own for one it never saw.
        reject_type: 2,
        reason_code: -1,
        still_working: stands_as,
        // Refused here, so it answers the change the caller has just made and
        // nothing else.
        answers_a_live_change: true,
        timestamp_ns: context.now_ns(),
    };
    shared.orders.push_cancel_reject(reject);
    crate::engine::hot_loop::emit(event_tx, crate::bridge::Event::CancelReject(reject));
}

/// Whether the slot a request names still holds the contract the caller meant.
///
/// A slot is an index, and a freed one goes to the next contract that needs it.
/// A caller reads which slot a contract holds from a cache that the engine
/// empties as it frees them — but the read and the request are two moments, and
/// between them the slot can be given away. Built anyway, the order goes out on
/// whatever holds the slot now: another contract's symbol, another contract's
/// routing, and a fill that moves that contract's position.
///
/// A caller that named no contract id states nothing to check, and a slot with
/// none registered against it is one the venue has not yet named — neither is a
/// disagreement.
fn the_slot_is_not_that_contract(
    context: &Context, instrument: crate::types::InstrumentId, con_id: i64,
) -> Option<String> {
    if con_id == 0 {
        return None;
    }
    match context.market.con_id(instrument) {
        Some(holds) if holds != con_id => Some(format!(
            "this order names contract {con_id} and the place it was given now holds \
             {holds}, so it was not sent: ask for the contract again and place it afresh",
        )),
        _ => None,
    }
}


pub(crate) fn drain_and_send_orders(
    ccp_conn: &mut Option<Connection>,
    context: &mut Context,
    account_id: &str,
    hb: &mut HeartbeatState,
    disconnected: bool,
    shared: &Arc<SharedState>,
    // Whether a reconnect's recovery is still settling what the broker holds.
    recovery_pending: bool,
    event_tx: &Option<crate::engine::hot_loop::EventSink>,
    left: &mut usize,
) {
    // If CCP is disconnected, leave orders in the pending buffer for retry after
    // reconnect.
    if disconnected || *left == 0 {
        return;
    }

    // Claim the buffer only once there is somewhere to send it. Draining first
    // and then finding no socket dropped every pending order on the floor.
    let conn = match ccp_conn.as_mut() {
        Some(c) => c,
        None => return,
    };
    let orders: Vec<OrderRequest> = context.drain_pending_orders().collect();
    let mut unsent: Vec<OrderRequest> = Vec::new();
    for order_req in orders {
        // Once a write has abandoned the transport nothing else can leave on
        // it, and the pre-write guard refuses the rest before they touch the
        // wire. Those are not in doubt the way the failed one is: they were
        // never sent, so they go back to wait for the reconnect rather than
        // being reported as orders of unknown state.
        if *left == 0 || conn.write_failed() {
            unsent.push(order_req);
            continue;
        }
        // A cancel or a replace names a version of an order the recovery may be
        // about to correct — a replace that failed left its attempted ClOrdID
        // in front of the one the broker actually holds. Sent now they would
        // state that version and be refused, leaving the order live.
        //
        // Only where that order's state is actually in doubt. An order placed
        // since the reconnect is not, and holding its cancel for the length of
        // a recovery that has nothing to do with it can let it fill first.
        // Cancel-all is included: it sends the same per-order frames, so it
        // carries the same speculative versions.
        let waits_for_recovery = recovery_pending
            && match order_req {
                OrderRequest::Cancel { order_id, .. } | OrderRequest::Modify { order_id, .. } => {
                    context.order(order_id).is_some_and(|o| o.status == OrderStatus::Uncertain)
                }
                // Scoped to the instrument this request cancels. An
                // `Uncertain` order on another instrument does not hold it.
                OrderRequest::CancelAll { instrument, .. } => context
                    .uncertain_orders()
                    .iter()
                    .any(|o| o.instrument == instrument),
                OrderRequest::GlobalCancel { ref instruments, .. } => context.uncertain_orders()
                    .iter().any(|o| instruments.contains(&o.instrument)),
                _ => false,
            };
        if waits_for_recovery {
            unsent.push(order_req);
            continue;
        }
        *left -= 1;
        // The one request that names a slot and puts nothing in the book. It
        // is out of the buffer from here and the loop reconsiders the slot
        // later in this same lap. Said for every request that names one
        // instead, a session placing orders enqueued a scan of the whole
        // open-order book per order placed, to be told what the reclaim's own
        // guard already knew — the submit is in the book by then. Said after
        // the match instead, an arm that leaves the loop early skipped it,
        // which is safe today only because a change carries no slot.
        if let OrderRequest::CancelAll { instrument, .. } = &order_req {
            context.slots_to_reconsider.push(*instrument);
        }
        if let OrderRequest::GlobalCancel { instruments, .. } = &order_req {
            context.slots_to_reconsider.extend(instruments.iter().copied());
        }
        let cancel_ids = match &order_req {
            OrderRequest::CancelAll { instrument, .. } => Some(context.open_orders_for(*instrument)
                .iter().map(|o| o.order_id).collect::<Vec<_>>()),
            OrderRequest::GlobalCancel { instruments, .. } => Some(instruments.iter()
                .flat_map(|instrument| context.open_orders_for(*instrument).iter().map(|o| o.order_id).collect::<Vec<_>>()).collect()),
            _ => None,
        };
        let oid = order_req.order_id();
        // Every leg this request writes. A bracket sends three and reports one
        // outcome, so a failure names the whole set rather than the first id.
        let written = order_req.order_ids();
        // What the engine believed before this request touched anything. A
        // replace writes its attempt into the tracked state ahead of the write,
        // and a write that fails must not leave that attempt standing as though
        // the broker had accepted it.
        let before = context.order(oid).copied();
        // Whether this request is the one that wrote an attempt into the
        // record. A cancel names an order that is in the book — which is why
        // it is being cancelled — so `before` is always Some for one, and the
        // rollback below took a failed cancel for a failed replace: it threw
        // away the fallback kept for a replacement still outstanding, and both
        // the acceptance and the refusal of that replacement then read as
        // belonging to nothing.
        let restating = matches!(&order_req, OrderRequest::Modify { .. });
        // Prices go out as stated. The venue rejects a price off the
        // contract's tick grid rather than adjusting it, so snapping here would
        // substitute a price the caller never gave.
        let result = match order_req {
            OrderRequest::SubmitEx {
                order_id, instrument, con_id, side, qty, kind, tif, attrs,
            } => {
                if let Some(refusal) = the_slot_is_not_that_contract(context, instrument, con_id) {
                    shared.orders.push_order_inactive(
                        order_id, crate::types::model::OrderOp::Place, ORDER_NOT_FOUND_ERROR_CODE, refusal,
                    );
                    report_uncertain(context, shared, event_tx, order_id);
                    continue;
                }
                send_order_ex(
                    conn, context, shared, account_id, order_id, instrument, side, qty, kind, tif,
                    &attrs,
                )
            }
            OrderRequest::SubmitBracket {
                con_id,
                parent_id,
                tp_id,
                sl_id,
                instrument,
                side,
                qty,
                entry_price,
                take_profit,
                stop_loss,
            } => {
                if let Some(refusal) = the_slot_is_not_that_contract(context, instrument, con_id) {
                    for id in [parent_id, tp_id, sl_id] {
                        shared.orders.push_order_inactive(
                            id, crate::types::model::OrderOp::Place, ORDER_NOT_FOUND_ERROR_CODE, refusal.clone(),
                        );
                        report_uncertain(context, shared, event_tx, id);
                    }
                    continue;
                }
                let exit_side = match side {
                    Side::Buy => Side::Sell,
                    Side::Sell | Side::ShortSell => Side::Buy,
                };
                let exit_side_str = fix_side(exit_side);
                let side_str = fix_side(side);
                let qty_str = format_qty(qty);
                let symbol = context.market.symbol(instrument).to_string();
                let (sec_type_str, destination) = context.market.order_routing(instrument);
                // Where each leg is going, kept beside what it was submitted
                // as, for the reason the single submit keeps it: the replace
                // restates the destination and reads it from the slot, which
                // the contract's own subscription writes too. Recorded for the
                // one and not for the three, a bracket leg replaced after a
                // watch was opened on the contract named somewhere the leg had
                // never been working.
                // What the contract is denominated in, not what most of them
                // happen to be.
                for id in [parent_id, tp_id, sl_id] {
                    context
                        .order_destination
                        .insert(id, (sec_type_str.clone(), destination.clone()));
                }
                let currency = currency_for(context, shared, instrument);
                // Versioned ClOrdIDs like every other submit path: a cancel or
                // replace that has seen no echo yet computes `{id}.{ver}` for
                // OrigClOrdID, and a bare id would not match. The ids are freshly
                // allocated, so the version is 0. Tag 6107 below reads the same
                // string, which is the form `send_order_ex` sends a parent link on.
                let parent_str = format!("{parent_id}.0");
                let tp_str = format!("{tp_id}.0");
                let sl_str = format!("{sl_id}.0");
                let entry_str = format_price(entry_price);
                let tp_price_str = format_price(take_profit);
                let sl_price_str = format_price(stop_loss);
                let oca_group = format!("OCA_{parent_id}");
                // Contract identity tags on every leg. A symbol alone names an
                // option or future family, which the venue answers as ambiguous
                // or unknown. Built before the tracking below borrows the
                // context mutably, then appended to each leg.
                let identity: Vec<(u32, String)> = {
                    let mut f = Vec::new();
                    push_contract_identity(&mut f, context, instrument, None);
                    f
                };
                let identity: Vec<(u32, &str)> =
                    identity.iter().map(|(t, v)| (*t, v.as_str())).collect();

                // 1. Parent order: limit entry
                context.insert_order(crate::types::Order::new(
                    parent_id,
                    instrument,
                    side,
                    qty,
                    entry_price,
                    b'2',
                    b'0',
                    0,
                ));
                // Each leg is recorded as it was placed, as the ordinary submit
                // records its own. A replace is a full statement of the order
                // and restates from this record, so a leg placed without one
                // was replaced with no group and no parent: the venue took the
                // replacement as stated, and the leg left its bracket -- a
                // stop that no longer cancels when its take-profit fills.
                let bracket_leg = |kind: crate::types::OrderKind, linked: bool| {
                    Box::new(crate::types::OrderSpec {
                        kind,
                        attrs: if linked {
                            crate::types::OrderAttrs {
                                parent_id,
                                oca_group_str: oca_group.clone(),
                                // The gateway's default, which is what the
                                // messages below state.
                                oca_type: 3,
                                ..Default::default()
                            }
                        } else {
                            Default::default()
                        },
                    })
                };
                context.record_placement(
                    parent_id,
                    bracket_leg(crate::types::OrderKind::Limit { price: entry_price }, false),
                );
                let now = chrono_free_timestamp();
                let mut parent_fields = vec![
                    (fix::TAG_MSG_TYPE, fix::MSG_NEW_ORDER),
                    (fix::TAG_SENDING_TIME, &now),
                    (11, &parent_str),
                    (1, account_id),
                    (55, &symbol),
                    (54, side_str),
                    (38, &qty_str),
                    (40, "2"), // Limit
                    (44, &entry_str),
                    (59, "0"), // DAY
                    (60, &now),
                    (167, &sec_type_str),
                    (100, &destination),
                    (6210, &destination),
                    (15, &currency),
                    (204, CUSTOMER),
                    (6122, origin_code(0)),
                ];
                parent_fields.extend_from_slice(&identity);
                let parent_sent = conn.send_fix(&parent_fields);

                // 2. Take-profit child: limit exit, linked to parent, in OCA group
                context.insert_order(crate::types::Order::new(
                    tp_id,
                    instrument,
                    exit_side,
                    qty,
                    take_profit,
                    b'2',
                    b'1',
                    0,
                ));
                context.record_placement(
                    tp_id,
                    bracket_leg(crate::types::OrderKind::Limit { price: take_profit }, true),
                );
                let now = chrono_free_timestamp();
                let mut tp_fields = vec![
                    (fix::TAG_MSG_TYPE, fix::MSG_NEW_ORDER),
                    (fix::TAG_SENDING_TIME, &now),
                    (11, &tp_str),
                    (1, account_id),
                    (55, &symbol),
                    (54, exit_side_str),
                    (38, &qty_str),
                    (40, "2"), // Limit
                    (44, &tp_price_str),
                    (59, "1"), // GTC
                    (60, &now),
                    (167, &sec_type_str),
                    (100, &destination),
                    (6210, &destination),
                    (15, &currency),
                    (204, CUSTOMER),
                    (6122, origin_code(0)),
                    (6107, &parent_str),            // ParentOrderID
                    (583, &oca_group),              // OCAGroup
                    (6209, "ReduceOnFillNonBlock"), // OCA type: gateway default 3
                ];
                tp_fields.extend_from_slice(&identity);
                let tp_sent = conn.send_fix(&tp_fields);

                // 3. Stop-loss child: stop exit, linked to parent, in OCA group
                context.insert_order(crate::types::Order::new(
                    sl_id, instrument, exit_side, qty, stop_loss, b'3', b'1', stop_loss,
                ));
                context.record_placement(
                    sl_id,
                    bracket_leg(crate::types::OrderKind::Stop { stop_price: stop_loss }, true),
                );
                let now = chrono_free_timestamp();
                // The legs go out as three messages and the arm reports one
                // outcome. Reporting only the last meant a parent that never
                // left was silence, with two children tracked against it.
                let mut sl_fields = vec![
                    (fix::TAG_MSG_TYPE, fix::MSG_NEW_ORDER),
                    (fix::TAG_SENDING_TIME, &now),
                    (11, &sl_str),
                    (1, account_id),
                    (55, &symbol),
                    (54, exit_side_str),
                    (38, &qty_str),
                    (40, "3"), // Stop
                    (99, &sl_price_str),
                    (59, "1"), // GTC
                    (60, &now),
                    (167, &sec_type_str),
                    (100, &destination),
                    (6210, &destination),
                    (15, &currency),
                    (204, CUSTOMER),
                    (6122, origin_code(0)),
                    (6107, &parent_str),            // ParentOrderID
                    (583, &oca_group),              // OCAGroup
                    (6209, "ReduceOnFillNonBlock"), // OCA type: gateway default 3
                ];
                sl_fields.extend_from_slice(&identity);
                parent_sent.and(tp_sent).and(conn.send_fix(&sl_fields))
            }
            OrderRequest::Cancel { order_id, stated } => {
                let result = send_cancel(conn, context, shared, account_id, order_id, &stated);
                if result.is_ok() {
                    synthesize_pending_cancel(context, shared, order_id, event_tx);
                }
                result
            }
            OrderRequest::CancelAll { stated, .. } | OrderRequest::GlobalCancel { stated, .. } => {
                let open_ids = cancel_ids.unwrap_or_default();
                // One outcome per order. `CancelAll` carries no order id of
                // its own, so a single result for the set cannot name the order
                // whose cancel failed.
                //
                // The first failure ends the loop: a failed write abandons the
                // transport, so the remaining cancels cannot leave and are
                // marked `Uncertain` with it.
                let mut failed = false;
                for oid in open_ids {
                    if failed {
                        report_uncertain(context, shared, event_tx, oid);
                        continue;
                    }
                    match send_cancel(conn, context, shared, account_id, oid, &stated) {
                        Ok(()) => synthesize_pending_cancel(context, shared, oid, event_tx),
                        Err(e) => {
                            log::error!(
                                "Failed to cancel order {oid}: {e} — its state is not known",
                            );
                            report_uncertain(context, shared, event_tx, oid);
                            failed = true;
                        }
                    }
                }
                Ok(())
            }
            OrderRequest::Modify {
                order_id,
                price,
                qty,
                outside_rth,
                ord_type,
                tif,
                stop_price,
                spec: stated,
            } => {
                // A replace states the whole order, so an untracked order has
                // nothing to restate. Refused rather than sent under defaults
                // that name no order the venue holds.
                let Some(orig) = context.order(order_id).copied() else {
                    log::warn!(
                        "order {order_id} cannot be replaced: this session holds no record \
                         of it, and a replace states the whole order",
                    );
                    shared.orders.push_order_inactive(
                        order_id,
                        crate::types::model::OrderOp::Modify,
                        ORDER_NOT_FOUND_ERROR_CODE,
                        format!("no order {order_id} is tracked here, so it cannot be replaced"),
                    );
                    the_change_did_not_go(order_id, 0, None, context, shared, event_tx);
                    continue;
                };
                // What the venue is known to be working, before this replace
                // is merged onto it. A refusal puts this back, so it is taken
                // first: taken after the merge it was the attempted terms, and
                // a rejected replace left the record holding the terms the
                // venue had just refused.
                let accepted = context.submitted.get(&order_id).cloned();
                // Whether the caller stated the order whole, as both surfaces
                // do. Then the statement is the order: its type, its prices
                // and everything it carries go out as its own placement would
                // state them, whatever type the order was working as.
                let statement_given = stated.is_some();
                // The venue merges what a replace states onto the order it is
                // already working and sends the result. It does not take the
                // caller's statement whole: an attribute the caller states
                // nothing for keeps the value the order already has, and some
                // of them cannot be changed by a replace at all.
                if let Some(stated) = stated {
                    match context.submitted.get_mut(&order_id) {
                        Some(resting) => merge_statement(resting, *stated),
                        None => {
                            let mut stated = stated;
                            if let Some(held) = shared.orders.attached_order_metadata(order_id) {
                                stated.attrs.keep_attached(Some(&crate::types::AttachedAttrs {
                                    family_key: held.family_key.unwrap_or_default(),
                                    parent: held.parent.unwrap_or_default(),
                                    use_parent_price: held.use_parent_price.unwrap_or(false),
                                    profit_offset: held.profit_offset,
                                    api_identity: held.api_order_id.zip(held.api_client_id),
                                    ..Default::default()
                                }));
                            }
                            context.submitted.insert(order_id, stated);
                        }
                    }
                }
                let spec = context.submitted.get(&order_id).cloned();
                // A trail rides on tag 211, restated from the record of the
                // order as it was placed. An order this session did not place
                // has no such record — an order the venue replayed at connect,
                // say — so the replace would go out without the field that
                // defines it and be refused naming it. Said here, where the
                // caller hears it, rather than sent and refused.
                if spec.is_none() && replace_needs_the_placed_record(orig.ord_type) {
                    shared.orders.push_order_inactive(
                        order_id,
                        crate::types::model::OrderOp::Modify,
                        ORDER_NOT_FOUND_ERROR_CODE,
                        format!(
                            "order {order_id} was not placed by this session, so the offset \
                             that defines it cannot be restated; withdraw it and place a \
                             new order",
                        ),
                    );
                    the_change_did_not_go(
                        order_id, orig.instrument, Some(orig.status), context, shared, event_tx,
                    );
                    continue;
                }
                // Whether the resting order was placed with tag 6433 set.
                let was_outside_rth = spec.as_ref().is_some_and(|s| s.attrs.outside_rth);
                let trail_as_t = trail_as_t(shared);
                // The shape the caller stated, where they stated one.
                let stated_shape = spec
                    .as_deref()
                    .filter(|_| statement_given)
                    .map(|spec| spec.kind.clone());
                // What the replace states. A zero field states nothing, so the
                // resting order's value stays in force. The fields a caller
                // changed are stated, so a change to the order type, the
                // time-in-force or the trigger is not accepted and dropped.
                // Whether the caller named the type, kept before the fallback
                // below overwrites it. A trigger on the request only means one
                // when the replace also states what it is replacing into.
                let ord_type_stated = ord_type != 0;
                let ord_type = match stated_shape.as_ref() {
                    Some(kind) => tracked_shape(kind).0,
                    None if ord_type != 0 => ord_type,
                    None => orig.ord_type,
                };
                let tif = if tif != 0 { tif } else { orig.tif };
                // Neither price is snapped to the tick grid; see the submit
                // path above.
                // Which tag each price belongs on depends on the order type,
                // and the answer is needed twice: once for what the engine
                // records, once for what goes on the wire. A replacement that
                // recorded the old trigger would leave the next modify
                // restating a price this one just moved.
                let trigger_only = is_trigger_only(ord_type);
                let orig_stop = orig.stop_price;
                let type_changed = orig.ord_type != ord_type;
                // A two-legged type carries a trigger on tag 99 when it has
                // one: either the resting order had one, or this replace states
                // both the type and the trigger. Every other type keeps the
                // shape it had, so a trigger on the request cannot become a tag
                // 99 for a limit order.
                //
                // A pegged, relative or snap order holds its offset in
                // `stop_price`, and a replace naming a new one restates it
                // below.
                let carries_trigger = trigger_only
                    || (matches!(ord_type, b'4' | crate::types::ORD_LIT)
                        && if ord_type_stated {
                            // The replace names the type, so only what it also
                            // states belongs to it. The resting trigger was the
                            // old type's: carrying it into a market-to-limit
                            // turns the order into limit-if-touched.
                            stop_price != 0
                        } else {
                            orig_stop != 0
                        });
                let new_stop = if trigger_only && stop_price == 0 {
                    price
                } else if carries_trigger && stop_price != 0 {
                    stop_price
                } else if stop_price != 0 && orig_stop != 0 && !type_changed {
                    // A pegged, relative or snap order holds its offset here,
                    // and the venue states the offset it holds on this tag.
                    // The shape restated below carries the caller's new offset
                    // on the peg tag, and this has to agree with it: measured,
                    // a relative order replaced naming a new offset went out
                    // stating the old one on both, and the venue kept it.
                    stop_price
                } else if type_changed && !carries_trigger {
                    // The replace moved the order to a type with no trigger.
                    // The resting order's must not ride along on tag 99 —
                    // a limit order does not have one.
                    0
                } else {
                    orig_stop
                };

                // Named before the record is restated. Names run out at the
                // width of the counter, and a modification that cannot be
                // named has to leave the order standing as the venue holds
                // it — not half-replaced here under a name the venue has
                // already seen, which is how the same terms go out twice.
                let prev_ver = *context.modify_versions.get(&order_id).unwrap_or(&0);
                let Some(new_ver) = prev_ver.checked_add(1) else {
                    shared.orders.push_order_inactive(
                        order_id,
                        crate::types::model::OrderOp::Modify,
                        ORDER_MESSAGE_ERROR_CODE,
                        format!(
                            "order {order_id} has been replaced as many times as it can be \
                             named; withdraw it and place a new order",
                        ),
                    );
                    the_change_did_not_go(
                        order_id, orig.instrument, Some(orig.status), context, shared, event_tx,
                    );
                    continue;
                };
                {
                    // The moved trigger is recorded too: a replacement that
                    // kept the old one would leave the next modify restating a
                    // price this one just moved. A stated shape is recorded as
                    // its own placement would be.
                    let (record_price, record_stop) = match stated_shape.as_ref() {
                        Some(kind) => {
                            let (_, price, stop) = tracked_shape(kind);
                            (price, stop)
                        }
                        None => (price, new_stop),
                    };
                    let mut replaced = crate::types::Order::new(
                        order_id,
                        orig.instrument,
                        orig.side,
                        qty,
                        record_price,
                        ord_type,
                        tif,
                        record_stop,
                    );
                    // A replace restates the order's terms, not its history.
                    // `filled` and `PendingReplace` carry forward: the next
                    // execution report reconciles its cumulative quantity
                    // against `filled`.
                    replaced.filled = orig.filled;
                    replaced.status = OrderStatus::PendingReplace;
                    context.insert_order(replaced);
                }
                // The attempt is recorded before the venue answers, so keep
                // what the venue is known to hold against that answer: a
                // refusal arrives later, as a message of its own, and puts
                // this back in the record's place. The write-failure restore
                // below does the same off its own copy.
                // Versioned ClOrdID chaining: orderId.0 → .1 → .2
                let clord_str = format!("{order_id}.{new_ver}");
                // OrigClOrdID matches whatever the server last recorded for
                // this order (which may pre-date the versioned scheme —).
                let orig_clord = context
                    .last_clord
                    .get(&order_id)
                    .cloned()
                    .unwrap_or_else(|| format!("{order_id}.{prev_ver}"));
                // Kept under the revision it belongs to. Revisions overlap —
                // the venue takes a second before it has answered the first —
                // and one fallback per order had the answer to one revision
                // spend or restore another's. The name the venue holds is kept
                // with the terms: the line below writes the attempt's own over
                // it ahead of the answer, and a refusal has to put both back.
                context.pre_replace.insert(
                    (order_id, new_ver),
                    (orig, orig_clord.clone(), accepted.clone()),
                );
                context.modify_versions.insert(order_id, new_ver);
                // Pre-seed `last_clord` with the id about to be emitted, so a
                // subsequent cancel before the modify-ack still references the
                // right version.
                context.last_clord.insert(order_id, clord_str.clone());

                let qty_str = format_qty(qty);
                let price_str = format_price(price);
                let now = chrono_free_timestamp();
                let side_str = fix_side(orig.side);
                let symbol = context.market.symbol(orig.instrument).to_string();
                // A replace names the contract by its local symbol (tag 6035),
                // which equals the symbol for a stock and differs for anything
                // with an expiry or a strike. Naming
                // the family there says nothing about which member is being
                // replaced.
                // A contract an expiry, a strike or a right names is not
                // named by its symbol, and standing the symbol in for its local
                // symbol states the family where the venue is being told which
                // member is being replaced. Only a contract named by its symbol
                // alone has the two the same; anything else drops the tag below
                // rather than send a name that belongs to something other than
                // the order being replaced.
                let identity = context.market.order_identity(orig.instrument);
                let named_by_symbol = identity.as_ref().is_none_or(|id| {
                    id.expiry.is_empty()
                        && id.right.is_empty()
                        && (id.strike.is_empty()
                            || id.strike.parse::<f64>().is_ok_and(|s| s <= 0.0))
                });
                let local_symbol = identity
                    .map(|id| id.local_symbol)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| if named_by_symbol { symbol.clone() } else { String::new() });
                // What this order went out on, not what the slot says now. The
                // slot's routing is written by the contract's subscription as
                // well, so a watch opened on another venue since the order was
                // placed moved it under the resting order. An order this
                // session did not place — replayed at connect, or named by the
                // venue — has no record of its own, and the slot is the only
                // answer for it.
                let (sec_type_str, destination) = context
                    .order_destination
                    .get(&order_id)
                    .cloned()
                    .unwrap_or_else(|| context.market.order_routing(orig.instrument));
                let ord_type_str = crate::types::ord_type_fix_str(ord_type).to_string();
                // An order recovered without a stated time-in-force has none to
                // restate. Tag 59 carries a real instruction on a replace, so a
                // guess here would change what the venue holds. Omitted
                // instead, leaving the resting order's value in force.
                let tif_str = std::str::from_utf8(&[tif]).unwrap_or("0").to_string();
                let con_id_str = spec.as_ref()
                    .and_then(|placed| placed.attrs.attached.as_ref()?.contract_id)
                    .or_else(|| context.market.con_id(orig.instrument))
                    .map(|c| c.to_string())
                    .unwrap_or_default();

                // Lean modify message — omit identity tags (6121, 6119, 231, 15, 204)
                let mut fields: Vec<(u32, &str)> = vec![
                    (fix::TAG_MSG_TYPE, fix::MSG_ORDER_REPLACE),
                    (fix::TAG_SENDING_TIME, &now),
                    (11, &clord_str),  // ClOrdID (versioned)
                    (41, &orig_clord), // OrigClOrdID (previous version)
                ];
                // Each price goes to the tag its order type uses, which is
                // the tag the type's own submit states it on. A type with no
                // limit leg sends no tag 44 at all: a trigger-only type states
                // its price on 99, and a market, trailing or on-close order
                // states none. Stating one as zero is refused —
                // "Invalid value in field # 44". A stated shape states its own
                // prices below, as its placement does.
                if stated_shape.is_none() && states_a_limit_price(ord_type) {
                    fields.push((44, &price_str)); // Price
                }
                // The account and the originator the order went out with.
                // Tag 6122 was accepted on a replace as `c`, confirmed against
                // a live session, which is a customer's order.
                let account = spec
                    .as_deref()
                    .map_or(account_id, |spec| account_for(&spec.attrs, account_id))
                    .to_string();
                fields.push((1, &account)); // Account
                fields.push((6122, origin_code(spec.as_deref().map_or(0, |s| s.attrs.origin))));
                // OutsideRTH, from the order the caller resubmitted rather than
                // hard-coded: the tracked record cannot express it, and asserting
                // 1 unconditionally opted every modified order into the extended
                // session. Same position it held in the capture.
                if outside_rth {
                    fields.push((6433, "1"));
                } else if was_outside_rth {
                    // Omitting tag 6433 on a replace leaves the resting
                    // order's value in force. No value that clears it has been
                    // observed, so the caller is told the flag is unchanged.
                    log::warn!(
                        "order {order_id} was placed for the extended session and the \
                         replacement does not ask for it; this client states no value \
                         that clears it, so the order goes on working outside regular hours",
                    );
                }
                let rest: [(u32, &str); 12] = [
                    (100, &destination),  // where the resting order is working
                    (6210, &destination), // and its second statement
                    (38, &qty_str),       // OrderQty
                    (54, side_str),       // Side
                    (40, &ord_type_str),  // OrdType
                    (55, &symbol),        // Symbol
                    (167, &sec_type_str), // SecurityType
                    (6035, &local_symbol), // the contract, not the family
                    (59, &tif_str),       // TIF — dropped below when unstated
                    (6008, &con_id_str),  // ConId
                    (6088, "Socket"),     // Connection type
                    (6211, ""),           // Empty where no alert placed it
                ];
                fields.extend(rest);
                if tif == crate::types::TIF_UNSTATED {
                    fields.retain(|(tag, _)| *tag != 59);
                }
                if local_symbol.is_empty() {
                    fields.retain(|(tag, _)| *tag != 6035);
                }
                // The trigger the caller moved, or the one the order already had.
                // A stated shape states its own, as its placement does.
                let stop_str;
                if stated_shape.is_none() && new_stop != 0 {
                    stop_str = format_price(new_stop);
                    fields.push((99, &stop_str));
                }
                // The gateway takes a replace as a full statement of the order,
                // not a difference against the resting one. This stated the
                // identity, the price and the quantity and stopped, so a
                // replaced order came back without the algo, the all-or-none
                // instruction, the good-till date or anything else it was
                // placed with — accepted, acknowledged, and quietly a different
                // order. What the submit said is restated here.
                let mut attr_fields: Vec<(u32, String)> = Vec::new();
                let mut restated_type = None;
                let mut type_named: Option<String> = None;
                // The record describes the order as it was PLACED, and a
                // replace never rewrites it. So everything restated from it —
                // the type's own companions, the execution instruction it
                // carries, the attributes it was placed with — belongs to that
                // type and to no other, and is restated only where the replace
                // states that type. Compared against the record rather than
                // against the resting order: an order converted once and
                // replaced again matches the resting type while the record
                // still describes the type before the conversion.
                //
                // Restated onto the wrong type it states the old type's prices
                // and instructions under the new type's name — a limit replaced
                // as a market keeping tag 44, a trailing stop replaced as a
                // market keeping 18=a, a midpoint peg writing its own tag 40
                // over the one the caller asked for. A replace that changes the
                // type with no statement of the order states the lean message
                // alone, which describes the type it is asking for whole.
                //
                // Compared as the venue names the type, not as this client
                // holds it. A reconnect replays the order and records the type
                // the wire spelled, while the record beside it still holds the
                // discriminant it was placed under — two names for one type,
                // and a pegged order replaced after a reconnect lost its peg to
                // the difference.
                //
                // A stated shape is the caller's own statement of the order,
                // and always describes the type it asks for: a change of type
                // with one states the new type whole, with everything the
                // order carries, as a gateway's replace does.
                let restating_the_record = statement_given
                    || spec.as_deref().is_some_and(|spec| {
                        crate::types::ord_type_fix_str(ord_type)
                            == crate::types::ord_type_fix_str(tracked_shape(&spec.kind).0)
                    });
                if let Some(spec) = spec.as_deref().filter(|_| restating_the_record) {
                    // A trailing stop carries its trail on tag 211, and a
                    // replace that left it out was refused naming the field —
                    // "Message must contain field # 211".
                    //
                    // Restated from the record alone it carries the price the
                    // order was placed with, so a caller naming a new one was
                    // answered as though it took while the venue kept the old:
                    // measured, a replace naming a trail of nine went out as
                    // `99=5 211=5`, and the venue accepted it. What the caller
                    // named goes on the wire instead.
                    let restated = restate_with(&spec.kind, price, stop_price);
                    // And recorded, so the next replace restates what this
                    // one moved to. Read off the placement alone, a replace
                    // that moved the quantity after one that moved an offset
                    // wrote the placed offset back onto the peg tag beside a
                    // trigger tag carrying the moved one — and the venue reads
                    // the peg tag.
                    if let Some(placed) = context.submitted.get_mut(&order_id) {
                        placed.kind = restated.clone();
                    }
                    push_type_and_prices(&mut attr_fields, &restated, trail_as_t);
                    // The name the shape goes out under is the name its own
                    // placement states, so the lean message states that one.
                    type_named = attr_fields
                        .iter()
                        .find(|(tag, _)| *tag == 40)
                        .map(|(_, name)| name.clone());
                    restated_type = push_order_attrs(
                        &mut attr_fields,
                        &spec.attrs,
                        &restated,
                        orig.side,
                        exec_inst_for(&spec.kind, trail_as_t),
                        parent_clord(context, &spec.attrs),
                    );
                    // Stated once. The lean message already names these, and the
                    // gateway reads a repeated tag as a second statement of it.
                    let stated: Vec<u32> = fields.iter().map(|(t, _)| *t).collect();
                    attr_fields.retain(|(tag, _)| !stated.contains(tag));
                }
                // The name the shape's own placement states, unless the
                // attributes settle on an order type of their own over it:
                // the lean message above states the continuous form. Restated
                // here, where the tag it names actually lives.
                if let Some(named) = restated_type.or(type_named.as_deref()) {
                    for (tag, value) in fields.iter_mut() {
                        if *tag == 40 {
                            *value = named;
                        }
                    }
                }
                fields.extend(attr_fields.iter().map(|(t, v)| (*t, v.as_str())));
                note_sent(shared, &fields);
                conn.send_fix(&fields)
            }
        };
        match result {
            Ok(()) => {
                hb.last_ccp_sent = Instant::now();
                // Recorded because silence has two causes and they look the
                // same from outside: the venue answering nothing, and this
                // client never having asked.
                //
                // Every id this request put on the wire, not the one it is
                // addressed by: a bracket writes three, and the two that were
                // not recorded read afterwards as orders nobody here placed —
                // so a bust or a correction for one of them was filed as
                // history, took nothing back, and left the position it was
                // undoing where it was.
                for id in &written {
                    shared.orders.note_the_order_went_out(*id);
                }
            }
            Err(e) => {
                // The caller is told, which is the whole of — it was
                // the silence that left a phantom position. What it is told is
                // that the state is not known: a write reporting failure may
                // already have put the frame on the wire, as the transport
                // says of TLS in as many words, so calling this a rejection
                // invited a resubmission of an order that may be working.
                //
                // Nothing is discarded and nothing is rolled back. The failure
                // abandons the transport, the reconnect that follows brings the
                // server's own account of what it holds, and `last_clord` is
                // re-recorded from that echo. Where the recovery accounts for
                // none of it, the sweep says so rather than this guessing.
                log::error!("Failed to send order {oid}: {e} — its state is not known");
                // Restore the last state the venue is known to hold. An
                // attempt the venue did not accept is not the order's own
                // values, and hydration must not read it as such. The replace
                // path alone writes one, and it carries one id.
                if restating
                    && oid != 0
                    && let Some(prior) = before
                {
                    context.insert_order(prior);
                    // The attempt never reached the wire, so the fallback kept
                    // against a refusal goes with it — the revision it was
                    // recorded under is the one this send was building.
                    let ver = *context.modify_versions.get(&oid).unwrap_or(&0);
                    if let Some((_, _, Some(record))) = context.pre_replace.remove(&(oid, ver))
                        && let Some(placed) = context.submitted.get_mut(&oid)
                    {
                        *placed = record;
                    }
                }
                // Every leg is marked, not just the one the outcome was
                // reported under. A bracket's children are sent whatever the
                // parent returns, so leaving them working states something the
                // wire never confirmed — an entry with exits that may not
                // exist, which is worse than an entry known to be uncertain.
                for id in written {
                    report_uncertain(context, shared, event_tx, id);
                }
            }
        }
    }
    context.pending_orders.requeue_front(unsent);
}

/// Everything a cancel states, in one place because the two callers stated it
/// twice and drifted.
///
/// The order it names, the account, the model, the quantity, the side and the
/// contract, and then what the withdrawal states about itself: who is
/// withdrawing it and whether a person entered it. A gateway writes those two
/// from the cancel and not from the placement — it replaces the order's own
/// with the cancel's, and removes one the cancel leaves empty — so a cancel
/// stating neither carries neither. A manual time it states is not written:
/// see `EClient::cancel_order`. No TransactTime is written anywhere in the
/// order path, so none is here.
fn send_cancel(
    conn: &mut Connection,
    context: &mut Context,
    shared: &SharedState,
    account_id: &str,
    order_id: u64,
    stated: &crate::types::model::OrderCancel,
) -> std::io::Result<()> {
    // OrigClOrdID must match exactly what the server has on record. Prefer the
    // string last observed on the wire (see — legacy orders recorded
    // without a `.{ver}` suffix won't match a computed `{id}.0`). Fall back to
    // the versioned scheme when there is no observation yet (fresh-order
    // place->immediate-cancel before the ack round-trip).
    let orig_clord = context.last_clord.get(&order_id).cloned().unwrap_or_else(|| {
        let ver = *context.modify_versions.get(&order_id).unwrap_or(&0);
        format!("{order_id}.{ver}")
    });
    let attempt = context.cancel_attempts.entry(order_id).and_modify(|n| *n += 1).or_insert(0);
    let clord_str =
        if *attempt == 0 { format!("C{order_id}") } else { format!("C{order_id}.{attempt}") };
    let now = chrono_free_timestamp();
    let tracked = context.order(order_id).copied();
    let side = tracked.map(|o| fix_side(o.side));
    // A cancel names the order, and also states what it is cancelling: the
    // quantity and the contract. Naming only the order left the venue to look
    // both up.
    // Stated as the decimal the order was sent with, so a cancel for a
    // fractional order names the quantity it is actually cancelling. An order
    // whose quantity is not tracked omits tag 38 rather than sending `38=0`,
    // which claims a cancel of nothing.
    let qty_str = tracked.filter(|o| o.qty > 0).map(|o| format_qty(o.qty).to_string());
    let con_id_str = context.submitted.get(&order_id)
        .and_then(|placed| placed.attrs.attached.as_ref()?.contract_id)
        .or_else(|| tracked.and_then(|o| context.market.con_id(o.instrument)))
        .filter(|c| *c != 0)
        .map(|c| c.to_string());
    // The model the order was placed against, restated here. The account
    // alone does not name the order where the account holds several models,
    // and the order named one when it went out.
    let model = context
        .submitted
        .get(&order_id)
        .map(|spec| spec.attrs.model_code.clone())
        .filter(|code| states_a_model(code));
    // The account the order went out for, which on a login holding several
    // is the one it named. An order the venue named at connect was placed by
    // no statement here, and is on the account the venue says it is on.
    let account = context
        .submitted
        .get(&order_id)
        .map(|spec| account_for(&spec.attrs, account_id).to_string())
        .or_else(|| {
            shared.orders.get_order_info(order_id).map(|info| info.order.account).filter(|a| !a.is_empty())
        })
        .unwrap_or_else(|| account_id.to_string());
    let mut fields = vec![
        (fix::TAG_MSG_TYPE, fix::MSG_ORDER_CANCEL),
        (fix::TAG_SENDING_TIME, &now),
        (11, &clord_str),
        (41, &orig_clord),
        (1, account.as_str()),
        (6088, "Socket"),
    ];
    if let Some(code) = model.as_deref() {
        fields.push((6700, code));
    }
    // Stated when it is known rather than defaulted: the cancel is keyed by
    // OrigClOrdID, and a guessed side is a claim about someone's order that
    // nothing here can stand behind.
    if let Some(qty) = qty_str.as_deref() {
        fields.push((38, qty));
    }
    if let Some(side) = side {
        fields.push((54, side));
    }
    if let Some(con_id) = con_id_str.as_deref() {
        fields.push((6008, con_id));
    }
    if !stated.ext_operator.is_empty() {
        fields.push((8089, stated.ext_operator.as_str()));
    }
    // `Y` or `N`, as on a placement; any other number states nothing.
    match stated.manual_order_indicator {
        1 => fields.push((1028, "Y")),
        0 => fields.push((1028, "N")),
        _ => {}
    }
    conn.send_fix(&fields)
}

/// Say so for every order still waiting when nothing will carry it.
///
/// The buffer's rule is that nothing is dropped from it, because an order
/// dropped is one nobody was told about. Stopping broke that: whatever the
/// last drain could not send was put back, and nothing drained it again — the
/// caller had been told the order was accepted, and given an id for it, and
/// then heard nothing ever again. An abandoned recovery of the trading
/// connection leaves the buffer the same way: standing orders wait in it for
/// a connection that is never coming back.
///
/// `why` states what happened, in words that read both as "`why` before this
/// order reached the venue" and as "still waiting when `why`".
pub(crate) fn refuse_what_is_left(
    context: &mut Context,
    shared: &Arc<SharedState>,
    why: &str,
) {
    let left: Vec<OrderRequest> = context.drain_pending_orders().collect();
    if left.is_empty() {
        return;
    }
    log::warn!("{} instruction(s) were still waiting when {why}", left.len());
    // How many withdrawals of the whole account were among them. Counted
    // rather than reported one by one: a caller asking for everything back
    // queues one of these per instrument the engine holds, and a refusal per
    // instrument would say the same thing fifty times.
    let mut whole_account = 0usize;
    for req in left {
        // What is said depends on what was asked for, because these do not
        // all name an order of their own. A cancel and a modify name the order
        // they act ON — one that is working at the venue — so reporting that
        // id as never placed states a live order is dead. What did not happen
        // is the instruction, and that is what is said.
        let (ids, what, op): (Vec<crate::types::OrderId>, String, crate::types::model::OrderOp) = match &req {
            OrderRequest::Cancel { .. } => (
                req.order_ids(),
                format!(
                    "{why} before this order's cancellation reached the venue, so \
                     the order stands as it was",
                ),
                crate::types::model::OrderOp::Cancel,
            ),
            OrderRequest::Modify { .. } => (
                req.order_ids(),
                format!(
                    "{why} before this order's change reached the venue, so the \
                     order stands as it was",
                ),
                crate::types::model::OrderOp::Modify,
            ),
            OrderRequest::CancelAll { .. } | OrderRequest::GlobalCancel { .. } => {
                // Names no order at all, so there is no order to report it
                // against. Said below under no request, which is how both
                // surfaces deliver what belongs to none of a caller's own.
                whole_account += 1;
                continue;
            }
            // A bracket is three orders under one request, and all three
            // were waiting. Reporting the parent alone leaves two carrying a
            // state nothing confirmed.
            //
            // Named rather than caught by a rest arm: a variant added later
            // that acts ON an existing order would inherit "never placed" and
            // tell a caller a working order is dead, which is the fault this
            // function was just repaired for. Listed, the compiler asks.
            OrderRequest::SubmitEx { .. } | OrderRequest::SubmitBracket { .. } => (
                req.order_ids(),
                format!("{why} before this order reached the venue, so it was never placed"),
                crate::types::model::OrderOp::Place,
            ),
            // Names nothing to the venue, so there is nothing to report.
        };
        for id in ids {
            if id == 0 {
                continue;
            }
            shared.orders.push_order_inactive(
                id,
                op,
                crate::error_codes::Refusal::NOT_CONNECTED,
                what.clone(),
            );
        }
    }
    if whole_account > 0 {
        log::warn!(
            "{whole_account} request(s) to cancel every order were still waiting when \
             {why}, and did not reach the venue: whatever was working still is",
        );
        shared.reference.push_historical_error(
            crate::bridge::ReferenceState::NO_REQUEST,
            crate::error_codes::Refusal::NOT_CONNECTED,
            format!(
                "{why} before the withdrawal of every order reached the venue, so \
                 whatever the account was working still is",
            ),
        );
    }
}

/// Mark an order's state unknown and announce it.
///
/// Reports the order as this session holds it. Instrument 0 is a valid
/// instrument id, so a zeroed update names another contract's order.
fn report_uncertain(
    context: &mut Context,
    shared: &Arc<SharedState>,
    event_tx: &Option<crate::engine::hot_loop::EventSink>,
    order_id: crate::types::OrderId,
) {
    if order_id == 0 {
        return;
    }
    context.set_order_status_forced(order_id, OrderStatus::Uncertain);
    let Some(order) = context.order(order_id).copied() else { return };
    let update = crate::engine::hot_loop::ccp::executions::uncertain_update(
        &order,
        shared.orders.get_order_info(order_id),
    );
    shared.orders.push_order_update(update);
    // Announced on the event channel as well as recorded.
    crate::engine::hot_loop::emit(event_tx, crate::bridge::Event::OrderUpdate(update));
}

fn fix_side(side: Side) -> &'static str {
    match side {
        Side::Buy => "1",
        Side::Sell => "2",
        Side::ShortSell => "5",
    }
}

/// Synthesize the PendingCancel phase when a cancel request goes out
/// The server acks a normal cancel with the terminal code only and never
/// sends the pending-cancel code, so without this local transition consumers
/// jump straight from Submitted to Cancelled. The server's ack, or a fill that
/// raced the cancel, then advances the status; a cancel reject restores the
/// working status through the forced setter.
fn synthesize_pending_cancel(
    context: &mut Context,
    shared: &Arc<SharedState>,
    order_id: crate::types::OrderId,
    event_tx: &Option<crate::engine::hot_loop::EventSink>,
) {
    if !context.update_order_status(order_id, OrderStatus::PendingCancel, false) {
        return; // unknown order, already terminal, or already pending-cancel
    }
    if let Some(order) = context.order(order_id).copied() {
        let update = OrderUpdate {
            order_id,
            instrument: order.instrument,
            status: OrderStatus::PendingCancel,
            filled_qty: qty_to_f64(order.filled),
            // Never below nothing. An order recovered from the venue's own
            // account of it is tracked with no quantity of its own — the
            // decimal it was submitted with lives only in the enriched record
            // — so a fill against it takes this under zero, and a quantity
            // still to trade cannot be less than none. The report that states
            // the same figure already holds this.
            remaining_qty: qty_to_f64(order.qty.saturating_sub(order.filled)),
            avg_price: 0,
            perm_id: 0,
            parent_id: 0,
            timestamp_ns: context.now_ns(),
        };
        shared.orders.push_order_update(update);
        crate::engine::hot_loop::emit(event_tx, crate::bridge::Event::OrderUpdate(update));
    }
}

/// Map the OCA type code (1..=4) to its tag 6209 wire label. 0/unset and
/// out-of-range coerce to 3 (ReduceOnFillNonBlock), the protocol default.
/// Unit a trailing amount is expressed in, on tag 6268: percent, as against
/// an absolute amount (0) or ticks (1).
pub(crate) const TRAIL_UNIT_PERCENT: u32 = 100;
/// The same unit for a trail stated as an amount.
const TRAIL_UNIT_AMOUNT: u32 = 0;

/// SecurityIDSource (tag 22) for a SecurityID carrying IB's local symbol
/// rather than a public identifier. Not one of the published sources, which
/// are single characters.
const IB_LOCAL_SYMBOL_SOURCE: &str = "101";

/// What this client calls itself when a message asks who originated it.
const ORIGINATOR: &str = "Socket";

/// Who the order is for. The venue requires it stated and this client places
/// orders for the account that authenticated it.
const CUSTOMER: &str = "0";

/// IB error code 135: the order a request names does not exist. Reported when
/// a replace arrives for an order this session does not track.
const ORDER_NOT_FOUND_ERROR_CODE: i32 = 135;
/// The venue's code for a message about an order rather than about the wire.
const ORDER_MESSAGE_ERROR_CODE: i32 = 399;

fn oca_type_str(oca_type: u8) -> &'static str {
    match oca_type {
        1 => "CancelOnFillWBlock",
        2 => "ReduceOnFillWBlock",
        4 => "ReduceOnFillWBlockFromTotal",
        _ => "ReduceOnFillNonBlock",
    }
}

/// Order types whose price *is* the trigger: they have no limit leg, so a
/// replace states the price in tag 99 and sends no tag 44 at all — which is
/// the shape their own submit has.
fn is_trigger_only(ord_type: u8) -> bool {
    matches!(ord_type, b'3' | b'J') || ord_type == crate::types::ORD_STP_PRT
}

/// The order types whose own submit states a limit price on tag 44.
///
/// Read off the submit encoder: every other type either states its price on
/// tag 99 as a trigger, or has no price to state. A peg to benchmark states
/// neither — its submit carries no tag 44, so its replace must not grow one.
fn states_a_limit_price(ord_type: u8) -> bool {
    matches!(ord_type, b'2' | b'4' | b'B')
        || ord_type == crate::types::ORD_LIT
        || ord_type == crate::types::ORD_PEG_BEST
}

/// The currency an order states for a contract (tag 15).
///
/// The caller's own, where they registered one. Otherwise the currency on the
/// venue's definition of the contract, which a caller naming it by contract id
/// alone does not supply. Empty where neither states one: the venue infers the
/// currency from the contract id, and stating a currency the caller did not
/// give describes a different listing.
fn currency_for(
    context: &Context,
    shared: &Arc<SharedState>,
    instrument: crate::types::InstrumentId,
) -> String {
    context
        .market
        .order_currency_stated(instrument)
        .or_else(|| {
            context
                .market
                .con_id(instrument)
                .and_then(|con_id| shared.reference.get_contract(con_id))
                .map(|known| known.currency)
                .filter(|c| !c.is_empty())
        })
        .unwrap_or_default()
}


/// One shared encoder for every extended order submission: the
/// order-type-specific tags come from `kind`; the TIF and the full
/// `OrderAttrs` block are emitted identically for all kinds.
/// `SubmitLimitEx`, `SubmitTrailingStopPctEx` and `SubmitEx` all route
/// through here so the attrs emission cannot drift between order types.
/// Restate the contract identity on an order for anything a symbol does not
/// name on its own. Without these an option order says nothing about which
/// strike, right or expiry it means and a future says nothing about its
/// contract month, which is why those types were refused outright rather than
/// sent under-specified.
fn push_contract_identity(
    fields: &mut Vec<(u32, String)>,
    context: &Context,
    instrument: crate::types::InstrumentId,
    attached_contract_id: Option<i64>,
) {
    // Name the contract by its id where one is known — before anything else,
    // and for every kind of contract including a stock, which has no other
    // identity to state. The terminal writes this on every order it can, and it
    // is the only field that names a contract exactly: the rest describe one
    // and leave the venue to match, which is how a description that matches
    // nothing becomes "Unknown contract" and one that matches several becomes
    // "Ambiguous".
    if let Some(con_id) = attached_contract_id.or_else(|| context.market.con_id(instrument))
        && con_id != 0
    {
        fields.push((6008, con_id.to_string()));
    }
    let Some(id) = context.market.order_identity(instrument) else {
        return;
    };
    let crate::engine::market_state::OrderIdentity {
        expiry,
        strike,
        right,
        multiplier,
        trading_class,
        local_symbol,
        currency: _,
    } = id;
    let (sec_type, _) = context.market.order_routing(instrument);
    // How an order names one contract rather than a family.
    //
    // A stock is named by its symbol alone. Every other kind is named by its
    // local symbol on SecurityID (tag 48) under source `101`, and states no
    // trading class: a trading class names a family, not one contract.
    // Options keep the encoding they already use.
    let names_itself_by_local_symbol =
        matches!(sec_type.as_str(), "FUT" | "FWD" | "IND" | "BOND" | "CFD" | "CRYPTO" | "WAR");
    // Which kinds state a maturity, and in what form. A future and a warrant
    // state the contract month and carry no maturity date at all. An option
    // states what it has always stated, because that is accepted.
    let states_contract_month = matches!(sec_type.as_str(), "FUT" | "FWD" | "WAR");
    if states_contract_month {
        // A month stated as a month is the contract month; a full date is a
        // date, and the first six characters of one are not the month it
        // belongs to. CLZ6 is the December contract and stops trading on the
        // twentieth of November, so truncating its last trade date named a
        // contract that does not exist and the venue refused the order. Each
        // form goes on the tag that carries it, which is the rule every other
        // kind of contract already follows.
        if let Some(tag) = super::ccp::maturity_tag(&expiry) {
            fields.push((tag, expiry.clone()));
        }
    } else if !names_itself_by_local_symbol
        && let Some(tag) = super::ccp::maturity_tag(&expiry)
    {
        fields.push((tag, expiry));
    }
    if names_itself_by_local_symbol {
        if !local_symbol.is_empty() {
            fields.push((48, local_symbol));
            fields.push((22, IB_LOCAL_SYMBOL_SOURCE.to_string()));
        }
    } else {
        if !trading_class.is_empty() {
            fields.push((6058, trading_class));
        }
        if !local_symbol.is_empty() {
            fields.push((6035, local_symbol));
        }
    }
    if strike.parse::<f64>().unwrap_or(0.0) > 0.0 {
        fields.push((202, strike));
    }
    // PutOrCall is a code on this wire, not the letter: Call = 1, Put = 0, the
    // same mapping the security-definition request uses. `C` names no side on
    // this tag.
    match right.to_ascii_uppercase().as_str() {
        "C" | "CALL" | "1" => fields.push((201, "1".to_string())),
        "P" | "PUT" | "0" => fields.push((201, "0".to_string())),
        "" => {}
        other => {
            log::warn!("order for an option with an unrecognised right {other:?}; omitting tag 201")
        }
    }
    if !multiplier.is_empty() {
        fields.push((231, multiplier));
    }
}

fn send_order_ex(
    conn: &mut Connection,
    context: &mut Context,
    shared: &Arc<SharedState>,
    account_id: &str,
    order_id: crate::types::OrderId,
    instrument: crate::types::InstrumentId,
    side: Side,
    qty: crate::types::Qty,
    kind: crate::types::OrderKind,
    tif: u8,
    attrs: &crate::types::OrderAttrs,
) -> std::io::Result<()> {
    let (ord_type_byte, track_price, track_stop) = tracked_shape(&kind);
    context.insert_order(crate::types::Order::new(
        order_id,
        instrument,
        side,
        qty,
        track_price,
        ord_type_byte,
        tif,
        track_stop,
    ));
    // Kept so a replace can restate it. A replace is a full statement of the
    // order: an attribute this submit set and the replace omits is lost.
    context.record_placement(
        order_id,
        Box::new(crate::types::OrderSpec { kind: kind.clone(), attrs: attrs.clone() }),
    );

    let ver = *context.modify_versions.get(&order_id).unwrap_or(&0);
    let symbol = context.market.symbol(instrument).to_string();
    if symbol.is_empty() {
        // The instrument carries no symbol. Tag 55 goes out empty and the
        // venue refuses the order.
        log::warn!(
            "order {order_id} names instrument {instrument}, which this session \
             registered under no symbol; the venue will refuse it",
        );
    }
    let (sec_type_str, destination) = context.market.order_routing(instrument);
    // Where this order is going, kept beside what it was submitted as. The
    // replace restates the destination and read it from the slot, which the
    // contract's own subscription writes too: a caller directing an order to
    // one venue and then watching that contract on another had the routing
    // moved under the resting order, and its next replace named somewhere the
    // order was never working.
    context
        .order_destination
        .insert(order_id, (sec_type_str.clone(), destination.clone()));
    let now = chrono_free_timestamp().to_string();
    let tif_byte = [tif];
    let tif_str = std::str::from_utf8(&tif_byte).unwrap_or("0");
    let trail_as_t = trail_as_t(shared);

    let mut fields: Vec<(u32, String)> = vec![
        (fix::TAG_MSG_TYPE, fix::MSG_NEW_ORDER.to_string()),
        (fix::TAG_SENDING_TIME, now.clone()),
        (11, format!("{order_id}.{ver}")),
        (1, account_for(attrs, account_id).to_string()),
        (55, symbol),
        (54, fix_side(side).to_string()),
        (38, format_qty(qty).to_string()),
    ];

    let exec_inst = exec_inst_for(&kind, trail_as_t);
    push_type_and_prices(&mut fields, &kind, trail_as_t);

    fields.push((59, tif_str.to_string()));
    fields.push((60, now));
    fields.push((167, sec_type_str.clone()));
    let attached = attrs.attached.as_deref();
    push_contract_identity(&mut fields, context, instrument, attached.and_then(|held| held.contract_id));
    if attached.is_some_and(|held| held.combo.is_some()) { fields.retain(|(tag, _)| *tag != 231); }
    if let Some((order_id, client_id)) = attached.and_then(|held| held.api_identity) {
        fields.push((6121, order_id.to_string()));
        fields.push((6119, client_id.to_string()));
    }
    // Who placed the order. Every order states it, and a cancel and a market
    // data subscription already did; a new order was the one message that left
    // it out.
    fields.push((6088, ORIGINATOR.to_string()));
    // The venue refuses an order that does not state this: "Must specify
    // Customer Or Firm flag". The reference client's order writer does not
    // emit it, so it reaches the venue some other way there, but what this
    // client sends has to satisfy the venue rather than match the writer.
    fields.push((204, CUSTOMER.to_string()));
    // Who originated it, which a gateway states on every order it sends.
    fields.push((6122, origin_code(attrs.origin).to_string()));
    // Tag 6211 names the alert an order came from, and is stated empty when
    // there is no alert. Tag 6238 is what that alert asked for, and belongs to
    // the alert: an order that came from none states it nowhere rather than
    // stating it empty.
    fields.push((6211, String::new()));
    // Routed per the instrument's own registration, as every other order type
    // is. A directed exchange is rejected for the midprice, snap and pegged
    // types: "The order type <name> is invalid for this combination of exchange
    // and security type". Confirmed against a live session.
    fields.push((100, destination.clone()));
    // Secondary routing field — the reference encoder always writes it
    // alongside the destination.
    fields.push((6210, destination));
    // Tag 15, read the same way every other submit path reads it, so a
    // contract registered by conId alone is denominated by its own definition
    // rather than defaulting to USD.
    fields.push((15, currency_for(context, shared, instrument)));

    let parent = parent_clord(context, attrs);
    if let Some(stated) = push_order_attrs(&mut fields, attrs, &kind, side, exec_inst, parent) {
        for (tag, value) in fields.iter_mut() {
            if *tag == 40 {
                *value = stated.to_string();
            }
        }
    }
    // A limit order carrying a beta or pair hedge, where the venue prices
    // hedge children at the parent's trade: a gateway states that it may,
    // unless the caller said not to. An adaptive or algo order is a limit
    // order too. Stated on a new order only; what a gateway states on a
    // replacement of one is not established here.
    if matches!(
        kind,
        crate::types::OrderKind::Limit { .. }
            | crate::types::OrderKind::Adaptive { .. }
            | crate::types::OrderKind::Algo { .. }
    ) && matches!(attrs.hedge_type, 3 | 4)
        && !attrs.dont_use_auto_price_for_hedge
        && shared.reference.enables("HDGLMT")
    {
        fields.push((8262, "1".to_string()));
    }
    // A ladder stated as a table, which a gateway states on a new order only
    // and after everything else: how many levels, then each level's price and
    // quantity, in the order the caller wrote them. The restart the table
    // would otherwise state goes, as a gateway drops it once the table reads.
    if let Some(table) = attrs.scale.as_deref().and_then(|scale| scale.table.as_deref()) {
        fields.retain(|(tag, _)| *tag != 6461);
        fields.push((6450, table.len().to_string()));
        for (quantity, price) in table {
            fields.push((6447, price.clone()));
            fields.push((6448, quantity.clone()));
        }
    }

    let refs: Vec<(u32, &str)> = fields.iter().map(|(t, s)| (*t, s.as_str())).collect();
    conn.send_fix(&refs)
}

/// The account an order goes out for: the one it names, where the session
/// holds several and the order named one, and the session's own otherwise.
fn account_for<'a>(attrs: &'a crate::types::OrderAttrs, session: &'a str) -> &'a str {
    if attrs.account.is_empty() { session } else { &attrs.account }
}

/// The character an order type contributes to tag 18, which comes first on
/// it. They were pushed from inside the type's own arm, which a replace does
/// not run — so a replaced pegged order lost the instruction that made it one.
///
/// The instructions that follow it — all-or-none, the algo marker and the
/// benchmark peg's own — are added where the rest of the order is stated.
///
/// A trailing stop the venue takes under a name of its own (`trail_as_t`)
/// needs no character to say what it is.
fn exec_inst_for(kind: &crate::types::OrderKind, trail_as_t: bool) -> String {
    use crate::types::OrderKind as K;
    match kind {
        K::Attached { exec_inst, .. } => exec_inst.as_str(),
        K::TrailingStop { .. } | K::TrailPct { .. } if !trail_as_t => "a",
        K::PegMkt { .. } => "P",
        K::PegMid { .. } => "M",
        K::Rel { .. } => "R",
        _ => "",
    }
    .to_string()
}

/// Whether the venue takes a trailing stop under its own name, `T`, rather
/// than as `P` with the trailing instruction beside it. It says so at logon.
fn trail_as_t(shared: &SharedState) -> bool {
    shared.reference.enables("TRAILSENDT")
}

/// Who originated an order, as the one character tag 6122 carries it in.
fn origin_code(origin: i32) -> &'static str {
    match origin {
        0 => "c",
        1 => "f",
        2 => "b",
        3 => "m",
        4 => "n",
        5 => "y",
        8 => "v",
        -1 => "p",
        9 => "j",
        _ => "?",
    }
}

/// Whether replacing this type needs the record of the order as it was placed.
///
/// Read off `push_type_and_prices`: these are the types that state a companion
/// beyond the type and the price the lean replace already carries — a trail on
/// 211, a peg offset, a limit-versus-trail offset. Everything else the lean
/// replace states whole.
fn replace_needs_the_placed_record(ord_type: u8) -> bool {
    // Trailing, relative and both pegs travel as `P`, and a trailing stop the
    // venue names on its own as `T`.
    ord_type == b'P'
        || ord_type == b'T'
        || matches!(
            ord_type,
            crate::types::ORD_TRAIL_LIMIT
                | crate::types::ORD_PEG_MKT
                | crate::types::ORD_PEG_MID
                | crate::types::ORD_PEG_BENCH
                | crate::types::ORD_SNAP_MKT
                | crate::types::ORD_SNAP_MID
                | crate::types::ORD_SNAP_PRI
                | crate::types::ORD_PASSV_REL
                | crate::types::ORD_PEG_BEST
        )
}

/// The placed shape with whatever the replace named written into it.
///
/// These types carry their price in the shape rather than on the lean
/// message — a trail, a peg or snap offset, a cap — so a replace restates it
/// from the record. A caller naming a new one means that number, and left out
/// of the restatement the venue keeps the old one while this session records
/// the new. Measured on a paper session, shape by shape: a trailing stop
/// replaced naming a trail of nine went out as five and was accepted; a
/// relative order, a midpoint peg, a midprice order and a trailing stop limit
/// each replaced naming a new offset, cap, trail or limit offset were read
/// back on a second session still held at the placed number.
///
/// Each number goes where the shape's own submit puts the same field. The
/// trigger the replace names is the trail, the peg offset or the snap offset,
/// which the submit reads off the auxiliary price; the price it names is the
/// cap, or a trailing stop limit's limit offset, which the submit reads off
/// the limit price. A trailing stop carries no limit price at all, so the
/// price a replace names is not its trail. The two shapes this account's
/// routes refuse at placement — pegged to market and passive relative — are
/// restated the same way, and the venue's answer to the placement is the
/// check on them.
///
/// A replace states nothing with a zero, which is how a caller moves the
/// quantity alone; that leaves the placed value in force, which is right.
fn restate_with(kind: &crate::types::OrderKind, price: i64, stop_price: i64) -> crate::types::OrderKind {
    use crate::types::OrderKind as K;
    let named = |placed: i64, asked: i64| if asked != 0 { asked } else { placed };
    match kind {
        K::TrailingStop { trail_stop_price, trail_amt } => K::TrailingStop {
            trail_stop_price: *trail_stop_price,
            trail_amt: named(*trail_amt, stop_price),
        },
        K::TrailingStopLimit { lmt_offset, trail_amt, trail_stop_price } => K::TrailingStopLimit {
            lmt_offset: named(*lmt_offset, price),
            trail_amt: named(*trail_amt, stop_price),
            trail_stop_price: *trail_stop_price,
        },
        K::Rel { offset, price_cap } => K::Rel {
            offset: named(*offset, stop_price),
            price_cap: named(*price_cap, price),
        },
        K::SnapMkt { offset } => K::SnapMkt { offset: named(*offset, stop_price) },
        K::SnapMid { offset } => K::SnapMid { offset: named(*offset, stop_price) },
        K::SnapPri { offset } => K::SnapPri { offset: named(*offset, stop_price) },
        K::PegMkt { offset, price_cap } => K::PegMkt {
            offset: named(*offset, stop_price),
            price_cap: named(*price_cap, price),
        },
        K::PegMid { offset, price_cap } => K::PegMid {
            offset: named(*offset, stop_price),
            price_cap: named(*price_cap, price),
        },
        K::PassiveRel { offset, price_cap } => K::PassiveRel {
            offset: named(*offset, stop_price),
            price_cap: named(*price_cap, price),
        },
        K::MidPrice { price_cap } => K::MidPrice { price_cap: named(*price_cap, price) },
        other => other.clone(),
    }
}

/// The order type byte, tracked price and tracked trigger a kind is held as.
///
/// One statement of it: the submit records the order this way and a replace
/// asks it which type the record describes, so the two cannot disagree about
/// what a tracked order is.
fn tracked_shape(kind: &crate::types::OrderKind) -> (u8, i64, i64) {
    use crate::types::OrderKind as K;
    match kind {
        K::Attached { ord_type, exec_inst, prices } => {
            let price = |tag| prices.iter().find(|(key, _)| *key == tag)
                .and_then(|(_, value)| value.parse::<f64>().ok())
                .map(crate::types::price_from_f64).unwrap_or(0);
            (crate::types::ord_type_from_fix(ord_type, exec_inst), price(44), price(99))
        }
        K::Market => (b'1', 0, 0),
        K::Limit { price } => (b'2', *price, 0),
        K::Stop { stop_price } => (b'3', *stop_price, *stop_price),
        K::StopLimit { price, stop_price } => (b'4', *price, *stop_price),
        K::TrailingStop { .. } => (b'P', 0, 0),
            // Each kind is tracked under the discriminant whose tag 40 the encoder
            // below writes, so a replace restates the type the submit sent. Tag 40
            // has no value `R`: a relative order is `P` with `18=R`.
        K::TrailingStopLimit { lmt_offset, .. } => (crate::types::ORD_TRAIL_LIMIT, *lmt_offset, 0),
        K::TrailPct { .. } => (b'P', 0, 0),
        K::Moc => (b'5', 0, 0),
        K::Loc { price } => (b'B', *price, 0),
        K::Mit { stop_price } => (b'J', *stop_price, *stop_price),
        K::Lit { price, stop_price } => (crate::types::ORD_LIT, *price, *stop_price),
        K::PegBench { price, .. } => (crate::types::ORD_PEG_BENCH, *price, 0),
        K::Mtl => (b'K', 0, 0),
        K::MktPrt => (b'U', 0, 0),
        K::StpPrt { stop_price } => (crate::types::ORD_STP_PRT, 0, *stop_price),
        K::MidPrice { price_cap } => (crate::types::ORD_MIDPX, *price_cap, 0),
        K::SnapMkt { offset } => (crate::types::ORD_SNAP_MKT, 0, *offset),
        K::SnapMid { offset } => (crate::types::ORD_SNAP_MID, 0, *offset),
        K::SnapPri { offset } => (crate::types::ORD_SNAP_PRI, 0, *offset),
        K::PegMkt { offset, .. } => (crate::types::ORD_PEG_MKT, 0, *offset),
        K::PegMid { offset, .. } => (crate::types::ORD_PEG_MID, 0, *offset),
        K::Rel { offset, price_cap } => (b'P', *price_cap, *offset),
        K::PassiveRel { offset, .. } => (crate::types::ORD_PASSV_REL, 0, *offset),
        K::PegBest { price } => (crate::types::ORD_PEG_BEST, *price, 0),
        K::AdjustableStop { stop_price, .. } => (b'3', 0, *stop_price),
        K::Adaptive { price, .. } | K::Algo { price, .. } => (b'2', *price, 0),
            // Tracked under the what-if marker so the response is recognised as a
            // preview; it never becomes a live order.
    }
}

/// The order type on tag 40, its price tags and the companions that type
/// needs — the shape the venue is asked for one.
///
/// One statement of it, because a replace is a full statement of the order
/// and has to reach the venue in the same shape its submit did. Written
/// only here, the two cannot drift: a replace that left out what the type
/// needs was refused naming the field — "Message must contain field # 211".
///
/// ExecInst is one field with the instructions in it separated by spaces, not
/// one field per instruction: the order type's own character, then `G` for
/// all-or-none, then `e` for an algo, then `R` for a benchmark peg. An order
/// that had a character of its own once lost its all-or-none entirely —
/// silently, on every trailing, relative, pegged and algo order.
///
/// A trailing stop is `T` where the venue takes it under that name
/// (`trail_as_t`), and `P` with its instruction otherwise.
fn push_type_and_prices(fields: &mut Vec<(u32, String)>, kind: &crate::types::OrderKind, trail_as_t: bool) {
    use crate::types::OrderKind as K;
    let trailing = if trail_as_t { "T" } else { "P" };
    match kind {
        K::Attached { ord_type, prices, .. } => {
            fields.push((40, ord_type.clone()));
            fields.extend(prices.iter().cloned());
        }
        K::Market => fields.push((40, "1".to_string())),
        K::Limit { price } => {
            fields.push((40, "2".to_string()));
            fields.push((44, format_price(*price).to_string()));
        }
        K::Stop { stop_price } => {
            fields.push((40, "3".to_string()));
            fields.push((99, format_price(*stop_price).to_string()));
        }
        K::StopLimit { price, stop_price } => {
            fields.push((40, "4".to_string()));
            fields.push((44, format_price(*price).to_string()));
            fields.push((99, format_price(*stop_price).to_string()));
        }
        K::AdjustableStop { stop_price, .. } => {
            // Base order type only. The 6257+ adjustable tags are appended after
            // the attribute block below, where the dedicated encoder this path
            // replaced put them.
            fields.push((40, "3".to_string())); // OrdType = Stop
            fields.push((99, format_price(*stop_price).to_string())); // StopPx
        }
        K::TrailingStop { trail_amt, .. } => {
            // capture: amount-based trailing stop carries
            // the trail amount in both 99 and 211 and requires 18=a.
            let t = format_price(*trail_amt).to_string();
            fields.push((40, trailing.to_string()));
            fields.push((99, t.clone()));
            fields.push((211, t));
            // The unit the trail is stated in, on every trailing order and
            // every replacement of one: an amount is nought. Left off, a
            // replace moving a percentage trail to an amount said nothing
            // about which of the two the number on 211 was.
            fields.push((6268, TRAIL_UNIT_AMOUNT.to_string()));
        }
        K::TrailingStopLimit { lmt_offset, trail_amt, .. } => {
            // capture: TRAIL LIMIT uses OrdType=TSL, no
            // tag 44, no tag 18; trail amount in both 99 and 211; 6370 is
            // the limit-vs-trail offset.
            let t = format_price(*trail_amt).to_string();
            fields.push((40, "TSL".to_string()));
            fields.push((99, t.clone()));
            fields.push((6370, format_price(*lmt_offset).to_string()));
            fields.push((211, t));
            fields.push((6268, TRAIL_UNIT_AMOUNT.to_string()));
        }
        K::TrailPct { trail_pct, .. } => {
            // The percent itself goes on 99 and 211 in decimal form (1.00 for
            // 1%). Tag 6268 is not the amount but the unit the trail is
            // expressed in — 100 for percent, 0 for an amount, 1 for ticks —
            // and it was being filled with the percent in basis points. A one
            // percent trail is 100 basis points, which is also the code for
            // percent, so the one case anybody tried was right by coincidence
            // and every other percentage sent a unit that is not a unit.
            let pct_decimal = format!("{:.2}", *trail_pct as f64 / 100.0);
            fields.push((40, trailing.to_string()));
            fields.push((99, pct_decimal.clone()));
            fields.push((211, pct_decimal));
            fields.push((6268, TRAIL_UNIT_PERCENT.to_string()));
        }
        K::Moc => fields.push((40, "5".to_string())),
        K::Loc { price } => {
            fields.push((40, "B".to_string()));
            fields.push((44, format_price(*price).to_string()));
        }
        K::Mit { stop_price } => {
            fields.push((40, "J".to_string()));
            fields.push((99, format_price(*stop_price).to_string()));
        }
        K::Lit { price, stop_price } => {
            fields.push((40, "LT".to_string()));
            fields.push((44, format_price(*price).to_string()));
            fields.push((99, format_price(*stop_price).to_string()));
        }
        K::Mtl => fields.push((40, "K".to_string())),
        K::MktPrt => fields.push((40, "U".to_string())),
        K::StpPrt { stop_price } => {
            fields.push((40, "SP".to_string()));
            fields.push((99, format_price(*stop_price).to_string()));
        }
        K::MidPrice { .. } => fields.push((40, "MIDPX".to_string())),
        // Tag 211 carries the offset and is required: without it the order is
        // rejected with "Message must contain field # 211". Confirmed against a
        // paper account for all three snap types.
        K::SnapMkt { offset } => {
            fields.push((40, "SMKT".to_string()));
            fields.push((211, format_price(*offset).to_string()));
        }
        K::SnapMid { offset } => {
            fields.push((40, "SMID".to_string()));
            fields.push((211, format_price(*offset).to_string()));
        }
        K::SnapPri { offset } => {
            fields.push((40, "SREL".to_string()));
            fields.push((211, format_price(*offset).to_string()));
        }
        // Both are OrdType "E" and are separated by ExecInst, which is what
        // ORD_PEG_MKT and ORD_PEG_MID state in types.rs. Emitting only the
        // OrdType sent the two as the same message, saying which peg neither.
        K::PegBench {
            ref_con_id,
            is_peg_decrease,
            pegged_change_amount,
            ref_change_amount,
            starting_price,
            stock_ref_price,
            ref_exchange,
            ..
        } => {
            // No tag 44: the wire shape of this type states the peg and the
            // contract it follows, and no price of its own. The starting
            // price rides tag 99, which is the only price on it.
            fields.push((40, "PB".to_string()));
            fields.push((6941, ref_con_id.to_string()));
            // The change amount carries its own direction: there is no separate
            // field saying which way it moves, so a decrease is a negative one.
            let signed = if *is_peg_decrease { -*pegged_change_amount } else { *pegged_change_amount };
            fields.push((6938, format_price(signed).to_string()));
            fields.push((6939, format_price(*ref_change_amount).to_string()));
            fields.push((6942, ref_exchange.clone()));
            fields.push((6580, format_price(*stock_ref_price).to_string()));
            fields.push((99, format_price(*starting_price).to_string()));
        }
        // The venue names these back as PegToMkt and PegToMid under "P", and
        // named them something else entirely under "E" — so an order a caller
        // asked to peg was read as another type. The offset rides 211, which
        // the shared attrs block already writes for the pegged kinds.
        K::PegMkt { .. } => {
            fields.push((40, "P".to_string()));
        }
        K::PegMid { .. } => {
            fields.push((40, "P".to_string()));
        }
        K::Rel { offset, price_cap } => {
            // capture: Relative shares OrdType=P and is
            // disambiguated by 18=R; peg offset on 211. It states no trigger
            // on 99, and the cap — where the caller set one — on 44.
            fields.push((40, "P".to_string()));
            fields.push((211, format_price(*offset).to_string()));
            if *price_cap != 0 {
                fields.push((44, format_price(*price_cap).to_string()));
            }
        }
        // A passive relative order states no instruction beside its name, so
        // the name is the whole of it here; the offset and the cap are
        // appended after the attribute block, where the pegged kinds carry
        // theirs.
        K::PassiveRel { .. } => {
            fields.push((40, "PSVR".to_string()));
        }
        K::PegBest { price } => {
            fields.push((40, "E2M".to_string()));
            fields.push((44, format_price(*price).to_string()));
        }
        K::Adaptive { price, .. } => {
            // Adaptive requires `18=e`. Without it the order is rejected with
            // "Invalid value in field # 18"; confirmed against a live session.
            // The strategy and its parameter are appended after the attribute
            // block.
            fields.push((40, "2".to_string()));
            fields.push((44, format_price(*price).to_string()));
        }
        K::Algo { price, .. } => {
            fields.push((40, "2".to_string()));
            fields.push((44, format_price(*price).to_string()));
            // Same `18=e` marker the adaptive wrapper carries. An algo order
            // without it is rejected with "Invalid value in field # 18", which
            // is also the answer to a wrong value, so the six algo types
            // were refused identically whether the field was absent or wrong.
        }
    }
}

/// A parent's full venue name, revision and all, as a child states it on
/// 6107: as a report of the child stated it, else the name the venue last
/// took the parent under, else its newest revision.
fn parent_clord(context: &Context, attrs: &crate::types::OrderAttrs) -> Option<String> {
    if let Some(stated) = attrs.attached.as_deref().map(|held| &held.parent).filter(|p| !p.is_empty()) {
        return Some(stated.clone());
    }
    let parent = attrs.parent_id;
    (parent > 0).then(|| {
        context.last_clord.get(&parent).cloned().unwrap_or_else(|| {
            format!("{parent}.{}", context.modify_versions.get(&parent).unwrap_or(&0))
        })
    })
}

/// Everything an order states beyond its identity, contract and price, in the
/// tag order the reference encoder uses. A replace restates all of it, so this
/// is shared rather than spelled out twice.
fn push_order_attrs(
    fields: &mut Vec<(u32, String)>,
    attrs: &crate::types::OrderAttrs,
    kind: &crate::types::OrderKind,
    // The side, because a short sale states where the stock comes from even
    // when the caller names no slot.
    side: Side,
    // The order type's own character, composed from its kind before this is
    // reached: the pegged, relative and trailing types each contribute one,
    // and the instructions below follow it on the same field.
    exec_inst: String,
    // The parent's full venue name, where the order has one.
    parent: Option<String>,
) -> Option<&'static str> {
    use crate::types::OrderKind as K;
    // A midpoint peg stated in two parts is a type of its own, whose name
    // carries the peg, so the peg's own character is not stated beside it.
    let unset_is_nought = |v: f64| if v == f64::MAX { 0.0 } else { v };
    let two_part_mid = matches!(kind, K::PegMid { .. })
        && unset_is_nought(attrs.mid_offset_at_whole) != 0.0
        && unset_is_nought(attrs.mid_offset_at_half) != 0.0;
    // The order type the attributes below settle on, when they settle on one
    // the caller's own tag 40 does not already state. Returned rather than
    // written over the caller's list: a replace states the type on the lean
    // message and hands this function a list of its own, so rewriting in place
    // silently did nothing there while the instruction that names the peg was
    // still dropped — leaving a bare `P`, which is three different orders.
    let mut order_type: Option<&'static str> = None;
    // Extended attributes — same tag order as the historical SubmitLimitEx
    // block.
    if attrs.display_size > 0 {
        fields.push((111, format_uint(attrs.display_size as u64).to_string()));
    }
    if attrs.min_qty > 0 {
        fields.push((110, format_uint(attrs.min_qty as u64).to_string()));
    }
    if attrs.outside_rth {
        fields.push((6433, "1".to_string()));
    }
    if attrs.hidden {
        fields.push((6135, "1".to_string()));
    }
    // Tag 168 takes the dash-joined UTC form, not the space-joined form used
    // elsewhere here.
    if attrs.good_after > 0 {
        fields.push((168, unix_to_ib_utc_dash(attrs.good_after)));
    }
    // GTD expiry: date-only -> tag 432; time-precise -> tag 126 (UTC).
    // Mutually exclusive — never both (gateway rejects both together).
    if attrs.good_till_date_ymd > 0 {
        fields.push((432, format!("{:08}", attrs.good_till_date_ymd)));
    } else if attrs.good_till > 0 {
        fields.push((126, unix_to_ib_utc_dash(attrs.good_till)));
    }
    // Day-till-cancelled: the order lives on like a good-till-cancelled one
    // and the venue stands it down at the close. Tag 59 has no byte of its
    // own for it — the life goes out as GTC and this flag is the whole of
    // the difference — so an order that stated the life without the flag
    // rested on as an ordinary GTC order that never came off the book.
    if attrs.deactivate_at_close {
        fields.push((6436, "1".to_string()));
    }
    let oca_str = if !attrs.oca_group_str.is_empty() {
        attrs.oca_group_str.clone()
    } else if attrs.oca_group > 0 {
        format!("OCA_{}", attrs.oca_group)
    } else {
        String::new()
    };
    // A preview is not placed, so it joins no group: a gateway states neither
    // the group nor its type on one. The parent it names is stated below.
    let in_group = !oca_str.is_empty();
    if in_group && !attrs.what_if {
        fields.push((583, oca_str));
        fields.push((6209, oca_type_str(attrs.oca_type).to_string()));
    }
    let attached = attrs.attached.as_deref();
    if let Some(held) = attached {
        if !held.family_key.is_empty() {
            fields.push((6531, held.family_key.clone()));
        }
        if let Some(factor) = held.ratio_factor {
            fields.push((6724, wire_number(factor)));
        }
        if held.use_parent_price {
            fields.push((6704, "1".into()));
            if let Some(offset) = held.profit_offset {
                fields.push((6446, wire_number(offset)));
            }
        }
    }
    if let Some(parent) = parent {
        fields.push((6107, parent));
    }
    if attrs.discretionary_amt > 0 {
        fields.push((9813, format_price(attrs.discretionary_amt).to_string()));
    }
    if attrs.sweep_to_fill {
        fields.push((6102, "1".to_string()));
    }
    // One field, its instructions separated by spaces and in this order: the
    // type's own character, all-or-none, the algo marker, and the benchmark
    // peg's own `R`. Run together, `aG` names no instruction the venue has.
    // All-or-none is stated only on an order in no group and carrying no
    // hedge, a preview's group included, as a gateway states it.
    let mut instructions: Vec<&str> = Vec::new();
    if !exec_inst.is_empty() && !two_part_mid {
        instructions.push(&exec_inst);
    }
    if attrs.all_or_none && !in_group && attrs.hedge_type == 0 {
        instructions.push("G");
    }
    if matches!(kind, K::Adaptive { .. } | K::Algo { .. }) {
        instructions.push("e");
    }
    if matches!(kind, K::PegBench { .. }) {
        instructions.push("R");
    }
    if !instructions.is_empty() {
        fields.push((18, instructions.join(" ")));
    }
    // Instructions the caller set. Each changes what is traded, so each goes
    // on the wire: a volatility order priced in
    // volatility, an offset the venue works from, a discretion the floor is
    // told about, the caller's own reference, and whether this opens a position
    // or closes one.
    if attrs.volatility > 0.0 {
        fields.push((9816, format!("{:.6}", attrs.volatility)));
    }
    if attrs.volatility_type > 0 {
        fields.push((6280, attrs.volatility_type.to_string()));
    }
    // What a volatility order does as the underlying moves: whether the venue
    // re-prices it, which price it references, and the band it stays inside.
    // A caller could state all four and have none of them sent.
    if attrs.seek_price_improvement {
        fields.push((6557, "1".to_string()));
    }
    // When a person entered the order by hand. A record the venue keeps, so an
    // order that states it and does not send it is recorded as something else.
    if !attrs.manual_order_time.is_empty() {
        fields.push((6532, attrs.manual_order_time.clone()));
    }
    // An error the caller has decided to send the order past anyway.
    if !attrs.advanced_error_override.is_empty() {
        fields.push((8229, attrs.advanced_error_override.clone()));
    }
    // The window an order is live in, and whether it may take liquidity.
    if !attrs.active_start_time.is_empty() {
        fields.push((6670, attrs.active_start_time.clone()));
    }
    if !attrs.active_stop_time.is_empty() {
        fields.push((6671, attrs.active_stop_time.clone()));
    }
    if attrs.post_only {
        fields.push((6605, "1".to_string()));
    }
    // Who asked for the order. A regulatory statement, not a preference, and
    // wrong by omission where it applies.
    if attrs.solicited {
        fields.push((6488, "1".to_string()));
    }
    // Tag 1028 carries the character `Y` or `N`, not a number. Any other value
    // reads as unstated. The tag is omitted when the caller states nothing.
    match attrs.manual_order_indicator {
        1 => fields.push((1028, "Y".to_string())),
        0 => fields.push((1028, "N".to_string())),
        _ => {}
    }
    if attrs.route_marketable_to_bbo {
        fields.push((8265, "1".to_string()));
    }
    if attrs.imbalance_only {
        fields.push((6737, "1".to_string()));
    }
    if attrs.allow_pre_open {
        fields.push((6524, "1".to_string()));
    }
    if attrs.ignore_open_auction {
        fields.push((6562, "1".to_string()));
    }
    if attrs.is_oms_container {
        fields.push((6406, "1".to_string()));
    }
    if !attrs.ext_operator.is_empty() {
        fields.push((8089, attrs.ext_operator.clone()));
    }
    if !attrs.customer_account.is_empty() {
        fields.push((6207, attrs.customer_account.clone()));
    }
    // Which model within the account the order trades against, on tag 6700.
    // The account names where the order goes and this names which sleeve of
    // it: an order placed for a model and sent without this trades the
    // account at large, which is a different position from the one asked for.
    if states_a_model(&attrs.model_code) {
        fields.push((6700, attrs.model_code.clone()));
    }
    // How an advisor's order is split across the accounts it is placed for.
    // Stated on the order, in this block, so a replace restates it and the
    // allocation follows the order it belongs to. Each is written only when the
    // caller named it: the venue leaves a blank value off the message entirely,
    // so an empty tag here would be a divergence rather than a no-op.
    if !attrs.fa_group.is_empty() {
        fields.push((6160, attrs.fa_group.clone()));
    }
    if !attrs.fa_method.is_empty() {
        fields.push((6159, attrs.fa_method.clone()));
    }
    if !attrs.fa_percentage.is_empty() {
        fields.push((6164, attrs.fa_percentage.clone()));
    }
    if attrs.professional_customer {
        fields.push((6636, "1".to_string()));
    }
    if attrs.ref_futures_con_id > 0 {
        fields.push((6564, attrs.ref_futures_con_id.to_string()));
    }
    // Who decided the trade and who executed it. An order that states none of
    // these where the venue expects them is reported without them.
    if !attrs.mifid2_decision_maker.is_empty() {
        fields.push((8237, attrs.mifid2_decision_maker.clone()));
    }
    if !attrs.mifid2_decision_algo.is_empty() {
        fields.push((8243, attrs.mifid2_decision_algo.clone()));
    }
    if !attrs.mifid2_execution_trader.is_empty() {
        fields.push((8254, attrs.mifid2_execution_trader.clone()));
    }
    if !attrs.mifid2_execution_algo.is_empty() {
        fields.push((8255, attrs.mifid2_execution_algo.clone()));
    }
    // Letting the venue manage the price, how long the order runs, and what it
    // competes against. A caller states these and they reached nothing.
    if attrs.use_price_mgmt_algo > 0 {
        fields.push((8339, attrs.use_price_mgmt_algo.to_string()));
    }
    if attrs.duration != i32::MAX && attrs.duration > 0 {
        fields.push((8402, attrs.duration.to_string()));
    }
    if attrs.min_compete_size > 0 {
        fields.push((8411, attrs.min_compete_size.to_string()));
    }
    if attrs.compete_against_best_offset != f64::MAX {
        fields.push((8412, format!("{:.6}", attrs.compete_against_best_offset)));
    }
    if attrs.continuous_update {
        fields.push((6275, "1".to_string()));
    }
    if attrs.reference_price_type > 0 {
        fields.push((6279, attrs.reference_price_type.to_string()));
    }
    // Nought is no bound to a gateway, which leaves it unstated.
    if attrs.stock_range_lower != f64::MAX && attrs.stock_range_lower != 0.0 {
        fields.push((6152, format!("{:.6}", attrs.stock_range_lower)));
    }
    if attrs.stock_range_upper != f64::MAX && attrs.stock_range_upper != 0.0 {
        fields.push((6153, format!("{:.6}", attrs.stock_range_upper)));
    }
    if attrs.percent_offset != f64::MAX {
        fields.push((9822, format!("{:.6}", attrs.percent_offset)));
    }
    if attrs.not_held {
        fields.push((6287, "1".to_string()));
    }
    if !attrs.order_ref.is_empty() {
        fields.push((6010, attrs.order_ref.clone()));
    }
    if !attrs.open_close.is_empty() {
        fields.push((77, attrs.open_close.clone()));
    }
    // Whether this order exercises the option it names or lapses it. The venue
    // has no exercise message: it reads the action off an ordinary order, and
    // it rides here so a replace restates it like every other attribute.
    if attrs.exercise_action != 0 {
        fields.push((6809, attrs.exercise_action.to_string()));
    }
    // The ladder. Sending the sizes and not the step, or the step and not the
    // sizes, describes no ladder at all, so an order that names one names all
    // of what it set.
    // The hedge. An order that asked for one and did not say so left the
    // position naked, which is the opposite of what it was for.
    // A combination states its legs on the order itself. There is no repeating
    // group for them and no standard leg tag: the count goes on 6079 and each
    // leg's contract, ratio and side on 6080, 6081 and 6082, with its venue,
    // position effect and short-sale slot after. The side is a flag, not the
    // letter the rest of the message uses.
    if !attrs.combo_legs.is_empty() {
        if let Some(multiplier) = attached.and_then(|held| held.combo.as_ref()?.multiplier) {
            fields.push((231, wire_number(multiplier)));
        }
        fields.push((6079, format_uint(attrs.combo_legs.len() as u64).to_string()));
        for leg in &attrs.combo_legs {
            fields.push((6080, leg.con_id.to_string()));
            fields.push((6081, format_uint(leg.ratio as u64).to_string()));
            // 1 buys the leg and 0 sells it, which is the opposite way round
            // from every other side field on the message. Sent the other way,
            // a long call spread priced at a debit came back "Guaranteed-to-Lose
            // combination orders are not allowed" — the venue had been given
            // the short spread — and reversing it previewed the long spread at
            // the margin a long spread carries.
            fields.push((6082, if leg.is_sell { "0" } else { "1" }.to_string()));
            // Empty where the leg routes with the combination rather than on a
            // venue of its own, which is what the terminal writes for SMART.
            if attached.and_then(|held| held.combo.as_ref()).is_none_or(|combo| combo.include_leg_exchanges) {
                fields.push((616, leg.exchange.clone()));
            }
            // The position effect rides on 6087, and on every leg. Stated on
            // 654, which is where the venue counts a leg's place within the
            // short-sale group and not a position at all, the instruction was
            // not read: each leg took the account's own default, so a leg
            // meant to close an option already held opened a second one
            // beside it. Left out where it is nought, a combination states
            // fewer effects than it has legs and the venue has one fewer to
            // attach to each of them — nought is a stated effect there, not
            // an absence.
            fields.push((6087, leg.open_close.to_string()));
        }
        if let Some(combo) = attached.and_then(|held| held.combo.as_ref()) {
            let mode = if matches!(combo.price_mode, 1 | 2) { 2 } else { combo.price_mode };
            fields.push((6175, mode.to_string()));
            fields.push((6134, combo.combo_type.to_string()));
            if combo.separate_delta_neutral { fields.push((6147, "1".into())); }
        }
        // Whether the combination is a short sale at all: stated once, and as
        // the one value the venue reads on it. Written per leg and carrying
        // that leg's own slot, the combination stated a number the venue puts
        // on this tag for a single order only, and stated it once per short
        // leg.
        if attrs.combo_legs.iter().any(|leg| leg.short_sale_slot == 1) {
            fields.push((6086, "1".to_string()));
        }
        // And the short-sale group, which is keyed and of a fixed width: a
        // number saying which leg each row is about, then the three fields,
        // empty for a leg that carries no short sale. Written only for the
        // legs that had one and never keyed, nothing on the wire said which
        // leg a locate or a borrow location belonged to — so on the ordinary
        // mixed combination they were stated against a leg the caller never
        // shorted.
        for (index, leg) in attrs.combo_legs.iter().enumerate() {
            fields.push((654, index.to_string()));
            if leg.short_sale_slot != 0 {
                // The value is the same for all of them — nothing a caller
                // states changes it.
                fields.push((6215, "N".to_string()));
                fields.push((6216, leg.designated_location.clone()));
                fields.push((
                    1689,
                    if leg.exempt_code != -1 { leg.exempt_code.to_string() } else { String::new() },
                ));
            } else {
                fields.push((6215, String::new()));
                fields.push((6216, String::new()));
                fields.push((1689, String::new()));
            }
        }
        // Where the caller priced the legs separately rather than pricing the
        // combination, each leg's price follows the legs, one per leg and in
        // leg order — which is the only thing that says which leg a price
        // belongs to, since nothing on the wire names it.
        //
        // A caller who priced the legs and had the prices dropped got the
        // combination worked at whatever the venue made of it, which is not
        // the order that was placed.
        if attrs.combo_legs.iter().any(|leg| leg.price.is_some()) {
            for leg in &attrs.combo_legs {
                // A leg the caller left unpriced states nothing at all. Padded
                // with an empty price tag, the venue was told that leg's price
                // — as nothing, which is a price it either reads as nought or
                // refuses, and either way a claim about the leg that was not
                // made here.
                if let Some(price) = leg.price {
                    fields.push((6879, format_price(price).to_string()));
                }
            }
        }
    }

    // Where the order clears, which is not the account it trades in. This
    // already read both of these back off the wire and sent neither.
    if !attrs.clearing_account.is_empty() {
        fields.push((440, attrs.clearing_account.clone()));
    }
    if !attrs.clearing_intent.is_empty() {
        fields.push((6419, attrs.clearing_intent.clone()));
    }

    // Lifecycle: whether the venue holds this order rather than working it,
    // whether it may work overnight, when it cancels itself, and what it takes
    // with it when it goes.
    if attrs.deactivate {
        fields.push((6521, "1".to_string()));
    }
    if attrs.deactivate_on_disconnect {
        fields.push((6661, "1".to_string()));
    }
    if attrs.include_overnight {
        fields.push((8534, "1".to_string()));
    }
    if attrs.auto_cancel_parent {
        fields.push((6965, "1".to_string()));
    }
    if attrs.min_trade_qty > 0 {
        fields.push((8415, format_uint(attrs.min_trade_qty as u64).to_string()));
    }
    if attrs.block_order {
        // Tag 9801 carries the character `Y`. A numeric `1` is not read, and
        // the tag is omitted when the flag is off.
        fields.push((9801, "Y".to_string()));
    }
    if !attrs.auto_cancel_date.is_empty() {
        fields.push((6596, attrs.auto_cancel_date.clone()));
    }

    // Who the order is for, which the venue reads as a regulatory statement.
    if !attrs.rule80a.is_empty() {
        fields.push((47, attrs.rule80a.clone()));
    }
    if attrs.post_to_ats != 0 {
        fields.push((8405, format_uint(attrs.post_to_ats as u64).to_string()));
    }

    // Short-sale handling. The location is stated only for the slot that has
    // one, which is the rule the venue applies, and the exemption rides its own
    // tag rather than the slot.
    // A short sale states where the stock comes from, and states it whatever the
    // slot is. Both fields were written only for a slot the caller had named, so
    // a plain short sale — the ordinary case, no slot stated — went out as a
    // short with no allocation on it at all, which is not a short sale this
    // venue will work. The location rides its own tag, and only for the slot
    // that has one.
    if matches!(side, Side::ShortSell) {
        let located = matches!(attrs.designated_location.as_str(), "TMBR" | "IBKR");
        fields.push((114, if located { "Y" } else { "N" }.to_string()));
        if attrs.short_sale_slot == 2 && !attrs.designated_location.is_empty() {
            fields.push((5700, attrs.designated_location.clone()));
        }
        fields.push((6086, attrs.short_sale_slot.to_string()));
        // An exemption from the short-sale rule is part of the short sale.
        // Stated outside it, an ordinary buy carried an exemption code for a
        // sale the venue had been told nothing about.
        if attrs.exempt_code != -1 {
            fields.push((1688, attrs.exempt_code.to_string()));
        }
    }
    // The hedge, as a number rather than the API's letter, with the parameter
    // the chosen kind takes: a beta or a pair ratio. Delta and FX take none.
    if attrs.hedge_type != 0 {
        fields.push((6665, attrs.hedge_type.to_string()));
        if attrs.hedge_beta != 0.0 {
            fields.push((6703, format!("{:.6}", attrs.hedge_beta)));
        }
        // The most a beta hedge may trade. Any stated value goes, nought
        // included, and the venue answers for it.
        if let Some(size) = attrs.hedge_max_size {
            fields.push((6690, size.to_string()));
        }
        if attrs.hedge_ratio != 0.0 {
            fields.push((6666, format!("{:.6}", attrs.hedge_ratio)));
        }
    }
    // Where the contract is listed, which is not where the order routes. The
    // venue reads the two separately and this stated only the routing.
    if !attrs.primary_exchange.is_empty() {
        fields.push((207, attrs.primary_exchange.clone()));
    }
    // The contract the order hedges against: which one, its delta and its
    // price. Stated on the contract rather than the order, so an order that
    // named a hedging leg still said nothing about what to hedge with.
    // Which contract to hedge with is stated on the contract and again on the
    // order, and a caller written against the reference client sets both. A
    // repeated tag reads as a second statement of the same field, so it is
    // stated once, from the contract.
    let delta_neutral_contract = attached.and_then(|held| held.combo.as_ref())
        .map(|combo| combo.delta_neutral_contract.as_ref())
        .unwrap_or(attrs.delta_neutral_contract.as_deref());
    let hedge_con_id = delta_neutral_contract
        .map(|dnc| dnc.con_id)
        .filter(|id| *id != 0)
        .or_else(|| attrs.delta_neutral.as_deref().map(|dn| dn.con_id).filter(|id| *id != 0));
    if let Some(con_id) = hedge_con_id {
        fields.push((6150, con_id.to_string()));
    }
    if let Some(dnc) = delta_neutral_contract {
        fields.push((6148, format!("{:.6}", dnc.delta)));
        fields.push((6149, format!("{:.6}", dnc.price)));
    }
    if let Some(dn) = attrs.delta_neutral.as_deref() {
        fields.push((6290, dn.order_type.clone()));
        if dn.aux_price != 0 {
            fields.push((6291, format_price(dn.aux_price).to_string()));
        }
    }
    if let Some(scale) = attrs.scale.as_deref() {
        if scale.init_level_size > 0 {
            fields.push((6403, format_uint(scale.init_level_size as u64).to_string()));
        }
        if scale.subs_level_size > 0 {
            fields.push((6445, format_uint(scale.subs_level_size as u64).to_string()));
        }
        if scale.price_increment > 0 {
            fields.push((6405, format_price(scale.price_increment).to_string()));
        }
        if scale.profit_offset > 0
            && !attached.is_some_and(|held| held.use_parent_price && held.profit_offset.is_some())
        {
            fields.push((6446, format_price(scale.profit_offset).to_string()));
        }
        if scale.price_adjust_value != 0 {
            fields.push((6527, format_price(scale.price_adjust_value).to_string()));
        }
        if scale.price_adjust_interval > 0 {
            fields.push((6526, format_uint(scale.price_adjust_interval as u64).to_string()));
        }
        if scale.auto_reset {
            fields.push((6461, "1".to_string()));
        }
        if scale.random_percent {
            fields.push((6795, "1".to_string()));
        }
        // A ladder that starts against a position already held, and a first
        // component already partly filled. Left out, the venue starts the
        // ladder from nothing and works components the caller has already had.
        if scale.init_position != 0 {
            fields.push((6485, scale.init_position.to_string()));
        }
        // How much of the first component is already filled, as a gateway
        // sends it. The venue answers "Can not contain field # 6486", and that
        // answer is the caller's to have.
        if let Some(filled) = scale.init_fill_qty {
            fields.push((6486, filled.to_string()));
        }
    }
    // The soft-dollar arrangement this order's commission goes to. Both parts
    // or neither: a tier named with nothing against it is not an arrangement.
    if !attrs.soft_dollar_tier_name.is_empty() && !attrs.soft_dollar_tier_val.is_empty() {
        fields.push((6519, attrs.soft_dollar_tier_name.clone()));
        fields.push((6520, attrs.soft_dollar_tier_val.clone()));
    }
    // The caller's own name for the algo running this order, as a gateway
    // sends it. The venue refuses it — "Invalid value in field # 8016" —
    // whether or not the order runs an algo, and that answer is the caller's.
    if !attrs.algo_id.is_empty() {
        fields.push((8016, attrs.algo_id.clone()));
    }
    // Who settles this order, where that is not the account's own.
    if !attrs.settling_firm.is_empty() {
        fields.push((6282, attrs.settling_firm.clone()));
    }
    // Whether discretion runs all the way to the limit price.
    if attrs.discretionary_up_to_limit {
        fields.push((8165, "1".to_string()));
    }
    if attrs.trigger_method > 0 {
        fields.push((6115, attrs.trigger_method.to_string()));
    }
    if attrs.cash_qty > 0 {
        // 152, not 5920. 5920 is not a tag this venue reads on an order, and
        // 152 is CashOrderQty — the same pairing all-or-none has, where 18 is
        // the tag that carries it. An order by cash amount was naming a field
        // the venue does not read.
        fields.push((152, format_price(attrs.cash_qty).to_string()));
    }
    // Condition tags:
    // 6123 conid, 6124 exchange, 6125 price, 6126 operator, 6128 cancel-on-condition,
    // 6136 list size, 6137 conjunction, 6166 strike, 6168 expiry, 6169
    // security type, 6220 multiplier, 6222 type, 6223 time, 6224 send-email,
    // 6226 email text, 6227 TWS actions, 6241 inactive, 6245 percentage,
    // 6246 execution pattern, 6263 volume, 6151 ignore-RTH, 8569 amount,
    // 6947 a type discriminator (NOT a timezone).
    //
    if !attrs.conditions.is_empty() {
        let cond_strs = build_condition_strings(&attrs.conditions);
        fields.push((6136, cond_strs[0].clone())); // first element is count
        // 6128 cancels the order when its condition fails; 6151 lets the
        // conditions ignore regular hours. Both tag numbers carry different
        // fields on other messages, so the meaning they have elsewhere does not
        // apply here. This pairing and this order are what an order message
        // takes; the other way round was tried and was wrong.
        if attrs.conditions_cancel_order {
            fields.push((6128, "1".to_string()));
        }
        if attrs.conditions_ignore_rth {
            fields.push((6151, "1".to_string()));
        }
        // Per-condition tags start at index 1, 11 strings per condition
        for i in 0..attrs.conditions.len() {
            let base = 1 + i * 11;
            fields.push((6222, cond_strs[base].clone())); // condType
            // Every slot, including the ones this condition has no use for.
            // The terminal writes only what applies, and following it here was
            // tried: it did not make the time condition acceptable, and it
            // broke the multi-condition order, which is refused for a missing
            // volume field the moment the price condition alongside it stops
            // stating an empty one. The gateway is reading these positionally.
            fields.push((6137, cond_strs[base + 1].clone())); // conjunction
            fields.push((6126, cond_strs[base + 2].clone())); // operator
            fields.push((6123, cond_strs[base + 3].clone())); // conId
            fields.push((6124, cond_strs[base + 4].clone())); // exchange
            fields.push((6127, cond_strs[base + 5].clone())); // triggerMethod
            fields.push((6125, cond_strs[base + 6].clone())); // price
            fields.push((6223, cond_strs[base + 7].clone())); // time
            fields.push((6245, cond_strs[base + 8].clone())); // percent
            fields.push((6263, cond_strs[base + 9].clone())); // volume
            fields.push((6246, cond_strs[base + 10].clone())); // execution

            // Writing the condition's own fields first and the empty ones
            // after, as the terminal does, and adding the empty 6947 it pads
            // with, changes nothing the venue does, and both are churn on a
            // path every condition already goes through. The order here stays
            // fixed.
        }
    }

    // Adjustable-stop tags last, keeping the position they held in the encoder
    // this path replaced: after 204 and the attribute block, not in among the
    // order-type tags. Values and conditions are unchanged; only the encoder
    // they come from is new.
    if let K::AdjustableStop {
        trigger_price,
        adjusted_order_type,
        adjusted_stop_price,
        adjusted_stop_limit_price,
        adjusted_trailing_amount,
        adjustable_trailing_unit,
        ..
    } = &kind
    {
        fields.push((6257, "1".to_string())); // has adjustable params
        fields.push((6261, adjusted_order_type.fix_code().to_string()));
        fields.push((6258, format_price(*trigger_price).to_string()));
        fields.push((6259, format_price(*adjusted_stop_price).to_string()));
        if *adjusted_stop_limit_price > 0 {
            fields.push((6262, format_price(*adjusted_stop_limit_price).to_string()));
        }
        // Trailing amount + unit for a Trail/TrailLimit conversion.
        if matches!(
            adjusted_order_type,
            crate::types::AdjustedOrderType::Trail | crate::types::AdjustedOrderType::TrailLimit
        ) {
            fields.push((6260, format_price(*adjusted_trailing_amount).to_string()));
            fields.push((6269, adjustable_trailing_unit.to_string()));
        }
    }

    // The optional tags each type appends last, in the position the per-type
    // encoders give them: after 204 and the attribute block, not in among the
    // order-type tags. The values and the conditions are unchanged.
    match &kind {
        K::MidPrice { price_cap } if *price_cap != 0 => {
            fields.push((44, format_price(*price_cap).to_string()));
        }
        // Tag 211 is stated even when the offset is 0. Omitting it is rejected
        // with "Invalid value in field # 44"; confirmed against a paper
        // account.
        // The offset rides the peg tag and the trigger tag both, as a gateway
        // states it for this type.
        K::PegMkt { offset, price_cap } => {
            let offset = format_price(*offset).to_string();
            fields.push((211, offset.clone()));
            fields.push((99, offset));
            if *price_cap != 0 {
                fields.push((44, format_price(*price_cap).to_string()));
            }
        }
        // The offset rides the peg tag the way a relative order's does, and
        // the trigger tag beside it, as a gateway states it for this type; the
        // cap rides the limit-price tag, and a zero cap is no cap.
        K::PassiveRel { offset, price_cap } => {
            let offset = format_price(*offset).to_string();
            fields.push((211, offset.clone()));
            fields.push((99, offset));
            if *price_cap != 0 {
                fields.push((44, format_price(*price_cap).to_string()));
            }
        }
        K::PegMid { offset, price_cap } => {
            // A midpoint peg states its offset one of two ways: as one
            // continuous number on tag 211, or as a whole-tick part on tag 8403
            // and a half-tick part on tag 8404. The two-part form has its own
            // tag 40 value and carries no peg instruction on tag 18. 0 on both
            // parts selects the continuous form.
            //
            // ponytail: the two-part form is selected on both parts being
            // non-zero. The protocol also requires each part to be an exact
            // multiple of the contract's price increment, and the two to differ
            // by exactly half of it; an off-grid pair is rejected by the venue.
            // Applying that test here would use an increment that is 0 until a
            // subscription is acknowledged, which downgrades a valid two-part
            // peg to the continuous form with no diagnostic. Add the test once
            // the increment is known before the order is built.
            let whole = unset_is_nought(attrs.mid_offset_at_whole);
            let half = unset_is_nought(attrs.mid_offset_at_half);
            fields.push((8403, format!("{whole:.6}")));
            fields.push((8404, format!("{half:.6}")));
            if two_part_mid {
                // The two-part form. The order type stated above is the one for
                // a continuous offset, so it is restated; the instruction that
                // names the peg was left off above — the type carries it.
                order_type = Some("PMID2");
            }
            fields.push((211, format_price(*offset).to_string()));
            // The worst price the peg may reach, which IBKR documents as the
            // limit-price field for these types. A zero cap is no cap, and zero
            // is not a price, so it is left off rather than stated as one.
            if *price_cap != 0 {
                fields.push((44, format_price(*price_cap).to_string()));
            }
        }
        // Stated up to the midpoint rather than against the best price, the
        // order carries the mid offsets in place of a compete offset. Stated
        // only where the caller set one: a compete-against-best order names
        // neither, and zero on both is not the same as nothing on either.
        K::PegBest { .. } => {
            if attrs.mid_offset_at_whole != f64::MAX || attrs.mid_offset_at_half != f64::MAX {
                let whole = if attrs.mid_offset_at_whole == f64::MAX { 0.0 } else { attrs.mid_offset_at_whole };
                let half = if attrs.mid_offset_at_half == f64::MAX { 0.0 } else { attrs.mid_offset_at_half };
                fields.push((8403, format!("{whole:.6}")));
                fields.push((8404, format!("{half:.6}")));
            }
        }
        // Optional initial stop trigger, carried wherever it was stated:
        // nought and below included, as a gateway carries them.
        K::TrailingStop { trail_stop_price: Some(trail_stop_price), .. }
        | K::TrailingStopLimit { trail_stop_price: Some(trail_stop_price), .. }
        | K::TrailPct { trail_stop_price: Some(trail_stop_price), .. } => {
            fields.push((6117, format_price(*trail_stop_price).to_string()));
        }
        _ => {}
    }

    // Strategy and preview tags last, in the position they held in the encoders
    // this path replaced: after 204 and the attribute block.
    match &kind {
        K::Adaptive { priority, .. } => {
            fields.push((847, "Adaptive".to_string()));
            fields.push((5957, "1".to_string()));
            fields.push((5958, "adaptivePriority".to_string()));
            fields.push((5960, priority.as_str().to_string()));
        }
        K::Algo { algo, .. } => {
            let (algo_name, mut param_strs) = build_algo_tags(algo);
            fields.push((847, algo_name.to_string()));
            // Tag 849 (maxPctVol) for the algos that use it, in the caller's
            // own spelling: a parameter is text on the wire, and what the
            // caller wrote is what goes. Written back out from a parsed
            // number, `5.0` went as `5` and `1e-05` as `0.00001`. One the
            // caller did not state is not sent; it used to go as `0`.
            let max_pct_vol = match algo {
                AlgoParams::Vwap { max_pct_vol, .. }
                | AlgoParams::ArrivalPx { max_pct_vol, .. }
                | AlgoParams::ClosePx { max_pct_vol, .. } => max_pct_vol.clone(),
                // A list forwarded as the caller wrote it still names one of
                // these strategies when they asked for one, and the tag stands
                // where it stood: read only off the modelled fields, a single
                // value this client does not fold sent the whole list down the
                // text path and took this tag off the order with it.
                AlgoParams::Named { strategy, params } if matches!(
                    strategy.to_lowercase().as_str(),
                    "vwap" | "arrivalpx" | "arrival_price" | "closepx" | "close_price",
                ) => params
                    .as_chunks::<2>().0.iter()
                    .find(|[key, _]| key == "maxPctVol")
                    .map(|[_, value]| value.clone()),
                _ => None,
            };
            if let Some(max_pct_vol) = max_pct_vol {
                fields.push((849, max_pct_vol));
                // And off the list of the rest, because it has a tag of its
                // own. Left on it, the same instruction went out twice — once
                // on its own tag and again as a parameter — and the count
                // ahead of the parameters was one larger than the parameters
                // the venue reads as generic ones.
                if let Some(at) = param_strs
                    .as_chunks::<2>().0.iter()
                    .position(|[key, _]| key == "maxPctVol")
                {
                    param_strs.drain(at * 2..at * 2 + 2);
                }
            }
            fields.push((5957, (param_strs.len() / 2).to_string()));
            // Key/value pairs: 5958=key, 5960=value, repeated.
            let (pairs, _) = param_strs.as_chunks::<2>();
            for [key, value] in pairs {
                fields.push((5958, key.clone()));
                fields.push((5960, value.clone()));
            }
        }
        _ => {}
    }
    // A preview asks about the order the caller described, so it states every
    // field that order states and this tag besides. Encoded from the type
    // alone it stated a limit price on types that carry none — refused with
    // "Invalid value in field # 44" — and no execution instruction at all,
    // which is what tells a trailing, relative or pegged order apart from the
    // three others that share its name.
    if attrs.what_if {
        fields.push((6091, "1".to_string()));
    }
    order_type
}

fn build_algo_tags(algo: &AlgoParams) -> (&str, Vec<String>) {
    // Name then value for each parameter the caller stated, in the order the
    // strategy lists them. One the caller did not state is not sent: the
    // venue's own default for it is not known here, and a value put in its
    // place — `0` for a flag, an empty time, Neutral — is a claim the caller
    // did not make. The count on 5957 is of what is sent.
    fn stated(pairs: &[(&str, Option<String>)]) -> Vec<String> {
        let mut out = Vec::with_capacity(pairs.len() * 2);
        for (key, value) in pairs {
            if let Some(value) = value {
                out.push(key.to_string());
                out.push(value.clone());
            }
        }
        out
    }
    let flag = |f: &Option<bool>| f.map(|f| if f { "1" } else { "0" }.to_string());
    let spelled = |r: &Option<RiskAversion>| r.as_ref().map(|r| r.as_str().to_string());
    match algo {
        // Carried through untouched: the venue decides which algorithms this
        // account may use, and it says so at logon.
        AlgoParams::Named { strategy, params } => (strategy.as_str(), params.clone()),
        AlgoParams::Vwap { no_take_liq, allow_past_end_time, start_time, end_time, .. } => (
            "Vwap",
            stated(&[
                ("noTakeLiq", flag(no_take_liq)),
                ("allowPastEndTime", flag(allow_past_end_time)),
                ("startTime", start_time.clone()),
                ("endTime", end_time.clone()),
            ]),
        ),
        AlgoParams::Twap { allow_past_end_time, start_time, end_time } => (
            "Twap",
            stated(&[
                ("allowPastEndTime", flag(allow_past_end_time)),
                ("startTime", start_time.clone()),
                ("endTime", end_time.clone()),
            ]),
        ),
        AlgoParams::ArrivalPx {
            risk_aversion,
            allow_past_end_time,
            force_completion,
            start_time,
            end_time,
            ..
        } => (
            "ArrivalPx",
            stated(&[
                ("riskAversion", spelled(risk_aversion)),
                ("allowPastEndTime", flag(allow_past_end_time)),
                ("forceCompletion", flag(force_completion)),
                ("startTime", start_time.clone()),
                ("endTime", end_time.clone()),
            ]),
        ),
        AlgoParams::ClosePx { risk_aversion, force_completion, start_time, .. } => (
            "ClosePx",
            stated(&[
                ("riskAversion", spelled(risk_aversion)),
                ("forceCompletion", flag(force_completion)),
                ("startTime", start_time.clone()),
            ]),
        ),
        AlgoParams::DarkIce { allow_past_end_time, display_size, start_time, end_time } => (
            "DarkIce",
            stated(&[
                ("allowPastEndTime", flag(allow_past_end_time)),
                ("displaySize", Some(display_size.clone())),
                ("startTime", start_time.clone()),
                ("endTime", end_time.clone()),
            ]),
        ),
        AlgoParams::PctVol { pct_vol, no_take_liq, start_time, end_time } => (
            "PctVol",
            stated(&[
                ("noTakeLiq", flag(no_take_liq)),
                ("pctVol", pct_vol.clone()),
                ("startTime", start_time.clone()),
                ("endTime", end_time.clone()),
            ]),
        ),
    }
}

fn build_condition_strings(conditions: &[OrderCondition]) -> Vec<String> {
    let mut out = Vec::with_capacity(1 + conditions.len() * 11);
    out.push(conditions.len().to_string());
    for (i, cond) in conditions.iter().enumerate() {
        // `a` or `o` as the caller joined this condition to the next, and
        // `n` on the last, which joins nothing whatever it holds: the
        // terminator is the position rather than a value a caller states.
        // Every condition but the last used to go as `a`, so an order joined
        // by OR reached the venue joined by AND.
        let is_last = i == conditions.len() - 1;
        let conj = if is_last { "n" } else if cond.is_conjunction_connection() { "a" } else { "o" };
        let op = |is_more: bool| if is_more { ">=" } else { "<=" };
        match cond {
            OrderCondition::Price { con_id, exchange, price, is_more, trigger_method, .. } => {
                out.push("1".into()); // condType
                out.push(conj.into()); // conjunction
                out.push(op(*is_more).into()); // operator
                out.push(con_id.to_string()); // conId
                out.push(exchange.clone()); // exchange
                out.push(trigger_method.to_string()); // triggerMethod
                out.push(format_price(*price).to_string()); // price
                out.push(String::new()); // time (unused)
                out.push(String::new()); // percent (unused)
                out.push(String::new()); // volume (unused)
                out.push(String::new()); // execution (unused)
            }
            // The type is 3, the operators are `>=` and `<=`, the
            // conjunctions are `a`/`o`/`n`, the value is `YYYYMMDD-HH:MM:SS`
            // in GMT, and a time condition carries that one field and no
            // other — not the contract, exchange, trigger method or price a
            // price condition carries, and not the timezone on tag 6947,
            // which is written only for the condition types that carry one.
            // The venue holds an order under this until its time.
            OrderCondition::Time { time, is_more, .. } => {
                out.push("3".into());
                out.push(conj.into());
                out.push(op(*is_more).into());
                out.push(String::new()); // conId (unused)
                out.push(String::new()); // exchange (unused)
                out.push(String::new()); // triggerMethod (unused)
                out.push(String::new()); // price (unused)
                out.push(time.clone()); // time
                out.push(String::new()); // percent (unused)
                out.push(String::new()); // volume (unused)
                out.push(String::new()); // execution (unused)
            }
            OrderCondition::Margin { percent, is_more, .. } => {
                out.push("4".into());
                out.push(conj.into());
                out.push(op(*is_more).into());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(percent.to_string()); // percent
                out.push(String::new());
                out.push(String::new());
            }
            OrderCondition::Execution { symbol, exchange, sec_type, .. } => {
                out.push("5".into());
                out.push(conj.into());
                out.push(String::new()); // operator (unused)
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                // SMART is a name for every exchange, and the venue spells
                // that "*" here. It is also the name this client sees a
                // routed contract under — BEST — and passing that through was
                // refused, so both spellings become the one the venue takes.
                let exch = if exchange == "SMART" || exchange == "BEST" { "*" } else { exchange.as_str() };
                out.push(format!("symbol={symbol};exchange={exch};securityType={sec_type};"));
            }
            OrderCondition::Volume { con_id, exchange, volume, is_more, .. } => {
                out.push("6".into());
                out.push(conj.into());
                out.push(op(*is_more).into());
                out.push(con_id.to_string());
                out.push(exchange.clone());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(volume.to_string()); // volume
                out.push(String::new());
            }
            OrderCondition::PercentChange { con_id, exchange, percent, is_more, .. } => {
                out.push("7".into());
                out.push(conj.into());
                out.push(op(*is_more).into());
                out.push(con_id.to_string());
                out.push(exchange.clone());
                out.push(String::new());
                out.push(String::new());
                out.push(String::new());
                out.push(format!("{percent}")); // percent
                out.push(String::new());
                out.push(String::new());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod attached_tests;
