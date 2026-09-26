//! The session's one order, as a caller reads it: each record delivered in the
//! order the engine took it in, the conflated state after the records of its
//! read, the session's last record after everything, and each order's reports
//! in the venue's order.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::tests::test_client;
use crate::api::wrapper::Wrapper;
use crate::bridge::{TickReqParams, hooks};
use crate::types::model::{
    CommissionAndFeesReport, Contract, ErrorOrigin, Execution, Order, OrderState, TickAttrib,
};
use crate::types::*;

/// Every callback these tests read, in the order it was made.
#[derive(Default)]
struct Heard(Vec<String>);

impl Heard {
    fn position_of(&self, prefix: &str) -> Option<usize> {
        self.0.iter().position(|e| e.starts_with(prefix))
    }

    fn has(&self, prefix: &str) -> bool {
        self.position_of(prefix).is_some()
    }
}

impl Wrapper for Heard {
    fn error(&mut self, req_id: i64, _error_time: i64, code: i64, message: &str, _: &str) {
        self.0.push(format!("error:{req_id}:{code}:{message}"));
    }
    fn connection_closed(&mut self) {
        self.0.push("connection_closed".into());
    }
    fn current_time(&mut self, _time: i64) {
        self.0.push("current_time".into());
    }
    fn order_status(
        &mut self,
        order_id: i64,
        status: &str,
        filled: f64,
        remaining: f64,
        avg_fill_price: f64,
        _: i64,
        _: i64,
        last_fill_price: f64,
        _: i64,
        _: &str,
        _: f64,
    ) {
        self.0.push(format!(
            "order_status:{order_id}:{status}:{filled}:{remaining}:{avg_fill_price}:{last_fill_price}"
        ));
    }
    fn open_order(&mut self, order_id: i64, _: &Contract, _: &Order, state: &OrderState) {
        self.0.push(format!("open_order:{order_id}:{}", state.status));
    }
    fn exec_details(&mut self, req_id: i64, _: &Contract, execution: &Execution) {
        self.0.push(format!(
            "exec_details:{req_id}:{}:{}:{}",
            execution.order_id, execution.shares, execution.price,
        ));
    }
    fn commission_and_fees_report(&mut self, report: &CommissionAndFeesReport) {
        self.0.push(format!("charge:{}", report.exec_id));
    }
    fn position(&mut self, _: &str, contract: &Contract, pos: f64, _: f64) {
        self.0.push(format!("position:{}:{pos}", contract.con_id));
    }
    fn position_end(&mut self) {
        self.0.push("position_end".into());
    }
    fn tick_price(&mut self, req_id: i64, tick_type: i32, price: f64, _: &TickAttrib) {
        self.0.push(format!("tick_price:{req_id}:{tick_type}:{price}"));
    }
    fn tick_req_params(&mut self, req_id: i64, _: f64, _: &str, _: i64) {
        self.0.push(format!("tick_req_params:{req_id}"));
    }
    fn update_account_value(&mut self, key: &str, value: &str, _: &str, _: &str) {
        self.0.push(format!("account:{key}:{value}"));
    }
    fn tick_option_computation(
        &mut self,
        req_id: i64,
        tick_type: i32,
        _: i32,
        _: f64,
        _: f64,
        _: f64,
        _: f64,
        _: f64,
        _: f64,
        _: f64,
        _: f64,
    ) {
        self.0.push(format!("tick_option_computation:{req_id}:{tick_type}"));
    }
    fn head_timestamp(&mut self, req_id: i64, head_timestamp: &str) {
        self.0.push(format!("head_timestamp:{req_id}:{head_timestamp}"));
    }
    fn contract_details_end(&mut self, req_id: i64) {
        self.0.push(format!("contract_details_end:{req_id}"));
    }
}

/// A limit order for `qty` shares of the contract `test_client` holds a slot
/// for.
fn limit(qty: f64, price: f64) -> Order {
    Order {
        action: "BUY".into(),
        total_quantity: qty,
        order_type: "LMT".into(),
        lmt_price: price,
        tif: "DAY".into(),
        ..Default::default()
    }
}

fn spy() -> Contract {
    Contract {
        con_id: 756733,
        symbol: "SPY".into(),
        sec_type: "STK".into(),
        exchange: "SMART".into(),
        currency: "USD".into(),
        ..Default::default()
    }
}

/// A status report with nothing else on it.
fn status(
    order_id: u64,
    status: OrderStatus,
    filled: f64,
    remaining: f64,
    avg: f64,
) -> OrderUpdate {
    OrderUpdate {
        order_id,
        instrument: 0,
        status,
        filled_qty: filled,
        remaining_qty: remaining,
        avg_price: price_from_f64(avg),
        perm_id: 0,
        parent_id: 0,
        timestamp_ns: 0,
    }
}

/// A print of `qty` at `price`, the order having filled `cum` in all at an
/// average of `avg`.
fn print(order_id: u64, qty: i64, price: f64, cum: i64, remaining: i64, avg: f64) -> Fill {
    Fill {
        instrument: 0,
        order_id,
        side: Side::Buy,
        price: price_from_f64(price),
        qty: qty_from_wire(qty),
        remaining: qty_from_wire(remaining),
        timestamp_ns: 0,
        cum_qty: qty_from_wire(cum),
        avg_price: price_from_f64(avg),
    }
}

fn holding(qty: f64) -> PositionInfo {
    PositionInfo {
        con_id: 265598,
        position: qty,
        symbol: "AAPL".into(),
        sec_type: "STK".into(),
        currency: "USD".into(),
        ..Default::default()
    }
}

// ═══════════════════════════════════════════════════════════════════
//  One order
// ═══════════════════════════════════════════════════════════════════

/// A report queued at any point of a read goes before a refusal pushed after
/// it, whatever queues the two wait in.
///
/// Read category by category, a refusal waiting in a queue drained early was
/// delivered ahead of a report in a queue drained late, though the venue had
/// said the report first: a fill of an order and then the refusal of its
/// modify arrived the other way round.
#[test]
fn a_report_queued_between_any_two_drains_goes_before_a_refusal_pushed_after_it() {
    let (client, _rx, shared) = test_client();
    let at = Rc::new(RefCell::new(0u32));
    {
        let (shared, at) = (Arc::clone(&shared), Rc::clone(&at));
        hooks::set(&hooks::BETWEEN_DRAINS, move || {
            let k = {
                let mut at = at.borrow_mut();
                *at += 1;
                *at
            };
            // Report A, in one of two queues read at different points.
            if k % 2 == 0 {
                shared.orders.push_order_update(status(
                    u64::from(1000 + k),
                    OrderStatus::Submitted,
                    0.0,
                    1.0,
                    0.0,
                ));
            } else {
                shared.reference.push_contract_details_end(3000 + k);
            }
            // And refusal B, pushed by another thread after it.
            let other = Arc::clone(&shared);
            std::thread::spawn(move || {
                other.push_refused(
                    ErrorOrigin::Request { id: i64::from(2000 + k), ends: true },
                    321,
                    "B",
                );
            })
            .join()
            .unwrap();
        });
    }
    let mut w = Heard::default();
    client.process_msgs(&mut w);
    hooks::clear(&hooks::BETWEEN_DRAINS);
    client.process_msgs(&mut w);

    let points = *at.borrow();
    assert!(points > 10, "the hook ran between the drains: {points}");
    for k in 1..=points {
        let a = if k % 2 == 0 {
            w.position_of(&format!("order_status:{}:", 1000 + k))
        } else {
            w.position_of(&format!("contract_details_end:{}", 3000 + k))
        };
        let b = w.position_of(&format!("error:{}:321:B", 2000 + k));
        let (a, b) = (a.expect("A is delivered"), b.expect("B is delivered"));
        assert!(a < b, "at drain {k}, the refusal went ahead of the report: {:?}", w.0);
    }
}

/// A loss said before a refusal is delivered before it.
#[test]
fn a_loss_queued_before_a_refusal_is_said_before_it() {
    let (client, _rx, shared) = test_client();
    shared.set_connection_lost();
    shared.push_refused(ErrorOrigin::Request { id: 7, ends: true }, 321, "refused");
    let mut w = Heard::default();
    client.process_msgs(&mut w);
    let lost = w.position_of("error:-1:1100:").expect("the loss is said");
    let refused = w.position_of("error:7:321:").expect("the refusal is said");
    assert!(lost < refused, "{:?}", w.0);
}

/// A stop says neither a loss nor a recovery: its end is the session's last
/// record.
#[test]
fn a_stop_says_neither_a_loss_nor_a_recovery() {
    let (client, _rx, shared) = test_client();
    shared
        .reference
        .set_session_over(crate::reliability::retry::DisconnectReason::ByDesign.as_str());
    shared.set_connection_lost();
    shared.set_connection_lost();
    shared.push_closed();
    let mut w = Heard::default();
    client.process_msgs(&mut w);
    assert_eq!(w.0, ["connection_closed"]);
}

/// A call's answer and its refusal stand after everything pushed before the
/// call.
///
/// Said at the call, a refusal reached the caller ahead of a report the
/// engine had queued before it; answered at the call, a reply did.
#[test]
fn a_calls_answer_and_refusal_arrive_after_what_was_pushed_before_it() {
    let (client, _rx, shared) = test_client();
    shared.orders.push_order_update(status(41, OrderStatus::Submitted, 0.0, 1.0, 0.0));
    client.req_current_time();
    shared.orders.push_order_update(status(42, OrderStatus::Submitted, 0.0, 1.0, 0.0));
    client.set_server_log_level(9);
    let mut w = Heard::default();
    client.process_msgs(&mut w);

    let first = w.position_of("order_status:41:").expect("the first report");
    let answer = w.position_of("current_time").expect("the answer");
    let second = w.position_of("order_status:42:").expect("the second report");
    let refusal = w.position_of("error:-1:").expect("the refusal");
    assert!(first < answer && answer < second && second < refusal, "{:?}", w.0);
}

/// The holdings answer at its place: after what was pushed before the call
/// and before what was pushed after it.
#[test]
fn a_positions_answer_stands_where_it_was_asked() {
    let (client, rx, shared) = test_client();
    shared.portfolio.account_download_is_settled();
    shared.portfolio.set_position_info(holding(100.0));
    shared.orders.push_fill(print(71, 1, 10.0, 1, 0, 10.0));
    client.req_positions();
    crate::api::client::tests::the_engine_answers(&rx, &shared);
    shared.orders.push_fill(print(72, 1, 10.0, 1, 0, 10.0));
    let mut w = Heard::default();
    client.process_msgs(&mut w);

    let before = w.position_of("exec_details:-1:71:").expect("the first fill");
    let answer = w.position_of("position:265598:100").expect("the holding");
    let end = w.position_of("position_end").expect("the end");
    let after = w.position_of("exec_details:-1:72:").expect("the second fill");
    assert!(before < answer && answer < end && end < after, "{:?}", w.0);
}

/// A holding that moved after a fill is said after that fill.
#[test]
fn a_holding_written_after_a_fill_is_said_after_it() {
    let (client, rx, shared) = test_client();
    shared.portfolio.account_download_is_settled();
    shared.portfolio.set_position_info(holding(100.0));
    let mut w = Heard::default();
    client.req_positions();
    crate::api::client::tests::the_engine_answers(&rx, &shared);
    client.process_msgs(&mut w);
    w.0.clear();

    shared.orders.push_fill(print(73, 50, 10.0, 50, 0, 10.0));
    shared.portfolio.set_position_info(holding(150.0));
    client.process_msgs(&mut w);

    let fill = w.position_of("exec_details:-1:73:").expect("the fill");
    let moved = w.position_of("position:265598:150").expect("the holding");
    assert!(fill < moved, "{:?}", w.0);
}

/// A slot handed from one contract to another between a read's poll and its
/// cut is not read as the new contract's.
///
/// The quote polled first is the old occupant's. Delivered under the request
/// that now holds the slot, it named the old contract's price as the new one's,
/// and the baseline it advanced kept the new contract's own quote from being
/// said whole.
#[test]
fn a_quote_polled_before_its_slot_changed_hands_is_not_the_new_contracts() {
    let (client, _rx, shared) = test_client();
    let client = Arc::new(client);
    let quote = |bid: i64| Quote {
        bid: bid * PRICE_SCALE,
        ask: (bid + 1) * PRICE_SCALE,
        ..Default::default()
    };
    // Contract A holds slot 0 under its first occupancy.
    shared.market.set_generation(0, 1);
    shared.push_slot_record(crate::bridge::Record::SlotTaken { slot: 0, generation: 1 });
    client.core.req_to_instrument.lock().unwrap().insert(1, 0);
    client.core.instrument_to_req.lock().unwrap().insert(0, 1);
    shared.market.push_quote(0, &quote(100));
    let mut w = Heard::default();
    client.process_msgs(&mut w);
    assert!(w.has("tick_price:1:1:100"), "{:?}", w.0);
    w.0.clear();

    // A moves, and between the poll and the cut the slot is released and
    // taken by B, whose own quote is then written.
    shared.market.push_quote(0, &quote(101));
    {
        let shared = Arc::clone(&shared);
        let held = Arc::clone(&client);
        hooks::set(&hooks::BEFORE_THE_CUT, move || {
            let core = &held.core;
            shared.push_slot_record(crate::bridge::Record::SlotReleased { slot: 0, generation: 1 });
            shared.market.set_generation(0, 0);
            core.req_to_instrument.lock().unwrap().remove(&1);
            core.instrument_to_req.lock().unwrap().remove(&0);
            core.last_quotes.lock().unwrap().remove(&0);
            shared.market.set_generation(0, 2);
            shared.push_slot_record(crate::bridge::Record::SlotTaken { slot: 0, generation: 2 });
            core.req_to_instrument.lock().unwrap().insert(2, 0);
            core.instrument_to_req.lock().unwrap().insert(0, 2);
            shared.market.push_quote(0, &quote(200));
        });
    }
    client.process_msgs(&mut w);
    hooks::clear(&hooks::BEFORE_THE_CUT);
    assert!(
        !w.0.iter().any(|e| e.starts_with("tick_price:")),
        "A's quote was delivered as B's: {:?}",
        w.0,
    );
    assert!(
        client.core.last_quotes.lock().unwrap().get(&0).is_none(),
        "B's baseline was advanced by A's quote",
    );

    client.process_msgs(&mut w);
    assert!(w.has("tick_price:2:1:200"), "B's own quote, whole: {:?}", w.0);
    assert!(w.has("tick_price:2:2:201"), "{:?}", w.0);
}

/// A call that answers reads the same order: with a record kept, the record
/// hears a fill queued before the call and then the call's answer; with none,
/// the fill stays where it was for the caller's own read.
#[test]
fn a_convenience_reads_the_one_order_with_a_record_kept_or_leaves_it_without() {
    for keep in [true, false] {
        let (client, rx, shared) = test_client();
        let record = Arc::new(std::sync::Mutex::new(Heard::default()));
        if keep {
            client.keep_record(record.clone());
        }
        shared.orders.push_fill(print(74, 1, 10.0, 1, 0, 10.0));
        let engine = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    if let ControlCommand::FetchHeadTimestamp { req_id, .. } = cmd {
                        shared.reference.push_head_timestamp(
                            req_id,
                            crate::control::historical::HeadTimestampResponse {
                                head_timestamp: "20000103-14:30:00".into(),
                                timezone: "UTC".into(),
                            },
                        );
                        return;
                    }
                }
            })
        };
        let head = client.head_timestamp(&spy(), "TRADES", true);
        engine.join().unwrap();
        assert_eq!(head.as_deref(), Ok("20000103-14:30:00"));

        let heard = &record.lock().unwrap().0;
        let mut w = Heard::default();
        client.process_msgs(&mut w);
        if keep {
            let fill = heard.iter().position(|e| e.starts_with("exec_details:-1:74:"));
            let answer = heard.iter().position(|e| e.starts_with("head_timestamp:"));
            assert!(
                fill.is_some() && answer.is_some() && fill < answer,
                "the record hears the fill, then the answer: {heard:?}",
            );
            assert!(!w.has("exec_details:"), "and the caller's read does not hear it again");
        } else {
            assert!(heard.is_empty());
            assert!(
                w.has("exec_details:-1:74:"),
                "the fill waited for the caller's read: {:?}",
                w.0
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
//  The session's last record
// ═══════════════════════════════════════════════════════════════════

/// What was queued before `disconnect()` is delivered under its request, and
/// then the close.
///
/// The requests' mappings were reset at the call, so the final read had
/// nowhere to deliver a quote or a subscription's callback: they went out
/// under no request, or not at all.
#[test]
fn what_was_queued_before_a_disconnect_is_delivered_under_its_request_before_the_close() {
    let (client, rx, shared) = test_client();
    shared.portfolio.account_download_is_settled();
    shared.portfolio.set_position_info(holding(100.0));
    let mut w = Heard::default();
    client.req_positions();
    crate::api::client::tests::the_engine_answers(&rx, &shared);
    client.process_msgs(&mut w);
    w.0.clear();

    client.core.req_to_instrument.lock().unwrap().insert(1, 0);
    client.core.instrument_to_req.lock().unwrap().insert(0, 1);
    shared.market.push_quote(0, &Quote { bid: 100 * PRICE_SCALE, ..Default::default() });
    shared.market.push_tick_req_params(
        0,
        TickReqParams { min_tick: 0.01, bbo_exchange: "9c0001".into(), snapshot_permissions: 3 },
    );
    shared.portfolio.set_position_info(holding(150.0));
    shared.orders.push_order_update(status(75, OrderStatus::Submitted, 0.0, 1.0, 0.0));
    client.disconnect();
    client.process_msgs(&mut w);

    let close = w.position_of("connection_closed").expect("the close");
    assert_eq!(close, w.0.len() - 1, "the close is the last thing said: {:?}", w.0);
    for said in
        ["tick_price:1:1:100", "tick_req_params:1", "position:265598:150", "order_status:75:"]
    {
        assert!(w.has(said), "{said} was not delivered before the close: {:?}", w.0);
    }
}

/// A refusal pushed after the session's last record is never delivered.
#[test]
fn a_refusal_after_the_last_record_is_never_delivered() {
    let (client, _rx, shared) = test_client();
    shared.push_closed();
    shared.push_refused(ErrorOrigin::Request { id: 8, ends: true }, 321, "late");
    let mut w = Heard::default();
    client.process_msgs(&mut w);
    client.process_msgs(&mut w);
    assert_eq!(w.0, ["connection_closed"]);
}

/// The last record ends the read it is in: what follows it is not the
/// session's to deliver.
#[test]
fn the_last_record_ends_its_read_after_what_came_before_it() {
    let (client, _rx, shared) = test_client();
    shared.orders.push_order_update(status(76, OrderStatus::Submitted, 0.0, 1.0, 0.0));
    shared.push_closed();
    shared.reference.push_contract_details_end(9);
    let mut w = Heard::default();
    client.process_msgs(&mut w);
    client.process_msgs(&mut w);
    assert_eq!(w.0.len(), 2, "{:?}", w.0);
    assert!(w.0[0].starts_with("order_status:76:"), "{:?}", w.0);
    assert_eq!(w.0[1], "connection_closed");
}

/// A quote and an account figure written after a read polled them, and then
/// the last record: the final values are delivered, then the close.
///
/// The state a read takes first is older than the last record; delivered as
/// the session's final word, the values written in between were lost.
#[test]
fn the_final_state_is_taken_again_at_the_last_record() {
    let (client, _rx, shared) = test_client();
    client.core.req_to_instrument.lock().unwrap().insert(1, 0);
    client.core.instrument_to_req.lock().unwrap().insert(0, 1);
    client.core.subscribe_account_updates(true);
    shared.market.push_quote(0, &Quote { bid: 100 * PRICE_SCALE, ..Default::default() });
    shared.portfolio.note_account_value("NetLiquidation", "1000", "USD");
    let mut w = Heard::default();
    client.process_msgs(&mut w);
    w.0.clear();

    {
        let shared = Arc::clone(&shared);
        hooks::set(&hooks::BEFORE_THE_CUT, move || {
            shared.market.push_quote(0, &Quote { bid: 101 * PRICE_SCALE, ..Default::default() });
            shared.portfolio.note_account_value("NetLiquidation", "1001", "USD");
            shared.push_closed();
        });
    }
    client.process_msgs(&mut w);
    hooks::clear(&hooks::BEFORE_THE_CUT);

    let quote = w.position_of("tick_price:1:1:101").expect("the final quote");
    let figure = w.position_of("account:NetLiquidation:1001").expect("the final figure");
    let close = w.position_of("connection_closed").expect("the close");
    assert!(quote < close && figure < close, "{:?}", w.0);
}

/// A calculation kept for its model, whose model the engine writes in the lap
/// that ends the session, is answered before the close.
///
/// Solved by the reader at each read, its answer was worked out after the
/// session's last record and never delivered.
#[test]
fn a_calculation_answered_by_the_last_model_is_delivered_before_the_close() {
    let (client, _rx, shared) = test_client();
    let iid: InstrumentId = 0;
    shared.market.set_instrument_count(1);
    client.core.req_to_instrument.lock().unwrap().insert(9, iid);
    shared.market.keep_calculation(
        9,
        crate::bridge::KeptCalculation {
            contract: Contract {
                symbol: "SPY".into(),
                sec_type: "OPT".into(),
                exchange: "SMART".into(),
                currency: "USD".into(),
                last_trade_date_or_contract_month: "20270320".into(),
                strike: 100.0,
                right: "C".into(),
                ..Default::default()
            },
            slot: iid,
            wants_volatility: false,
            option_price: 0.0,
            under_price: 100.0,
            answered: false,
        },
    );

    // The lap that ends the session: the model and what it answers, then the
    // last record, as the loop pushes them.
    shared.market.push_option_computation(OptionComputation {
        instrument: iid,
        implied_vol: 0.2,
        opt_price: 5.0,
        und_price: 100.0,
        pv_dividend: 0.0,
        ..Default::default()
    });
    crate::client_core::answer_kept_calculations(&shared, Some(iid), None);
    shared.push_closed();

    let mut w = Heard::default();
    client.process_msgs(&mut w);
    let answered = w
        .position_of("tick_option_computation:9:53")
        .or_else(|| w.position_of("error:9:"))
        .expect("the calculation is answered");
    let close = w.position_of("connection_closed").expect("the close");
    assert!(answered < close, "{:?}", w.0);
}

// ═══════════════════════════════════════════════════════════════════
//  An order's reports
// ═══════════════════════════════════════════════════════════════════

/// An acknowledgement, a fill and its charge in one read, in the venue's
/// order, each with its own values.
///
/// Paired up at the read, the fill took the status and went first, and the
/// acknowledgement followed it: the order read as submitted with nothing
/// filled after it had filled.
#[test]
fn an_acknowledgement_a_fill_and_its_charge_arrive_in_the_venues_order() {
    let (client, rx, shared) = test_client();
    client.try_place_order(77, &spy(), &limit(1.0, 10.0)).expect("placed");
    while rx.try_recv().is_ok() {}

    shared.orders.push_order_update(status(77, OrderStatus::Submitted, 0.0, 1.0, 0.0));
    shared.orders.push_fill_and_status(
        print(77, 1, 10.0, 1, 0, 10.0),
        None,
        status(77, OrderStatus::Filled, 1.0, 0.0, 10.0),
    );
    shared.orders.push_charge(CommissionAndFeesReport {
        exec_id: "0001.77".into(),
        commission_and_fees: 1.0,
        currency: "USD".into(),
        ..Default::default()
    });
    let mut w = Heard::default();
    client.process_msgs(&mut w);

    assert_eq!(
        w.0,
        [
            "open_order:77:Submitted",
            "order_status:77:Submitted:0:1:0:0",
            "order_status:77:Filled:1:0:10:10",
            "exec_details:-1:77:1:10",
            "charge:0001.77",
        ],
    );
}

/// Three partial fills and two modifications of one order across three reads
/// arrive in the venue's order, each with the quantities and prices its own
/// report stated.
#[test]
fn an_orders_reports_across_reads_keep_the_venues_order_and_their_own_values() {
    let (client, rx, shared) = test_client();
    client.try_place_order(78, &spy(), &limit(300.0, 10.0)).expect("placed");
    while rx.try_recv().is_ok() {}
    let partial = |filled: f64, avg: f64| {
        status(78, OrderStatus::PartiallyFilled, filled, 300.0 - filled, avg)
    };
    let mut w = Heard::default();

    shared.orders.push_fill_and_status(
        print(78, 100, 10.0, 100, 200, 10.0),
        None,
        partial(100.0, 10.0),
    );
    shared.orders.push_order_update(status(78, OrderStatus::Submitted, 100.0, 200.0, 10.0));
    client.process_msgs(&mut w);
    shared.orders.push_fill_and_status(
        print(78, 100, 10.5, 200, 100, 10.25),
        None,
        partial(200.0, 10.25),
    );
    shared.orders.push_order_update(status(78, OrderStatus::Submitted, 200.0, 100.0, 10.25));
    client.process_msgs(&mut w);
    shared.orders.push_fill_and_status(
        print(78, 100, 11.0, 300, 0, 10.5),
        None,
        status(78, OrderStatus::Filled, 300.0, 0.0, 10.5),
    );
    client.process_msgs(&mut w);

    assert_eq!(
        w.0,
        [
            "order_status:78:Submitted:100:200:10:10",
            "exec_details:-1:78:100:10",
            "open_order:78:Submitted",
            "order_status:78:Submitted:100:200:10:0",
            "order_status:78:Submitted:200:100:10.25:10.5",
            "exec_details:-1:78:100:10.5",
            "open_order:78:Submitted",
            "order_status:78:Submitted:200:100:10.25:0",
            "order_status:78:Filled:300:0:10.5:11",
            "exec_details:-1:78:100:11",
        ],
    );
}

/// A working status the venue echoes behind the fill that finished an order is
/// dropped where it is pushed; the acknowledgement pushed before that fill is
/// still delivered.
///
/// Dropped at the read instead, the question asked was whether the order had
/// finished by the time of the read, which also took the acknowledgement that
/// preceded the fill; kept, the echo reported a filled order as working with
/// nothing filled.
#[test]
fn a_working_status_echoed_behind_the_finishing_fill_is_dropped_at_the_push() {
    let (client, rx, shared) = test_client();
    client.try_place_order(79, &spy(), &limit(1.0, 10.0)).expect("placed");
    while rx.try_recv().is_ok() {}

    shared.orders.push_order_update(status(79, OrderStatus::Submitted, 0.0, 1.0, 0.0));
    shared.orders.push_fill_and_status(
        print(79, 1, 10.0, 1, 0, 10.0),
        None,
        status(79, OrderStatus::Filled, 1.0, 0.0, 10.0),
    );
    shared.orders.push_completed_order(CompletedOrder {
        venue_order: String::new(), stated: None, held: None,
        order_id: 79,
        instrument: 0,
        status: OrderStatus::Filled,
        filled_qty: 1,
        timestamp_ns: 0,
    });
    shared.orders.push_order_update(status(79, OrderStatus::Submitted, 0.0, 1.0, 0.0));
    let mut w = Heard::default();
    client.process_msgs(&mut w);

    let statuses: Vec<&String> = w.0.iter().filter(|e| e.starts_with("order_status:")).collect();
    assert_eq!(statuses, ["order_status:79:Submitted:0:1:0:0", "order_status:79:Filled:1:0:10:10"],);
}

#[test]
fn an_accounts_retirement_discards_the_values_already_polled() {
    let (client, _rx, shared) = test_client();
    client.core.subscribe_account_updates(true);
    shared.portfolio.note_account_value("NetLiquidation", "1000", "USD");
    let mut heard = Heard::default();
    client.process_msgs(&mut heard);
    assert!(heard.has("account:NetLiquidation:1000"));
    heard.0.clear();
    shared.portfolio.note_account_value("NetLiquidation", "2000", "USD");
    shared.push_call_record(crate::bridge::Record::Retired(Retirement::Question(
        crate::types::model::Question::AccountUpdates,
    )));
    client.process_msgs(&mut heard);
    assert!(!heard.has("account:"), "{:?}", heard.0);
}

#[test]
fn each_positions_answer_keeps_the_account_its_request_named() {
    #[derive(Default)]
    struct Positions(Vec<(i64, String, f64)>);
    impl Wrapper for Positions {
        fn position_multi(
            &mut self,
            req_id: i64,
            account: &str,
            _: &str,
            _: &Contract,
            position: f64,
            _: f64,
        ) {
            self.0.push((req_id, account.into(), position));
        }
    }
    let (client, _rx, shared) = test_client();
    for (account, position) in [("DU123", 3.0), ("DU2", 7.0)] {
        shared.portfolio_for(account).set_position_info(crate::types::PositionInfo {
            con_id: 756733,
            position,
            ..Default::default()
        });
        shared.push_call_record(crate::bridge::Record::Answer(
            crate::bridge::Answer::PositionsMulti {
                req_id: 1,
                account: account.into(),
                model_code: String::new(),
            },
        ));
    }
    let mut heard = Positions::default();
    client.process_msgs(&mut heard);
    assert_eq!(heard.0, [(1, "DU123".into(), 3.0), (1, "DU2".into(), 7.0)]);
    heard.0.clear();
    shared.portfolio_for("DU2").set_position_info(crate::types::PositionInfo {
        con_id: 756733,
        position: 9.0,
        ..Default::default()
    });
    shared.push_call_record(crate::bridge::Record::Answer(crate::bridge::Answer::PositionsMulti {
        req_id: 2,
        account: "DU2".into(),
        model_code: String::new(),
    }));
    client.process_msgs(&mut heard);
    assert_eq!(heard.0, [(1, "DU2".into(), 9.0), (2, "DU2".into(), 9.0)]);
}

#[test]
fn every_request_keeps_its_outcome_between_the_surrounding_records() {
    type Call = fn(&crate::api::EClient);
    let calls: &[(&str, Call)] = &[
        ("req_smart_components", |c| c.req_smart_components(9, "a6")),
        ("req_news_providers", |c| c.req_news_providers()),
        ("req_current_time", |c| c.req_current_time()),
        ("req_current_time_in_millis", |c| c.req_current_time_in_millis()),
        ("request_fa", |c| c.request_fa(1)),
        ("req_config", |c| c.req_config(9)),
        ("update_config", |c| c.update_config(9)),
        ("replace_fa", |c| c.replace_fa(9, 1, "<List/>")),
        ("calculate_implied_volatility", |c| {
            c.calculate_implied_volatility(9, &Default::default(), 1.0, 1.0)
        }),
        ("calculate_option_price", |c| c.calculate_option_price(9, &Default::default(), 1.0, 1.0)),
        ("cancel_calculate_implied_volatility", |c| c.cancel_calculate_implied_volatility(9)),
        ("cancel_calculate_option_price", |c| c.cancel_calculate_option_price(9)),
        ("req_soft_dollar_tiers", |c| c.req_soft_dollar_tiers(9)),
        ("req_family_codes", |c| c.req_family_codes()),
        ("set_server_log_level", |c| c.set_server_log_level(1)),
        ("req_user_info", |c| c.req_user_info(9)),
        ("req_historical_data", |c| {
            c.req_historical_data(
                9,
                &super::tests::spy(),
                "20260901-00:00:00",
                "1 D",
                "1 min",
                "TRADES",
                false,
                1,
                false,
            )
        }),
        ("cancel_historical_data", |c| c.cancel_historical_data(9)),
        ("req_head_time_stamp", |c| {
            c.req_head_time_stamp(9, &super::tests::spy(), "TRADES", false, 1)
        }),
        ("req_contract_details", |c| c.req_contract_details(9, &super::tests::spy())),
        ("cancel_contract_data", |c| c.cancel_contract_data(9)),
        ("req_mkt_depth_exchanges", |c| c.req_mkt_depth_exchanges()),
        ("req_matching_symbols", |c| c.req_matching_symbols(9, "SPY")),
        ("req_wsh_meta_data", |c| c.req_wsh_meta_data(9)),
        ("cancel_wsh_meta_data", |c| c.cancel_wsh_meta_data(9)),
        ("cancel_wsh_event_data", |c| c.cancel_wsh_event_data(9)),
        ("req_wsh_event_data", |c| c.req_wsh_event_data(9, Default::default())),
        ("req_sec_def_opt_params", |c| c.req_sec_def_opt_params(9, "SPY", "", "STK", 1)),
        ("cancel_head_time_stamp", |c| c.cancel_head_time_stamp(9)),
        ("req_market_rule", |c| c.req_market_rule(1)),
        ("req_news_bulletins", |c| c.req_news_bulletins(false)),
        ("cancel_news_bulletins", |c| c.cancel_news_bulletins()),
        ("req_scanner_parameters", |c| c.req_scanner_parameters()),
        ("req_scanner_subscription", |c| {
            c.req_scanner_subscription(9, "STK", "STK.US", "TOP_PERC_GAIN", 1, &[], "")
        }),
        ("cancel_scanner_subscription", |c| c.cancel_scanner_subscription(9)),
        ("req_historical_news", |c| c.req_historical_news(9, 1, "", "", "", 1)),
        ("req_news_article", |c| c.req_news_article(9, "BRFG", "BRFG$1")),
        ("req_adjustments", |c| c.req_adjustments(9, 1, "STK", "SMART", "", "")),
        ("req_fundamental_data", |c| {
            c.req_fundamental_data(9, &super::tests::spy(), "ReportsFinSummary")
        }),
        ("cancel_fundamental_data", |c| c.cancel_fundamental_data(9)),
        ("cancel_historical_news", |c| c.cancel_historical_news(9)),
        ("req_histogram_data", |c| c.req_histogram_data(9, &super::tests::spy(), false, "1 day")),
        ("cancel_histogram_data", |c| c.cancel_histogram_data(9)),
        ("req_historical_ticks", |c| {
            c.req_historical_ticks(
                9,
                &super::tests::spy(),
                "",
                "20260901-00:00:00",
                1,
                "TRADES",
                false,
                false,
            )
        }),
        ("cancel_historical_ticks", |c| c.cancel_historical_ticks(9)),
        ("req_historical_schedule", |c| {
            c.req_historical_schedule(9, &super::tests::spy(), "20260901-00:00:00", "1 D", false)
        }),
        ("req_positions", |c| c.req_positions()),
        ("req_pnl", |c| c.req_pnl(9, "DU123", "")),
        ("cancel_pnl", |c| c.cancel_pnl(9)),
        ("req_pnl_single", |c| c.req_pnl_single(9, "DU123", "", 1)),
        ("cancel_pnl_single", |c| c.cancel_pnl_single(9)),
        ("req_account_summary", |c| c.req_account_summary(9, "All", "NetLiquidation")),
        ("cancel_account_summary", |c| c.cancel_account_summary(9)),
        ("req_account_updates", |c| c.req_account_updates(false, "")),
        ("cancel_positions", |c| c.cancel_positions()),
        ("req_managed_accts", |c| c.req_managed_accts()),
        ("req_account_updates_multi", |c| c.req_account_updates_multi(9, "DU123", "", false)),
        ("cancel_account_updates_multi", |c| c.cancel_account_updates_multi(9)),
        ("req_positions_multi", |c| c.req_positions_multi(9, "DU123", "")),
        ("cancel_positions_multi", |c| c.cancel_positions_multi(9)),
        ("req_spread_scan", |c| c.req_spread_scan(9, &super::tests::spy(), &Default::default())),
        ("req_mkt_data_ex", |c| {
            c.req_mkt_data_ex(9, &super::tests::spy(), "", false, false, 1, &[])
        }),
        ("cancel_mkt_data", |c| c.cancel_mkt_data(9)),
        ("req_tick_by_tick_data", |c| {
            c.req_tick_by_tick_data(9, &super::tests::spy(), "Last", 1, false)
        }),
        ("cancel_tick_by_tick_data", |c| c.cancel_tick_by_tick_data(9)),
        ("req_mkt_depth", |c| c.req_mkt_depth(9, &super::tests::spy(), 1, false)),
        ("cancel_mkt_depth", |c| c.cancel_mkt_depth(9)),
        ("req_real_time_bars", |c| {
            c.req_real_time_bars(9, &super::tests::spy(), 1, "TRADES", false)
        }),
        ("cancel_real_time_bars", |c| c.cancel_real_time_bars(9)),
        ("req_ping", |c| c.req_ping()),
        ("place_order", |c| c.place_order(9, &super::tests::spy(), &Default::default())),
        ("exercise_options", |c| {
            c.exercise_options(9, &super::tests::spy(), 1, 1, "DU123", false, Default::default())
        }),
        ("cancel_order", |c| c.cancel_order(9, "")),
        ("cancel_order_by_perm_id", |c| c.cancel_order_by_perm_id(9)),
        ("req_global_cancel", |c| c.req_global_cancel("")),
        ("req_ids", |c| c.req_ids(1)),
        ("req_open_orders", |c| c.req_open_orders()),
        ("req_all_open_orders", |c| c.req_all_open_orders()),
        ("req_completed_orders", |c| c.req_completed_orders(false)),
        ("req_auto_open_orders", |c| c.req_auto_open_orders(false)),
        ("req_executions", |c| c.req_executions(9, &Default::default())),
        ("req_mkt_data", |c| c.req_mkt_data(9, &super::tests::spy(), "", false, false)),
        ("req_market_data_type", |c| c.req_market_data_type(1)),
    ];
    for (name, call) in calls {
        let (client, rx, shared) = test_client();
        drop(rx);
        shared.push_refused(crate::api::ErrorOrigin::Session, 2103, "before");
        call(&client);
        shared.push_refused(crate::api::ErrorOrigin::Session, 2104, "after");
        let cut = shared.next_seq();
        let mut heard = Heard::default();
        client.process_msgs(&mut heard);
        let errors: Vec<_> = heard.0.iter().filter(|e| e.starts_with("error:")).collect();
        assert!(errors.first().unwrap().starts_with("error:-1:2103:"), "{name}: {:?}", heard.0);
        assert!(errors.last().unwrap().starts_with("error:-1:2104:"), "{name}: {errors:?}");
        assert_eq!(shared.next_seq(), cut, "{name}: reading produces no records");
    }
}
