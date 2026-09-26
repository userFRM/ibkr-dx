//! What each error is about, as a caller reads it: a request and whether
//! nothing more follows for it, an order and the operation on it, a request
//! that carries no number, the session, or a lookup this client made for
//! itself.

use super::tests::test_client;
use crate::api::wrapper::Wrapper;
use crate::types::model::{
    CommissionAndFeesReport, Contract, ErrorOrigin, Execution, ExecutionFilter, Order, OrderOp,
    OrderState, Question,
};

/// Every error with what it is about, and the callbacks around them.
#[derive(Default)]
struct Told(Vec<String>);

impl Wrapper for Told {
    fn error_from(&mut self, origin: ErrorOrigin, _error_time: i64, code: i64, _: &str, _: &str) {
        self.0.push(format!("{origin:?} {code}"));
    }
    fn exec_details(&mut self, req_id: i64, _: &Contract, _: &Execution) {
        self.0.push(format!("exec_details {req_id}"));
    }
    fn exec_details_end(&mut self, req_id: i64) {
        self.0.push(format!("exec_details_end {req_id}"));
    }
    fn open_order(&mut self, order_id: i64, _: &Contract, _: &Order, _: &OrderState) {
        self.0.push(format!("open_order {order_id}"));
    }
    fn open_order_end(&mut self) {
        self.0.push("open_order_end".into());
    }
}

/// A wrapper written before `error_from` existed.
#[derive(Default)]
struct OnlyError(Vec<(i64, i64)>, Vec<i64>);

impl Wrapper for OnlyError {
    fn error(&mut self, req_id: i64, error_time: i64, code: i64, _: &str, _: &str) {
        self.0.push((req_id, code));
        self.1.push(error_time);
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

fn limit() -> Order {
    Order {
        action: "BUY".into(),
        total_quantity: 100.0,
        order_type: "LMT".into(),
        lmt_price: 100.0,
        tif: "DAY".into(),
        ..Default::default()
    }
}

/// The same number can be an order's and a lookup's the engine made for
/// itself. Each error says which it is about, and a wrapper that implements
/// only `error` still hears both under that number.
#[test]
fn an_internal_lookups_error_and_an_orders_error_under_one_number_say_which_they_are() {
    let n = u32::MAX - 1;
    let (client, _rx, shared) = test_client();
    // As the engine refuses a lookup, and an order, under the number each
    // was made under.
    shared.reference.push_historical_error(n, 200, "no security definition".into());
    shared.orders.push_order_inactive(u64::from(n), OrderOp::Place, 201, "refused".into());
    let mut told = Told::default();
    client.process_msgs(&mut told);
    assert_eq!(
        told.0,
        [
            format!("{:?} 200", ErrorOrigin::Internal(n)),
            format!("{:?} 201", ErrorOrigin::Order { id: i64::from(n), op: OrderOp::Place }),
        ],
    );

    let (client, _rx, shared) = test_client();
    shared.reference.push_historical_error(n, 200, "no security definition".into());
    shared.orders.push_order_inactive(u64::from(n), OrderOp::Place, 201, "refused".into());
    let mut heard = OnlyError::default();
    client.process_msgs(&mut heard);
    assert_eq!(heard.0, [(i64::from(n), 200), (i64::from(n), 201)], "as it always was");
}

/// Every error carries a time in milliseconds since the epoch, as a gateway
/// stamps every error it sends: a refusal of an order the venue sent, the
/// whole second the venue's message states it was sent, and anything else,
/// the clock as the error is delivered. Every error one report makes carries
/// the report's time.
#[test]
fn every_error_carries_a_clock_reading() {
    let refused: &[(u32, &str)] = &[(150, "8"), (39, "8")];
    // A change the venue restates as refused: its message, and the refusal of
    // the change.
    let restated: &[(u32, &str)] = &[(150, "D"), (39, "0"), (378, "102")];
    // `None` where the time is the clock's.
    for (report, sent, told, stamped) in [
        (refused, None, &[201][..], None),
        (refused, Some("20260926-10:15:10.250"), &[201], Some(1_790_417_710_000)),
        (restated, Some("20260926-10:15:10.250"), &[399, 10148], Some(1_790_417_710_000)),
    ] {
        let (client, _rx, shared) = test_client();
        client.place_order(9, &spy(), &Order { order_type: "NOT A TYPE".into(), ..limit() });
        let mut context = crate::engine::context::Context::new();
        let instrument = context.register_instrument(756733);
        context.insert_order(crate::types::Order::new(
            7, instrument, crate::types::Side::Buy, crate::types::QTY_SCALE, crate::types::PRICE_SCALE,
            b'2', b'0', 0,
        ));
        let mut frame: std::collections::HashMap<u32, String> = [
            (11, "7"), (58, "refused"), (40, "2"), (38, "1"), (14, "0"), (151, "1"),
        ].iter().chain(report).map(|(tag, value)| (*tag, value.to_string())).collect();
        if let Some(sent) = sent {
            frame.insert(52, sent.into());
        }
        crate::engine::hot_loop::ccp::CcpState::new()
            .handle_exec_report(&frame, b"", &mut context, &shared, &None, "DU1");
        let mut heard = OnlyError::default();
        client.process_msgs(&mut heard);
        let venue: Vec<_> = told.iter().map(|&code| (7, code)).collect();
        assert_eq!(heard.0, [&[(9, 10051)][..], &venue].concat(), "{report:?} {sent:?}");
        assert!(heard.1[0] > 1_700_000_000_000, "a clock reading in milliseconds, got {:?}", heard.1);
        for &at in &heard.1[1..] {
            match stamped {
                Some(when) => assert_eq!(at, when, "the venue's time, to the second it states: {report:?}"),
                None => assert!(at > 1_700_000_000_000, "a clock reading, got {:?}", heard.1),
            }
        }
    }
}

/// A new order and a change to it, each refused at its call under one
/// number, say which they are.
#[test]
fn a_refused_new_order_and_a_refused_modify_of_one_id_carry_place_and_modify() {
    let (client, _rx, _shared) = test_client();
    let unsendable = Order { order_type: "NOT A TYPE".into(), ..limit() };
    client.place_order(9, &spy(), &unsendable);
    // The venue is working it now.
    client.track_order_for_test(9, spy(), limit(), 0);
    client.place_order(9, &spy(), &unsendable);
    let mut told = Told::default();
    client.process_msgs(&mut told);
    let origins: Vec<&str> = told.0.iter().map(|e| e.rsplit_once(' ').unwrap().0).collect();
    assert_eq!(
        origins,
        [
            format!("{:?}", ErrorOrigin::Order { id: 9, op: OrderOp::Place }),
            format!("{:?}", ErrorOrigin::Order { id: 9, op: OrderOp::Modify }),
        ],
    );
}

/// The executions notice is not the request's end: the executions and their
/// end follow it.
#[test]
fn the_executions_notice_does_not_end_the_request_and_its_answer_follows() {
    let (client, _rx, shared) = test_client();
    // What this session holds starts now, so the days before are not held.
    shared.reference.set_executions_held_from(Some(jiff::Timestamp::now().as_second()));
    client.core.push_execution(
        spy(),
        Execution { order_id: 5, exec_id: "e1".into(), ..Default::default() },
        CommissionAndFeesReport::default(),
    );
    client.req_executions(7, &ExecutionFilter { last_n_days: 2, ..Default::default() });
    let mut told = Told::default();
    client.process_msgs(&mut told);
    assert_eq!(
        told.0,
        [
            format!("{:?} 321", ErrorOrigin::Request { id: 7, ends: false }),
            "exec_details 7".to_string(),
            "exec_details_end 7".to_string(),
        ],
    );
}

/// What belongs to no request is the session's; a refusal ends its request,
/// and a word about a companion of a quote does not end the quote.
#[test]
fn the_sessions_words_and_a_streams_words_say_what_they_are_about() {
    let (client, _rx, shared) = test_client();
    client.map_req_instrument(4, 0);
    shared.set_connection_lost();
    shared.set_connection_restored(String::new());
    shared.market.push_venue_error("the venue says so".into());
    shared.market.push_companion_refusal(0, 1, "not this series".into());
    shared.market.push_subscription_failure(0, "no such contract".into());
    let mut told = Told::default();
    client.process_msgs(&mut told);
    assert_eq!(
        told.0,
        [
            format!("{:?} 1100", ErrorOrigin::Session),
            format!("{:?} 1102", ErrorOrigin::Session),
            format!("{:?} 2148", ErrorOrigin::Session),
            format!("{:?} 321", ErrorOrigin::Request { id: 4, ends: false }),
            format!("{:?} 200", ErrorOrigin::Request { id: 4, ends: true }),
        ],
    );
}

/// A withdrawal of everything names no request and no question: it has no
/// end of its own. Its refusal is the session's.
#[test]
fn a_global_cancel_refused_is_the_sessions() {
    let (client, _rx, _shared) = test_client();
    client.core.set_readonly(true);
    client.req_global_cancel("");
    let mut told = Told::default();
    client.process_msgs(&mut told);
    assert!(told.0[0].starts_with(&format!("{:?} ", ErrorOrigin::Session)), "{:?}", told.0);
}

/// A withdrawal by permanent id is refused under the number the order's own
/// reports carry in this session, as a cancel; one naming no order of this
/// session's is the session's.
#[test]
fn a_cancel_by_permanent_id_is_refused_under_the_orders_own_number() {
    let (client, rx, shared) = test_client();
    shared.orders.set_replay_done();
    shared.orders.push_order_info(
        4242,
        crate::bridge::RichOrderInfo {
            contract: spy(),
            order: Order { order_id: 4242, perm_id: 777_001, ..limit() },
            order_state: OrderState { status: "Submitted".into(), ..Default::default() },
            last_exec: Default::default(),
        },
    );
    // And the venue has since said the order finished, so the withdrawal the
    // order is found for is refused.
    shared.orders.push_order_update(crate::types::OrderUpdate {
        order_id: 4242,
        instrument: 0,
        status: crate::types::OrderStatus::Filled,
        filled_qty: 1.0,
        remaining_qty: 0.0,
        avg_price: 0,
        perm_id: 777_001,
        parent_id: 0,
        timestamp_ns: 0,
    });
    client.cancel_order_by_perm_id(777_001);
    client.cancel_order_by_perm_id(1);
    rx.pump();
    let mut told = Told::default();
    client.process_msgs(&mut told);
    told.0.retain(|e| e.starts_with("Order") || e.starts_with("Session"));
    let origins: Vec<&str> = told.0.iter().map(|e| e.rsplit_once(' ').unwrap().0).collect();
    assert_eq!(
        origins,
        [
            format!("{:?}", ErrorOrigin::Order { id: 4242, op: OrderOp::Cancel }),
            format!("{:?}", ErrorOrigin::Session),
        ],
    );
}

/// An advisor's question about a partition carries no number; replacing one
/// does.
#[test]
fn an_advisor_question_and_an_advisor_replacement_say_which_they_are() {
    let (client, _rx, shared) = test_client();
    use crate::engine::hot_loop::ccp::PendingAdvisor;
    shared.reference.push_advisor_refused(PendingAdvisor::origin_of(-1, false), 1, "no".into());
    shared.reference.push_advisor_refused(PendingAdvisor::origin_of(8, true), 1, "no".into());
    let mut told = Told::default();
    client.process_msgs(&mut told);
    assert_eq!(
        told.0,
        [
            format!("{:?} 1", ErrorOrigin::Question { q: Question::Fa, ends: true }),
            format!("{:?} 1", ErrorOrigin::Request { id: 8, ends: true }),
        ],
    );
}

/// The questions the account answers say which question an error ends, and
/// withdrawing the account's figures, like withdrawing the holdings, is the
/// session's.
#[test]
fn the_accounts_questions_say_which_question_an_error_ends() {
    let (client, rx, shared) = test_client();
    // The session ends while the holdings are being waited for.
    shared.portfolio.account_download_is_pending();
    let ending = shared.clone();
    let ends = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        ending.reference.set_session_over("stopped");
    });
    client.req_positions();
    crate::api::client::tests::the_engine_answers(&rx, &shared);
    ends.join().unwrap();
    client.req_account_updates(true, "");
    client.req_account_updates(false, "");
    let mut told = Told::default();
    client.process_msgs(&mut told);
    let origins: Vec<&str> = told.0.iter().map(|e| e.rsplit_once(' ').unwrap().0).collect();
    assert_eq!(
        origins,
        [
            format!("{:?}", ErrorOrigin::Question { q: Question::Positions, ends: true }),
            format!("{:?}", ErrorOrigin::Question { q: Question::AccountUpdates, ends: true }),
            format!("{:?}", ErrorOrigin::Session),
        ],
    );
}

/// The scanner's parameters are asked for under no number: a refusal of the
/// question says it is that question's.
#[test]
fn a_refused_question_for_the_scanners_parameters_says_so() {
    let (client, rx, shared) = test_client();
    // An engine with no historical connection to ask it on.
    let mut engine = crate::engine::hot_loop::HotLoop::new(shared, None, None);
    engine.set_control_rx(rx.into_receiver());
    client.req_scanner_parameters();
    engine.poll_once();
    let mut told = Told::default();
    client.process_msgs(&mut told);
    let origins: Vec<&str> = told.0.iter().map(|e| e.rsplit_once(' ').unwrap().0).collect();
    assert_eq!(
        origins,
        [format!("{:?}", ErrorOrigin::Question { q: Question::ScannerParameters, ends: true })],
    );
}

/// Each word about an order says which operation on it it answers, and a
/// refusal of a request, however it arrives, says it ends that request.
#[test]
fn the_words_about_orders_and_requests_say_what_they_answer() {
    let (client, _rx, shared) = test_client();
    shared.orders.push_order_notice(5, OrderOp::Modify, 2181, "not supported".into());
    shared.orders.push_cancel_reject(crate::types::CancelReject {
        order_id: 5,
        instrument: 0,
        reject_type: 2,
        reason_code: -1,
        timestamp_ns: 0,
        still_working: None,
        answers_a_live_change: false,
    });
    shared.orders.push_cancel_reject(crate::types::CancelReject {
        order_id: 6,
        instrument: 0,
        reject_type: 1,
        reason_code: 0,
        timestamp_ns: 0,
        still_working: None,
        answers_a_live_change: false,
    });
    shared.market.push_subscription_failure_for(11, "no such contract".into());
    shared.reference.push_scanner_data(
        12,
        crate::control::scanner::ScannerResult {
            con_ids: Vec::new(),
            entries: Vec::new(),
            scan_time: String::new(),
            error_text: "Scanner subscription not allowed".into(),
        },
    );
    let mut told = Told::default();
    client.process_msgs(&mut told);
    let origins: Vec<&str> = told.0.iter().map(|e| e.rsplit_once(' ').unwrap().0).collect();
    assert_eq!(
        origins,
        [
            format!("{:?}", ErrorOrigin::Order { id: 5, op: OrderOp::Modify }),
            format!("{:?}", ErrorOrigin::Order { id: 5, op: OrderOp::Modify }),
            format!("{:?}", ErrorOrigin::Order { id: 6, op: OrderOp::Cancel }),
            format!("{:?}", ErrorOrigin::Request { id: 11, ends: true }),
            format!("{:?}", ErrorOrigin::Request { id: 12, ends: true }),
        ],
    );
}
