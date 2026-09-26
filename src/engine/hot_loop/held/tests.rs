//! What the loop holds, and how each exchange it carries ends.
//!
//! A request whose answer the wire cannot tie to it stays open until its real
//! end, and the next one is sent only after; a request is withdrawn by its own
//! cancel; and each ends once.

use super::*;
use crate::api::client::EClient;
use crate::protocol::connection::Connection;
use std::io::Read;
use std::time::Duration;

/// A loop with a historical connection and a channel to it.
fn with_historical() -> (HotLoop, Arc<SharedState>, Sender<ControlCommand>, std::net::TcpStream) {
    let shared = Arc::new(SharedState::new());
    let mut hl = HotLoop::new(shared.clone(), None, None);
    let (conn, peer) = Connection::for_test();
    peer.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    hl.hmds_conn = Some(conn);
    let (tx, rx) = channel();
    hl.set_control_rx(rx);
    (hl, shared, tx, peer)
}

/// Everything the peer has been sent since it was last read.
fn sent(peer: &mut std::net::TcpStream) -> String {
    let mut out = String::new();
    let mut buf = [0u8; 16384];
    while let Ok(n) = peer.read(&mut buf) {
        if n == 0 {
            break;
        }
        out.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    out
}

/// A historical-connection frame carrying one query document.
fn historical_frame(xml: &str) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    msg
}

/// Hand the loop one frame from the historical connection.
fn arrives(hl: &mut HotLoop, msg: &[u8]) {
    hl.hmds.process_hmds_message(msg, &mut hl.hmds_conn, &hl.shared, &hl.event_tx, &mut hl.hb);
    hl.poll_once();
}

fn stock() -> ContractRef {
    ContractRef {
        con_id: 756733,
        symbol: "SPY".into(),
        sec_type: "STK".into(),
        exchange: "SMART".into(),
        currency: "USD".into(),
        ..Default::default()
    }
}

/// `cancel_historical_data` withdraws a trading schedule asked under its
/// number, as it withdraws bars: nothing is refused, the venue is told, and
/// the schedule's late answer reaches nobody.
#[test]
fn withdrawing_a_schedule_query_refuses_nothing_and_its_late_answer_is_dropped() {
    let (mut hl, shared, tx, mut peer) = with_historical();
    tx.send(ControlCommand::FetchHistoricalSchedule {
        contract: stock(),
        req_id: 7,
        end_date_time: "20260306-21:00:00".into(),
        duration: "1 W".into(),
        use_rth: true,
        filters: Default::default(),
    })
    .unwrap();
    hl.poll_control_commands();
    let query_id = hl.hmds.pending_schedule.first().expect("the schedule is on the wire").0.clone();
    let _ = sent(&mut peer);

    tx.send(ControlCommand::CancelHistorical { req_id: 7 }).unwrap();
    hl.poll_control_commands();

    assert!(hl.hmds.pending_schedule.is_empty(), "its entry goes with the withdrawal");
    assert!(
        shared.reference.drain_historical_errors().is_empty(),
        "and nothing is refused: the withdrawal named a query that was waiting",
    );
    let told = sent(&mut peer);
    assert!(told.contains(&format!("ticker:{query_id}")), "the venue is told to stop: {told}");

    let late = format!(
        "<ResultSetSchedule><id>{query_id}</id><eoq>true</eoq><tz>US/Eastern</tz>\
         <derivedStart>20260306-14:30:00</derivedStart><Events><Open><time>20260306-14:30:00</time>\
         <refDate>20260306</refDate></Open><Close><time>20260306-21:00:00</time></Close></Events>\
         </ResultSetSchedule>",
    );
    arrives(&mut hl, &historical_frame(&late));
    assert!(
        shared.reference.drain_historical_schedules().is_empty(),
        "the late answer reaches nobody",
    );
}

/// A number a caller reused for a corporate-actions query of its own and for
/// bars is withdrawn by each call for its own query.
///
/// `cancel_historical_data` took the oldest actions query under the number,
/// which was the caller's standalone one: its answer was lost, and the query
/// the bars' own fold had sent went on being served.
#[test]
fn withdrawing_bars_withdraws_their_own_actions_query_and_not_the_callers() {
    let (mut hl, shared, tx, mut peer) = with_historical();
    // The caller's own query, under 7.
    tx.send(ControlCommand::FetchAdjustments {
        req_id: 7,
        con_id: 756733,
        sec_type: "STK".into(),
        exchange: "SMART".into(),
        start_date: "20240101".into(),
        end_date: "20241231".into(),
    })
    .unwrap();
    hl.poll_control_commands();
    let standalone = hl.hmds.pending_adjustments[0].0.clone();
    // And adjusted bars under 7, whose series has sent its own.
    hl.hmds.send_adjustments_request(
        7,
        756733,
        "STK",
        "SMART",
        "20240601",
        "20260101",
        &shared,
        &mut hl.hmds_conn,
        &mut hl.hb,
    );
    let the_folds = hl.hmds.pending_adjustments[1].0.clone();
    hl.hmds.held.push(hmds::HeldSeries {
        req_id: 7,
        bars: Vec::new(),
        timezone: String::new(),
        actions_query: Some(the_folds.clone()),
        fold: hmds::Fold::Adjusted,
        actions: None,
        complete: true,
        along: Default::default(),
    });
    hl.hmds.pending_historical.push(("hist_1".into(), 7));
    let _ = sent(&mut peer);

    tx.send(ControlCommand::CancelHistorical { req_id: 7 }).unwrap();
    hl.poll_control_commands();

    let told = sent(&mut peer);
    assert!(told.contains(&the_folds), "the bars' own actions query is withdrawn: {told}");
    assert!(!told.contains(&standalone), "and not the caller's: {told}");
    assert_eq!(
        hl.hmds.pending_adjustments.iter().map(|(q, ..)| q.clone()).collect::<Vec<_>>(),
        std::slice::from_ref(&standalone),
        "the caller's own query is still waiting",
    );

    // And the caller's own withdrawal takes its own.
    tx.send(ControlCommand::CancelCorporateActions { req_id: 7 }).unwrap();
    hl.poll_control_commands();
    assert!(sent(&mut peer).contains(&standalone));
    assert!(hl.hmds.pending_adjustments.is_empty());
}

/// A bar stream withdrawn before the venue acknowledged it is withdrawn again
/// by the number its acknowledgement states: withdrawn by the name this client
/// asked under, the venue finds nothing and goes on sending.
#[test]
fn a_bar_stream_withdrawn_before_its_ack_is_withdrawn_by_number_at_the_ack() {
    let (mut hl, _shared, tx, mut peer) = with_historical();
    tx.send(ControlCommand::SubscribeRealTimeBar {
        contract: stock(),
        req_id: 7,
        what_to_show: "TRADES".into(),
        use_rth: true,
        filters: Default::default(),
    })
    .unwrap();
    hl.poll_control_commands();
    let query_id = hl.hmds.rtbar_subs[0].0.clone();
    let _ = sent(&mut peer);

    tx.send(ControlCommand::CancelRealTimeBar { req_id: 7 }).unwrap();
    hl.poll_control_commands();
    let _ = sent(&mut peer);

    arrives(
        &mut hl,
        &historical_frame(&format!(
            "<ResultSetTickerId><id>{query_id}</id><tickerId>4711</tickerId>\
         <minTick>0.01</minTick></ResultSetTickerId>",
        )),
    );
    let told = sent(&mut peer);
    assert!(told.contains("ticker:4711"), "withdrawn again by its number: {told}");
    assert!(hl.hmds.rtbar_subs.is_empty(), "and nothing is routed to it");
    assert!(hl.hmds.rtbar_withdrawn_unnumbered.is_empty());
}

/// Where a corporate-actions answer is put is said in the loop's step, so a
/// withdrawal and a request again under one number stay in order.
///
/// Said at the call, the second request's slot was ready before the loop had
/// taken the withdrawal of the first: an answer to the first arriving in
/// between filled it, and the second caller read the first range's actions as
/// its own.
#[test]
fn a_withdrawal_and_a_request_again_under_one_number_stay_in_order() {
    let (mut hl, shared, tx, _peer) = with_historical();
    let client = EClient::from_parts(shared.clone(), tx, std::thread::spawn(|| {}), "DU1".into());
    client.req_adjustments(7, 756733, "STK", "SMART", "20240101", "20241231");
    hl.poll_control_commands();
    let first = hl.hmds.pending_adjustments[0].0.clone();

    // The caller withdraws it and asks under the same number again, and the
    // first's answer arrives before the loop takes either.
    client.cancel_adjustments(7);
    client.req_adjustments(7, 756733, "STK", "SMART", "20250101", "20251231");
    let answer = {
        let echoed =
            format!("<ListOfQueries><ConAdjQuery><id>{first}</id></ConAdjQuery></ListOfQueries>");
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=U\x016040=10022\x016118=");
        msg.extend_from_slice(echoed.as_bytes());
        msg.extend_from_slice(b"\x0196=conc\n756733,-1,-1\n\n\x01");
        msg
    };
    arrives(&mut hl, &answer);
    hl.poll_control_commands();

    assert_eq!(
        client.adjustments_for(7),
        None,
        "the first range's answer is not the second request's",
    );
}

/// The scanner's parameters are asked one question at a time: the answer
/// names no question, so a second waits for the first's answer.
#[test]
fn a_second_question_for_the_scanners_parameters_waits_for_the_first_answer() {
    let (mut hl, shared, tx, mut peer) = with_historical();
    tx.send(ControlCommand::FetchScannerParams).unwrap();
    tx.send(ControlCommand::FetchScannerParams).unwrap();
    hl.poll_control_commands();
    assert_eq!(sent(&mut peer).matches("6040=10001").count(), 1, "one on the wire");
    assert_eq!(hl.commands_held(), 1, "and the second counted while it waits");

    let mut answer = Vec::new();
    answer.extend_from_slice(b"35=U\x016040=10002\x016118=<ScanParameterResponse/>\x01");
    arrives(&mut hl, &answer);
    assert_eq!(sent(&mut peer).matches("6040=10001").count(), 1, "the second goes after it");
    assert_eq!(shared.reference.drain_scanner_params().len(), 1);
    assert_eq!(hl.commands_held(), 0);
}

// ── The account's questions, held for the download ──

/// A loop that is not running, and a client admitting to it.
fn stopped() -> (HotLoop, Arc<SharedState>, EClient) {
    let shared = Arc::new(SharedState::new());
    let mut hl = HotLoop::new(shared.clone(), None, None);
    let (tx, rx) = channel();
    hl.set_control_rx(rx);
    let client = EClient::from_parts(shared.clone(), tx, std::thread::spawn(|| {}), "DU1".into());
    (hl, shared, client)
}

fn heard(client: &EClient) -> Vec<String> {
    let mut w = crate::api::wrapper::tests::RecordingWrapper::default();
    client.process_msgs(&mut w);
    w.events
}

fn aapl(qty: f64) -> crate::types::PositionInfo {
    crate::types::PositionInfo {
        con_id: 265598,
        position: qty,
        symbol: "AAPL".into(),
        sec_type: "STK".into(),
        currency: "USD".into(),
        ..Default::default()
    }
}

/// `req_positions` held for the account's download, then `cancel_positions`:
/// the question is never answered, and its cancel is confirmed alone.
#[test]
fn a_held_positions_question_withdrawn_by_its_cancel_is_answered_with_its_retirement_alone() {
    let (mut hl, shared, client) = stopped();
    shared.portfolio.account_download_is_pending();
    shared.portfolio.set_position_info(aapl(100.0));
    client.req_positions();
    hl.poll_once();
    hl.asks.answer_what_is_ready(&shared, &mut { COMMANDS_PER_LAP });
    assert_eq!(client.backlog(), 1, "held for the download, and counted");

    client.cancel_positions();
    hl.poll_once();
    // The download completes afterwards: nothing answers the withdrawn one.
    shared.portfolio.account_download_is_settled();
    hl.asks.answer_what_is_ready(&shared, &mut { COMMANDS_PER_LAP });
    assert_eq!(heard(&client), ["question_retired:Positions"]);
    assert_eq!(client.backlog(), 0);
}

/// Answered before its cancel: the answer, its end, then the retirement, and
/// nothing of the holdings after it.
#[test]
fn a_positions_question_answered_before_its_cancel_ends_before_its_retirement() {
    let (mut hl, shared, client) = stopped();
    shared.portfolio.account_download_is_settled();
    shared.portfolio.set_position_info(aapl(100.0));
    client.req_positions();
    hl.poll_once();
    hl.asks.answer_what_is_ready(&shared, &mut { COMMANDS_PER_LAP });
    client.cancel_positions();
    hl.poll_once();
    // A holding moves after the cancel was taken.
    shared.portfolio.set_position_info(aapl(150.0));
    let told = heard(&client);
    let end = told.iter().position(|e| e == "position_end").expect("the answer ends");
    let retired =
        told.iter().position(|e| e == "question_retired:Positions").expect("the retirement");
    assert!(end < retired, "{told:?}");
    assert_eq!(retired, told.len() - 1, "nothing of the holdings follows it: {told:?}");
    assert!(heard(&client).is_empty(), "and nothing later either");
}

/// `req_account_updates(true)` then `(false)` with no read between: the answer
/// then the retirement where the subscription was answered, the retirement
/// alone where it was still held; nothing of the account after either.
#[test]
fn an_account_subscription_and_its_withdrawal_end_at_the_retirement() {
    #[derive(Default)]
    struct Account(Vec<String>);
    impl crate::api::wrapper::Wrapper for Account {
        fn update_account_value(&mut self, key: &str, _: &str, _: &str, _: &str) {
            self.0.push(format!("update_account_value:{key}"));
        }
        fn account_download_end(&mut self, _: &str) {
            self.0.push("account_download_end".into());
        }
        fn question_retired(&mut self, q: crate::types::model::Question) {
            self.0.push(format!("question_retired:{q:?}"));
        }
    }
    let heard = |client: &EClient| {
        let mut w = Account::default();
        client.process_msgs(&mut w);
        w.0
    };
    for downloaded in [true, false] {
        let (mut hl, shared, client) = stopped();
        shared.portfolio.note_account_value("NetLiquidation", "100000.00", "USD");
        if downloaded {
            shared.portfolio.account_download_is_settled();
        } else {
            shared.portfolio.account_download_is_pending();
        }
        client.req_account_updates(true, "");
        hl.poll_once();
        hl.asks.answer_what_is_ready(&shared, &mut { COMMANDS_PER_LAP });
        client.req_account_updates(false, "");
        hl.poll_once();
        shared.portfolio.account_download_is_settled();
        shared.portfolio.note_account_value("NetLiquidation", "100001.00", "USD");
        hl.asks.answer_what_is_ready(&shared, &mut { COMMANDS_PER_LAP });
        let told = heard(&client);
        assert_eq!(
            told.last().map(String::as_str),
            Some("question_retired:AccountUpdates"),
            "downloaded={downloaded}: nothing of the account follows the retirement: {told:?}",
        );
        assert_eq!(
            told.iter().any(|e| e.starts_with("account_download_end")),
            downloaded,
            "downloaded={downloaded}: the answer precedes the retirement only where it was given: {told:?}",
        );
        assert!(heard(&client).is_empty());
    }
}

/// A request waiting for a download is withdrawn before it is answered.
#[test]
fn a_request_and_its_cancel_taken_together_leave_only_the_withdrawal() {
    let (mut hl, shared, client) = stopped();
    shared.portfolio.account_download_is_pending();
    client.req_positions_multi(7, "", "");
    client.cancel_positions_multi(7);
    hl.poll_once();
    hl.asks.answer_what_is_ready(&shared, &mut { COMMANDS_PER_LAP });
    assert!(heard(&client).is_empty(), "nothing answered, nothing refused");
    assert_eq!(client.backlog(), 0);
}

/// A question the replay decides is held in the engine, not on the caller's
/// thread, and answered once the venue has named what the account is working.
#[test]
fn an_open_orders_question_is_held_for_the_replay_and_the_call_waits_for_nothing() {
    let (mut hl, shared, client) = stopped();
    shared.orders.replay_is_pending();
    let began = std::time::Instant::now();
    client.req_open_orders();
    client.req_ids(1);
    assert!(began.elapsed() < Duration::from_millis(200), "the calls wait for nothing");
    hl.poll_once();
    hl.asks.answer_what_is_ready(&shared, &mut { COMMANDS_PER_LAP });
    assert!(heard(&client).is_empty(), "held while the venue names what is working");
    assert_eq!(client.backlog(), 2);

    shared.orders.set_replay_done();
    hl.poll_once();
    let told = heard(&client);
    assert_eq!(told.first().map(String::as_str), Some("open_order_end"), "{told:?}");
    assert!(told.iter().any(|e| e.starts_with("next_valid_id:")), "{told:?}");
    assert_eq!(client.backlog(), 0);
}

/// A loop with a trading connection and a channel to it.
fn with_trading() -> (HotLoop, Arc<SharedState>, Sender<ControlCommand>, std::net::TcpStream) {
    let shared = Arc::new(SharedState::new());
    let mut hl = HotLoop::new(shared.clone(), None, None);
    let (conn, peer) = Connection::for_test();
    peer.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    hl.ccp_conn = Some(conn);
    hl.set_account_id("DU1".into());
    let (tx, rx) = channel();
    hl.set_control_rx(rx);
    (hl, shared, tx, peer)
}

/// What the trading connection was sent, its fields apart.
fn on_the_wire(peer: &mut std::net::TcpStream) -> String {
    sent(peer).replace('\u{1}', "|")
}

/// A placement of one share at a limit, under `order_id`.
fn placement(
    order_id: u64,
    contract: crate::types::model::Contract,
    parent_id: i64,
    transmit: bool,
) -> ControlCommand {
    ControlCommand::Place(Box::new(crate::types::Placement {
            allocator: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
        order_id,
        contract,
        order: crate::types::model::Order {
            order_id: order_id as i64,
            action: "BUY".into(),
            total_quantity: 1.0,
            order_type: "LMT".into(),
            lmt_price: 100.0,
            tif: "DAY".into(),
            parent_id,
            transmit,
            ..Default::default()
        },
        warnings: Vec::new(),
    }))
}

/// SPY as the venue numbers it.
fn spy() -> crate::types::model::Contract {
    crate::types::model::Contract {
        con_id: 756733,
        symbol: "SPY".into(),
        sec_type: "STK".into(),
        exchange: "SMART".into(),
        currency: "USD".into(),
        ..Default::default()
    }
}

/// SPY as a caller describes it, with no number for the venue to go on.
fn described() -> crate::types::model::Contract {
    crate::types::model::Contract { con_id: 0, ..spy() }
}

#[test]
fn an_orders_client_id_does_not_change_the_session_identity() {
    use crate::types::OrderRequest;
    for session_client in [0, 7] {
        let (mut hl, shared, _, _peer) = with_trading();
        shared.orders.set_replay_done();
        shared.orders.set_api_client_id(session_client);
        for (id, client_id) in [(10, 0), (11, 0), (10, 7)] {
            let ControlCommand::Place(mut p) = placement(id, spy(), 0, true) else { unreachable!() };
            p.order.client_id = client_id;
            if client_id == 7 { p.order.total_quantity = 2.0; }
            hl.take_order_command(ControlCommand::Place(p));
        }
        let refused = shared.drain_refused();
        assert!(refused.is_empty(), "{refused:?}");
        for id in [10, 11] {
            assert_eq!(shared.orders.attached_order_metadata(id).unwrap().api_client_id, Some(session_client));
        }
        let orders: Vec<_> = hl.context.pending_orders.drain().collect();
        assert!(matches!(&orders[..], [
            OrderRequest::SubmitEx { order_id: 10, .. },
            OrderRequest::SubmitEx { order_id: 11, .. },
            OrderRequest::Modify { order_id: 10, qty, .. },
        ] if *qty == crate::types::qty_from_f64(2.0)), "{orders:?}");
    }
}

/// Every order a program places states the program's order and client, and no
/// transact time, as a gateway states every order a program places: one placed
/// alone and each leg of a bracket.
#[test]
fn every_order_a_program_places_states_its_order_and_client_and_no_transact_time() {
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    shared.orders.set_api_client_id(7);
    tx.send(placement(5, spy(), 0, true)).unwrap();
    tx.send(ControlCommand::Bracket(Box::new(crate::types::Bracket {
        contract: spy(), parent_id: 20, side: crate::types::Side::Buy, quantity: 1.0,
        entry: 100.0, take_profit: 110.0, stop_loss: 90.0,
    }))).unwrap();
    hl.poll_once();
    hl.poll_once();
    let wire = on_the_wire(&mut peer);
    let field = |order: &str, tag: &str| {
        order.split('|').find_map(|f| f.strip_prefix(tag)).map(str::to_string)
    };
    let stated: Vec<_> = wire.split("|35=D|").skip(1)
        .map(|order| (field(order, "6121="), field(order, "6119="), field(order, "60=")))
        .collect();
    let program = |id: &str| (Some(id.to_string()), Some("7".to_string()), None);
    assert_eq!(stated, [program("5"), program("20"), program("21"), program("22")], "{wire}");
}

/// A scan's text remains on its request while the venue names the contract,
/// and scans of that contract then take their turns under that name.
#[test]
fn scans_given_by_description_keep_their_text_and_their_turn() {
    let (mut hl, shared, tx, _peer) = with_trading();
    let scan = |req_id, text: &str| ControlCommand::Subscribe {
        req_id,
        contract: (&described()).into(),
        filters: Default::default(),
        mode_9887: 0,
        delayed_mode: None,
        frozen: false,
        delayed_frozen: false,
        regulatory_snapshot: false,
        snapshot: false,
        generic_ticks: vec![481],
        news: None,
        spread_scan: Some(text.into()),
        calculation: None,
    };
    for (id, text) in [(1, "first"), (2, "second"), (3, "withdrawn")] {
        shared.admit(&tx, scan(id, text)).unwrap();
    }
    hl.poll_once();
    assert_eq!(hl.ccp.pending_named.len(), 3, "each whole scan waits for its contract");
    let queries: Vec<_> = hl.ccp.pending_named.iter().map(|(id, ..)| *id).collect();
    shared.admit(&tx, ControlCommand::CancelMktData { req_id: 3 }).unwrap();
    hl.poll_once();
    assert_eq!(shared.backlog(), 2);
    for id in queries {
        let id = id.to_string();
        let frame = crate::protocol::fix::fix_build(
            &[
                (35, "d"),
                (320, &id),
                (323, "4"),
                (55, "SPY"),
                (167, "STK"),
                (6008, "756733"),
                (207, "SMART"),
                (15, "USD"),
            ],
            1,
        );
        hl.ccp.process_ccp_message(
            &frame,
            &mut hl.ccp_conn,
            &mut hl.context,
            &shared,
            &None,
            &mut hl.hb,
            "DU1",
        );
    }
    hl.poll_once();
    assert_eq!(hl.farm.spread_scans.get(&756733).map(String::as_str), Some("first"));
    assert_eq!(shared.backlog(), 1, "the second waits on the first scan's withdrawal");
    assert!(!hl.md_requests.contains_key(&3));
    shared.admit(&tx, ControlCommand::CancelMktData { req_id: 1 }).unwrap();
    hl.poll_once();
    hl.poll_once();
    assert_eq!(hl.farm.spread_scans.get(&756733).map(String::as_str), Some("second"));
    assert!(hl.md_requests.contains_key(&2));
    assert_eq!(shared.backlog(), 0);
}

/// The venue's naming of SPY, answering the lookup an order asked.
fn named_spy(hl: &mut HotLoop) {
    let (asked, _) = hl.ccp.order_naming.remove(0);
    hl.ccp.orders_named.push((
        asked,
        ccp::OrderNamed::Contract(Box::new(crate::control::contracts::ContractDefinition {
            con_id: 756733,
            symbol: "SPY".into(),
            sec_type: crate::control::contracts::SecurityType::Stock,
            exchange: "SMART".into(),
            currency: "USD".into(),
            ..Default::default()
        })),
    ));
}

/// Where a message of this type first appears on the wire.
fn at(wire: &str, msg_type: &str) -> usize {
    wire.find(&format!("35={msg_type}|")).unwrap_or_else(|| panic!("no 35={msg_type} in {wire}"))
}

/// `place_order` of a contract to name, then a disconnect, with no read in
/// between: the logout and the stop taken with it wait for the naming, and
/// the order reaches the wire before the logout.
#[test]
fn an_order_being_named_at_the_stop_goes_out_before_the_logout() {
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(5, described(), 0, true)).unwrap();
    tx.send(ControlCommand::Logout).unwrap();
    tx.send(ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(hl.is_running(), "the stop taken in the same lap waits for the finishing phase");
    assert!(!on_the_wire(&mut peer).contains("35=5|"), "and nothing is logged out yet");

    named_spy(&mut hl);
    hl.poll_once();
    assert!(hl.is_running(), "newly built orders wait for the next lap's allowance");
    hl.poll_once();
    assert!(!hl.is_running(), "the phase is over, and the stop has run");
    let wire = on_the_wire(&mut peer);
    assert!(at(&wire, "D") < at(&wire, "5"), "the order goes before the logout: {wire}");
    assert!(shared.drain_refused().is_empty());
}

/// The same with no answer to the naming: the order is refused under its
/// number once the naming's bound has passed, and then the logout goes.
#[test]
fn an_order_whose_naming_is_never_answered_is_refused_before_the_logout() {
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(5, described(), 0, true)).unwrap();
    tx.send(ControlCommand::Logout).unwrap();
    tx.send(ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(hl.is_running());

    hl.ccp.order_naming[0].1 -= ccp::CcpState::NAMING_TIMEOUT + Duration::from_secs(1);
    hl.ccp.sweep_pending_named(&shared);
    hl.poll_once();
    assert!(!hl.is_running());
    let refused = shared.drain_refused();
    assert!(matches!(refused.as_slice(), [(5, ..)]), "refused under its number: {refused:?}");
    let wire = on_the_wire(&mut peer);
    assert!(!wire.contains("35=D|"), "nothing was placed: {wire}");
    assert!(wire.contains("35=5|"), "and the session is logged out: {wire}");
}

/// A cancel taken while the venue is still naming the working set, then a
/// disconnect: the cancel reaches the wire before the logout.
#[test]
fn a_cancel_waiting_on_the_replay_at_the_stop_goes_out_before_the_logout() {
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.replay_is_pending();
    shared.orders.push_order_info(
        9,
        crate::bridge::RichOrderInfo {
            contract: spy(),
            order: crate::types::model::Order { order_id: 9, ..Default::default() },
            order_state: crate::types::model::OrderState {
                status: "Submitted".into(),
                ..Default::default()
            },
            last_exec: Default::default(),
        },
    );
    tx.send(ControlCommand::CancelOrder { order_id: 9, stated: Default::default() }).unwrap();
    tx.send(ControlCommand::Logout).unwrap();
    tx.send(ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(hl.is_running(), "the cancel waits for the naming, and the stop for the cancel");

    shared.orders.set_replay_done();
    hl.poll_once();
    assert!(hl.is_running(), "newly built orders wait for the next lap's allowance");
    hl.poll_once();
    assert!(!hl.is_running());
    let wire = on_the_wire(&mut peer);
    assert!(at(&wire, "F") < at(&wire, "5"), "the cancel goes before the logout: {wire}");
}

/// A family kept for a later transmit goes out whole, in the order it was
/// placed, when its last member transmits at the stop. An order kept alone
/// is never sent by the stop.
#[test]
fn a_kept_family_goes_out_in_order_and_a_kept_order_alone_stays_kept() {
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(10, spy(), 0, false)).unwrap();
    tx.send(placement(11, spy(), 10, true)).unwrap();
    tx.send(ControlCommand::Logout).unwrap();
    tx.send(ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(hl.is_running(), "newly built orders wait for the next lap's allowance");
    hl.poll_once();
    assert!(!hl.is_running());
    let wire = on_the_wire(&mut peer);
    let parent = wire.find("|11=10.").expect("the parent went out");
    let child = wire.find("|11=11.").expect("the child went out");
    assert!(
        parent < child && child < at(&wire, "5"),
        "in the order placed, then the logout: {wire}"
    );

    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(12, spy(), 0, false)).unwrap();
    tx.send(ControlCommand::Logout).unwrap();
    tx.send(ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(!hl.is_running());
    let wire = on_the_wire(&mut peer);
    assert!(!wire.contains("35=D|"), "an order kept alone is not sent by the stop: {wire}");
    assert!(shared.drain_refused().is_empty(), "nor refused");
    assert!(shared.orders.drain_order_inactive().is_empty());
}

/// Children placed separately cancel together and retain that link on a change.
#[test]
fn individually_placed_children_share_the_parents_group() {
    for held in [false, true] {
        let (mut hl, shared, tx, mut peer) = with_trading();
        shared.orders.set_replay_done();
        tx.send(placement(10, spy(), 0, !held)).unwrap();
        let mut take_profit = placement(11, spy(), 10, !held);
        let mut stop = placement(12, spy(), 10, true);
        if let ControlCommand::Place(order) = &mut take_profit {
            order.order.action = "SELL".into();
            order.order.lmt_price = 110.0;
        }
        if let ControlCommand::Place(order) = &mut stop {
            order.order.action = "SELL".into();
            order.order.order_type = "STP".into();
            order.order.aux_price = 90.0;
            order.order.tif = "GTC".into();
            if held {
                order.order.adjusted_order_type = "STP".into();
                order.order.trigger_price = 92.0;
                order.order.adjusted_stop_price = 93.0;
            }
        }
        tx.send(take_profit.clone()).unwrap();
        tx.send(stop).unwrap();
        hl.poll_once();
        hl.poll_once();
        let wire = on_the_wire(&mut peer);
        for id in [10, 11, 12] {
            let name = format!("|11={id}.0|");
            let frame = wire.split("8=FIX").find(|frame| frame.contains(&name)).unwrap();
            if id == 10 {
                assert!(!frame.contains("|583="), "the parent stands alone: {frame}");
            } else {
                assert!(frame.contains("|6107=10.0|"), "{frame}");
                assert!(frame.contains("|583=10|6209=ReduceOnFillNonBlock|"), "{frame}");
            }
        }
        // And recorded as the wire states them, for a caller reading them back.
        let recorded: Vec<_> = shared
            .take_records(shared.next_seq(), crate::bridge::Take::Dispatch { bulletins: false })
            .into_iter()
            .filter_map(|(_, record)| match record {
                crate::bridge::Record::OrderBook(crate::bridge::OrderBook::Taken(taken)) if taken.order_id != 10 => {
                    Some((taken.order_id, taken.order.oca_group, taken.order.oca_type))
                }
                _ => None,
            })
            .collect();
        assert_eq!(recorded, [(11, "10".to_string(), 3), (12, "10".to_string(), 3)]);
        let stop = wire.split("8=FIX").find(|frame| frame.contains("|11=12.0|")).unwrap();
        for field in ["|40=3|", "|99=90|", "|59=1|"] {
            assert!(stop.contains(field), "{stop}");
        }
        if held {
            for field in ["|6257=1|", "|6258=92|", "|6259=93|"] {
                assert!(stop.contains(field), "{stop}");
            }
        }
        if let ControlCommand::Place(order) = &mut take_profit {
            order.order.transmit = true;
            order.order.lmt_price = 111.0;
        }
        tx.send(take_profit).unwrap();
        hl.poll_once();
        hl.poll_once();
        let wire = on_the_wire(&mut peer);
        assert!(wire.contains("|35=G|"), "{wire}");
        assert!(wire.contains("|583=10|6209=ReduceOnFillNonBlock|"), "{wire}");
        assert!(wire.contains("|6107=10.0|"), "{wire}");
        assert!(shared.drain_refused().is_empty());
    }
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(ControlCommand::Bracket(Box::new(crate::types::Bracket {
        contract: spy(), parent_id: 10, side: crate::types::Side::Buy,
        quantity: 1.0, entry: 100.0, take_profit: 110.0, stop_loss: 90.0,
    }))).unwrap();
    tx.send(placement(13, spy(), 10, true)).unwrap();
    hl.poll_once();
    hl.poll_once();
    let wire = on_the_wire(&mut peer);
    for id in [11, 12, 13] {
        let name = format!("|11={id}.0|");
        let frame = wire.split("8=FIX").find(|frame| frame.contains(&name)).unwrap();
        assert!(frame.contains("|583=10|"), "every child shares the group: {frame}");
        assert!(frame.contains("|6209=ReduceOnFillNonBlock|"), "{frame}");
        assert!(frame.contains("|6107=10.0|"), "{frame}");
    }

    // A scale order's child keeps the group its scale order gives it.
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    let mut parent = placement(10, spy(), 0, true);
    if let ControlCommand::Place(order) = &mut parent {
        order.order.scale_init_level_size = 100;
    }
    let mut child = placement(11, spy(), 10, true);
    if let ControlCommand::Place(order) = &mut child {
        order.order.action = "SELL".into();
        order.order.scale_profit_offset = 1.0;
    }
    tx.send(parent).unwrap();
    tx.send(child).unwrap();
    hl.poll_once();
    hl.poll_once();
    let wire = on_the_wire(&mut peer);
    let child = wire.split("8=FIX").find(|frame| frame.contains("|11=11.0|")).unwrap();
    assert!(child.contains("|6107=10.0|") && !child.contains("|583=10|"), "{child}");
}

/// A placement kept for a later transmit remains unsent work until its
/// withdrawal or the session's end, even though its contract is already known.
#[test]
fn a_placement_kept_for_transmit_counts_until_its_withdrawal() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    shared.admit(&tx, placement(12, spy(), 0, false)).unwrap();
    hl.poll_once();
    assert_eq!(shared.backlog(), 1);
    shared
        .admit(&tx, ControlCommand::CancelOrder { order_id: 12, stated: Default::default() })
        .unwrap();
    hl.poll_once();
    assert_eq!(shared.backlog(), 0);

    shared.admit(&tx, placement(13, spy(), 0, false)).unwrap();
    hl.poll_once();
    assert_eq!(shared.backlog(), 1);
    shared.admit(&tx, ControlCommand::Logout).unwrap();
    shared.admit(&tx, ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(!hl.is_running());
    assert_eq!(shared.backlog(), 0, "the session's end withdraws what was kept");
}

/// A parent whose naming fails is refused under its own number, and the
/// session still finishes.
#[test]
fn a_parent_whose_naming_fails_at_the_stop_is_refused_under_its_number() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(20, described(), 0, false)).unwrap();
    tx.send(placement(21, spy(), 20, true)).unwrap();
    tx.send(ControlCommand::Logout).unwrap();
    tx.send(ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(hl.is_running(), "the child waits behind its parent, and the stop behind both");

    let (asked, _) = hl.ccp.order_naming.remove(0);
    hl.ccp.orders_named.push((asked, ccp::OrderNamed::Unnamed(0)));
    hl.poll_once();
    assert!(hl.is_running(), "newly built orders wait for the next lap's allowance");
    hl.poll_once();
    assert!(!hl.is_running());
    let refused = shared.drain_refused();
    assert!(
        refused.iter().any(|(id, ..)| *id == 20),
        "the parent is refused under its number: {refused:?}"
    );
}

/// A question held for the account's download and a request held for its
/// contract's name at the stop are withdrawn in silence: the session's end
/// is theirs.
#[test]
fn what_is_held_at_the_stop_that_is_not_an_order_is_withdrawn_in_silence() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.portfolio.account_download_is_pending();
    tx.send(ControlCommand::Ask(crate::types::Ask::Positions)).unwrap();
    tx.send(ControlCommand::FetchHistorical {
        contract: ContractRef { con_id: 0, ..stock() },
        req_id: 3,
        end_date_time: String::new(),
        duration: "1 D".into(),
        bar_size: "1 hour".into(),
        filters: Default::default(),
        what_to_show: "TRADES".into(),
        use_rth: true,
        keep_up_to_date: false,
        format_date: 1,
        include_expired: false,
    })
    .unwrap();
    tx.send(ControlCommand::Logout).unwrap();
    tx.send(ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(!hl.is_running());
    assert!(shared.drain_refused().is_empty(), "nothing is refused");
    assert!(shared.reference.drain_historical_errors().is_empty(), "nothing is said for the bars");
    assert_eq!(hl.asks.len(), 0);
}

/// A caller that drops every sender runs the same finishing phase: the order
/// being named goes out, then the logout.
#[test]
fn a_dropped_channel_runs_the_same_finishing_phase() {
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(5, described(), 0, true)).unwrap();
    drop(tx);
    hl.poll_once();
    assert!(hl.is_running(), "the order is still being named");

    named_spy(&mut hl);
    hl.poll_once();
    assert!(hl.is_running(), "newly built orders wait for the next lap's allowance");
    hl.poll_once();
    assert!(!hl.is_running());
    let wire = on_the_wire(&mut peer);
    assert!(at(&wire, "D") < at(&wire, "5"), "the order, then the logout: {wire}");
}

/// An order command read behind the stop, or after the logout was written,
/// is refused under its number rather than dropped unsaid.
#[test]
fn an_order_after_the_logout_or_the_stop_is_refused_under_its_number() {
    let (mut hl, shared, tx, mut peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(ControlCommand::Logout).unwrap();
    hl.poll_once();
    assert!(on_the_wire(&mut peer).contains("35=5|"), "nothing to finish, so the logout goes");
    tx.send(placement(8, spy(), 0, true)).unwrap();
    hl.poll_once();
    tx.send(ControlCommand::Shutdown).unwrap();
    tx.send(placement(9, spy(), 0, true)).unwrap();
    hl.poll_once();
    assert!(!hl.is_running());
    let refused: Vec<u64> =
        shared.orders.drain_order_inactive().into_iter().map(|note| note.0).collect();
    assert_eq!(refused, [8, 9], "each is refused under its number");
    assert!(!on_the_wire(&mut peer).contains("35=D|"), "and neither went out");
}

/// A recovery that halts refuses the order commands the loop holds and
/// withdraws its questions in silence, as a stop does.
#[test]
fn a_halted_recovery_refuses_the_held_orders_and_withdraws_the_rest_in_silence() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    shared.portfolio.account_download_is_pending();
    tx.send(placement(5, described(), 0, true)).unwrap();
    tx.send(ControlCommand::Ask(crate::types::Ask::Positions)).unwrap();
    hl.poll_once();
    hl.halt_recovery(retry::DisconnectReason::TakenOver);
    hl.poll_once();
    assert!(!hl.is_running());
    let refused: Vec<u64> =
        shared.orders.drain_order_inactive().into_iter().map(|note| note.0).collect();
    assert_eq!(refused, [5], "the order being named is refused under its number");
    assert!(shared.drain_refused().is_empty(), "and the question is withdrawn in silence");
}

/// What the loop has built for the venue since this was last asked, in the
/// order it built it: each request's order number and what it does.
fn built(hl: &mut HotLoop) -> Vec<(u64, &'static str)> {
    hl.context_mut()
        .drain_pending_orders()
        .map(|req| {
            let kind = match req {
                crate::types::OrderRequest::SubmitEx { .. } => "place",
                crate::types::OrderRequest::Modify { .. } => "modify",
                crate::types::OrderRequest::Cancel { .. } => "cancel",
                _ => "other",
            };
            (req.order_id(), kind)
        })
        .collect()
}

/// An order waiting for its contract to be named holds up only what depends
/// on it: another order goes at once, and the one held goes once named.
#[test]
fn a_placement_held_for_naming_holds_up_nothing_else() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(5, described(), 0, true)).unwrap();
    tx.send(placement(6, spy(), 0, true)).unwrap();
    hl.poll_once();
    assert_eq!(
        built(&mut hl),
        [(6, "place")],
        "the other order goes at once, and the one being named waits"
    );

    named_spy(&mut hl);
    hl.poll_once();
    assert_eq!(built(&mut hl), [(5, "place")], "it goes once named");
    assert!(shared.drain_refused().is_empty());
}

/// A modify of an order still being named waits behind its placement and
/// goes after it.
#[test]
fn a_modify_behind_a_held_placement_goes_out_after_it() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(5, described(), 0, true)).unwrap();
    // Stating the contract by its number, so nothing but the order ahead of
    // it holds it.
    let mut revised = placement(5, spy(), 0, true);
    if let ControlCommand::Place(p) = &mut revised {
        p.order.lmt_price = 101.0;
    }
    tx.send(revised).unwrap();
    hl.poll_once();
    assert_eq!(built(&mut hl), [], "the modify waits behind the placement being named");

    named_spy(&mut hl);
    hl.poll_once();
    assert_eq!(built(&mut hl), [(5, "place"), (5, "modify")], "placed, then revised");
    assert!(shared.drain_refused().is_empty());
}

/// A cancel of an order still being named withdraws it with the modify
/// behind it, and nothing of either is built — the naming's answer included.
#[test]
fn a_cancel_behind_a_held_placement_withdraws_it_and_its_modify() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(5, described(), 0, true)).unwrap();
    tx.send(placement(5, described(), 0, true)).unwrap();
    tx.send(ControlCommand::CancelOrder { order_id: 5, stated: Default::default() }).unwrap();
    hl.poll_once();
    assert_eq!(hl.intake.waiting(), 0, "nothing of the order is left waiting");

    named_spy(&mut hl);
    hl.poll_once();
    assert_eq!(built(&mut hl), [], "nothing reaches the venue");
}

/// A child whose parent is still being named waits behind it, and the family
/// goes out in the order it was placed.
#[test]
fn a_child_waits_behind_its_held_parent() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    tx.send(placement(20, described(), 0, false)).unwrap();
    tx.send(placement(21, spy(), 20, true)).unwrap();
    hl.poll_once();
    assert_eq!(built(&mut hl), [], "the child waits behind its parent");

    named_spy(&mut hl);
    hl.poll_once();
    assert_eq!(built(&mut hl), [(20, "place"), (21, "place")], "in the order placed");
    assert!(shared.drain_refused().is_empty());
}

/// An SPY call as the venue numbers it.
fn spy_call() -> crate::types::model::Contract {
    crate::types::model::Contract {
        con_id: 700_001,
        symbol: "SPY".into(),
        sec_type: "OPT".into(),
        exchange: "SMART".into(),
        currency: "USD".into(),
        last_trade_date_or_contract_month: "20261218".into(),
        strike: 500.0,
        right: "C".into(),
        multiplier: "100".into(),
        ..Default::default()
    }
}

/// An exercise (1) or a lapse (2) of `qty` contracts of the call, under
/// `order_id`, overriding the natural action or not.
fn exercise(order_id: u64, action: u8, qty: i64, override_: bool) -> ControlCommand {
    ControlCommand::Exercise(Box::new(crate::types::Exercise {
        allocator: None,
        req_id: order_id as i64,
        order_id,
        stated: false,
        contract: spy_call(),
        action,
        qty: crate::types::qty_from_wire(qty),
        account: String::new(),
        states: Default::default(),
        override_,
    }))
}

/// A trading loop whose account has stated it holds `held` of the call, with
/// the call's slot and, where given, its in-the-money figure already held.
fn holding_the_call(
    held: f64,
    figure: Option<[f64; 3]>,
) -> (HotLoop, Arc<SharedState>, Sender<ControlCommand>, InstrumentId) {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    shared.portfolio.account_download_is_settled();
    let c = spy_call();
    shared.portfolio.set_position_info(crate::types::PositionInfo {
        con_id: c.con_id,
        position: held,
        ..Default::default()
    });
    let identity = crate::types::model::contract_identity(
        &c.last_trade_date_or_contract_month,
        c.strike,
        &c.right,
        &c.multiplier,
        &c.currency,
    );
    let slot =
        hl.register_contract(c.con_id, c.symbol.clone(), &c.sec_type, &c.exchange, &identity, "");
    if let Some(figure) = figure {
        shared.market.note_stated_figures(slot, 493, figure.to_vec());
    }
    (hl, shared, tx, slot)
}

/// What the loop built for the venue as exercises: each one's number, action
/// and how many contracts.
fn exercised(hl: &mut HotLoop) -> Vec<(u64, u8, f64)> {
    hl.context_mut()
        .drain_pending_orders()
        .filter_map(|req| match req {
            crate::types::OrderRequest::SubmitEx { order_id, qty, attrs, .. } => {
                Some((order_id, attrs.exercise_action, crate::types::qty_to_f64(qty)))
            }
            _ => None,
        })
        .collect()
}

/// The watches the engine opened for itself, on what they watch.
fn own_watches(hl: &HotLoop) -> Vec<(i64, Vec<u32>)> {
    hl.md_requests
        .iter()
        .filter(|(req_id, _)| crate::engine::hot_loop::intake::engine_owned(**req_id))
        .map(|(req_id, req)| (*req_id, req.series.clone()))
        .collect()
}

const NOT_IN_THE_MONEY: &str =
    "Error processing request:Exercise ignored because option is not in-the-money.";
const IN_THE_MONEY: &str = "Error processing request:Lapse ignored because option is in-the-money.";

/// An option out of the money, by its in-the-money figure: an exercise with
/// the override off is refused under 322 in a gateway's words, and one with
/// it on goes.
#[test]
fn an_exercise_of_an_option_out_of_the_money_goes_only_with_the_override() {
    let (mut hl, shared, tx, _) = holding_the_call(2.0, Some([-0.5, 1.0, 0.2]));
    tx.send(exercise(5, 1, 1, false)).unwrap();
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [], "nothing goes to the venue");
    let refused = shared.drain_refused();
    assert_eq!(refused, [(5, 322, NOT_IN_THE_MONEY.to_string())]);

    tx.send(exercise(6, 1, 1, true)).unwrap();
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [(6, 1, 1.0)], "the override lets it go");
    assert!(shared.drain_refused().is_empty());
    assert!(own_watches(&hl).is_empty(), "a figure already held is used, and nothing is watched");
}

/// An option in the money: an exercise goes, and a lapse is refused with the
/// override off and goes with it on.
#[test]
fn a_lapse_of_an_option_in_the_money_goes_only_with_the_override() {
    let (mut hl, shared, tx, _) = holding_the_call(2.0, Some([1.5, 1.0, 0.2]));
    tx.send(exercise(5, 1, 1, false)).unwrap();
    tx.send(exercise(6, 2, 1, false)).unwrap();
    tx.send(exercise(7, 2, 1, true)).unwrap();
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [(5, 1, 1.0), (7, 2, 1.0)]);
    assert_eq!(shared.drain_refused(), [(6, 322, IN_THE_MONEY.to_string())]);
}

/// An account holding none of the option is refused under 322 in a gateway's
/// words, and nothing is watched for it.
#[test]
fn an_exercise_on_an_account_holding_none_is_refused() {
    let (mut hl, shared, tx, _) = holding_the_call(0.0, None);
    tx.send(exercise(5, 1, 1, true)).unwrap();
    hl.poll_once();
    assert_eq!(exercised(&mut hl), []);
    assert_eq!(
        shared.drain_refused(),
        [(
            5,
            322,
            "Error processing request:No unlapsed position exists in this option in account DU1."
                .to_string()
        )],
    );
    assert!(own_watches(&hl).is_empty());
}

/// More than the account holds is clamped to what it holds.
#[test]
fn an_exercise_of_more_than_is_held_goes_for_what_is_held() {
    let (mut hl, shared, tx, _) = holding_the_call(3.0, Some([1.5, 1.0, 0.2]));
    tx.send(exercise(5, 1, 10, false)).unwrap();
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [(5, 1, 3.0)]);
    assert!(shared.drain_refused().is_empty());
}

/// With no figure held, the option is watched for one, under a number of the
/// engine's own that nothing is said of; the exercise waits, with no bound,
/// until a figure that says whether it is in the money arrives, and the watch
/// is withdrawn then.
#[test]
fn an_exercise_waits_for_its_options_figure_and_the_watch_goes_with_the_answer() {
    let (mut hl, shared, tx, slot) = holding_the_call(2.0, None);
    tx.send(exercise(5, 1, 1, false)).unwrap();
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [], "it waits");
    let watches = own_watches(&hl);
    assert_eq!(watches.len(), 1, "one watch: {watches:?}");
    assert_eq!(watches[0].1, [493], "on the option's in-the-money series");
    assert!(shared.drain_refused().is_empty());

    // A figure that does not say whether it is in the money is not the answer.
    shared.market.note_stated_figures(slot, 493, vec![1.5, 0.0, 0.2]);
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [], "still waiting");

    shared.market.note_stated_figures(slot, 493, vec![1.5, 1.0, 0.2]);
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [(5, 1, 1.0)], "decided on the figure that came");
    assert!(own_watches(&hl).is_empty(), "and the watch is withdrawn");
    assert!(shared.drain_refused().is_empty(), "nothing is said of the watch");
}

/// An exercise still waiting for its figure at `disconnect()` is refused once,
/// with the stop's words, and the logout follows.
#[test]
fn an_exercise_waiting_for_its_figure_at_the_stop_is_refused_once() {
    let (mut hl, shared, tx, _) = holding_the_call(2.0, None);
    tx.send(exercise(5, 1, 1, false)).unwrap();
    hl.poll_once();
    assert_eq!(own_watches(&hl).len(), 1);
    tx.send(ControlCommand::Logout).unwrap();
    tx.send(ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(!hl.is_running(), "nothing bounds the wait, so the stop does not wait for it");
    let refused = shared.drain_refused();
    assert_eq!(refused.len(), 1, "refused once: {refused:?}");
    assert_eq!(refused[0].0, 5);
    assert!(refused[0].2.starts_with("the engine stopped"), "{refused:?}");
    assert_eq!(exercised(&mut hl), []);
}

#[test]
fn an_exercise_uses_the_named_accounts_position_and_clamp() {
    let (mut hl, shared, tx, _) = holding_the_call(9.0, Some([1.5, 1.0, 0.2]));
    shared.portfolio_for("DU2").set_position_info(crate::types::PositionInfo {
        con_id: spy_call().con_id,
        position: 2.0,
        ..Default::default()
    });
    let ControlCommand::Exercise(mut e) = exercise(5, 1, 8, false) else { unreachable!() };
    e.account = "DU2".into();
    tx.send(ControlCommand::Exercise(e.clone())).unwrap();
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [(5, 1, 2.0)]);
    e.order_id = 6;
    e.req_id = 6;
    e.account = "DU3".into();
    tx.send(ControlCommand::Exercise(e)).unwrap();
    hl.poll_once();
    assert!(exercised(&mut hl).is_empty());
    assert_eq!(
        shared.drain_refused(),
        [(
            6,
            322,
            "Error processing request:No unlapsed position exists in this option in account DU3."
                .into()
        )]
    );
}

#[test]
fn an_exercise_reads_the_attribute_and_invalid_values_as_a_gateway_does() {
    for (value, attribute, exercise_goes, lapse_goes) in [
        (0.0, 1.0, true, true),
        (0.0, 0.0, false, false),
        (-0.5, 1.0, false, true),
        (1.5, 1.0, true, false),
        (f64::MAX, 1.0, false, true),
        (f64::NAN, 1.0, false, true),
        (f64::INFINITY, 1.0, false, true),
        (f64::NEG_INFINITY, 1.0, false, true),
    ] {
        for (action, goes) in [(1, exercise_goes), (2, lapse_goes)] {
            let (mut hl, shared, tx, _) = holding_the_call(2.0, Some([value, attribute, 0.2]));
            tx.send(exercise(5, action, 1, false)).unwrap();
            hl.poll_once();
            assert_eq!(!exercised(&mut hl).is_empty(), goes, "{value:?}, {attribute}, {action}");
            assert_eq!(shared.drain_refused().is_empty(), goes);
        }
    }
}

#[test]
fn an_overridden_exercise_still_waits_for_its_options_figure() {
    let (mut hl, shared, tx, slot) = holding_the_call(2.0, None);
    shared.admit(&tx, exercise(5, 1, 1, true)).unwrap();
    hl.poll_once();
    assert_eq!(shared.backlog(), 1);
    assert!(exercised(&mut hl).is_empty());
    assert_eq!(own_watches(&hl).len(), 1);
    shared.market.note_stated_figures(slot, 493, vec![-0.5, 1.0, 0.2]);
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [(5, 1, 1.0)]);
    assert!(own_watches(&hl).is_empty());
    assert!(shared.drain_refused().is_empty());
}

#[test]
fn an_exercise_clamps_to_whole_contracts_in_the_saved_position() {
    let (mut hl, shared, tx, slot) = holding_the_call(2.5, None);
    tx.send(exercise(5, 1, 8, false)).unwrap();
    hl.poll_once();
    assert_eq!(own_watches(&hl).len(), 1);
    shared.portfolio.set_position_info(crate::types::PositionInfo {
        con_id: spy_call().con_id,
        position: 1.0,
        ..Default::default()
    });
    shared.market.note_stated_figures(slot, 493, vec![1.5, 1.0, 0.2]);
    hl.poll_once();
    assert_eq!(exercised(&mut hl), [(5, 1, 2.0)]);
}

#[test]
fn a_snapshot_is_registered_after_its_option_is_named() {
    for by_id in [true, false] {
        let (mut hl, shared, tx, _peer) = with_trading();
        let contract = if by_id {
            ContractRef { con_id: 700001, ..Default::default() }
        } else {
            ContractRef { symbol: "SPY".into(), exchange: "SMART".into(), ..Default::default() }
        };
        shared
            .admit(
                &tx,
                ControlCommand::Subscribe {
                    req_id: 17,
                    contract,
                    filters: Default::default(),
                    mode_9887: 0,
                    delayed_mode: None,
                    frozen: false,
                    delayed_frozen: false,
                    regulatory_snapshot: false,
                    snapshot: true,
                    generic_ticks: Vec::new(),
                    news: None,
                    spread_scan: None,
                    calculation: None,
                },
            )
            .unwrap();
        hl.poll_once();
        assert!(hl.md_requests.is_empty(), "registration waits for the contract's name");
        assert_eq!(shared.backlog(), 1);
        let query = hl.ccp.pending_named[0].0.to_string();
        let frame = crate::protocol::fix::fix_build(
            &[
                (35, "d"),
                (320, &query),
                (323, "4"),
                (55, "SPY"),
                (167, "OPT"),
                (6008, "700001"),
                (207, "SMART"),
                (15, "USD"),
            ],
            1,
        );
        hl.ccp.process_ccp_message(
            &frame,
            &mut hl.ccp_conn,
            &mut hl.context,
            &shared,
            &None,
            &mut hl.hb,
            "DU1",
        );
        hl.poll_once();
        let taken = shared
            .take_records(shared.next_seq(), crate::bridge::Take::Whole { bulletins: false })
            .into_iter()
            .find_map(|(_, record)| match record {
                crate::bridge::Record::MarketDataTaken(taken) if taken.req_id == 17 => Some(taken),
                _ => None,
            })
            .expect("the named option is registered");
        assert!(taken.marked, "the venue's option type sets the snapshot mask");
        let core = crate::client_core::ClientCore::new();
        core.note_mkt_data_taken(&shared, &taken);
        for tick in [1, 2, 4, 9, 14] {
            core.note_snapshot_tick(17, tick);
        }
        assert!(core.check_snapshot_done(17).is_none(), "the option still needs its computations");
        assert_eq!(shared.backlog(), 0);
    }
}

fn subscription_to_name(calculation: bool) -> ControlCommand {
    ControlCommand::Subscribe {
        req_id: 17,
        contract: ContractRef { con_id: 700001, ..Default::default() },
        filters: Default::default(),
        mode_9887: 0,
        delayed_mode: None,
        frozen: false,
        delayed_frozen: false,
        regulatory_snapshot: false,
        snapshot: false,
        generic_ticks: Vec::new(),
        news: None,
        spread_scan: None,
        calculation: calculation.then(|| {
            Box::new(crate::types::Calculation {
                contract: crate::api::Contract { con_id: 700001, ..Default::default() },
                wants_volatility: false,
                option_price: 0.2,
                under_price: 100.0,
            })
        }),
    }
}

fn name_the_option(hl: &mut HotLoop, query: u32) {
    let frame = crate::protocol::fix::fix_build(
        &[
            (35, "d"),
            (320, &query.to_string()),
            (323, "4"),
            (55, "SPY"),
            (167, "OPT"),
            (6008, "700001"),
            (207, "SMART"),
            (15, "USD"),
            (202, "100"),
            (201, "1"),
            (541, "20270115"),
        ],
        1,
    );
    hl.ccp.process_ccp_message(
        &frame,
        &mut hl.ccp_conn,
        &mut hl.context,
        &hl.shared,
        &None,
        &mut hl.hb,
        "DU1",
    );
}

#[test]
fn a_calculation_keeps_the_terms_the_venue_named() {
    for quote_already_open in [false, true] {
        let (mut hl, shared, tx, _peer) = with_trading();
        if quote_already_open {
            let mut quote = subscription_to_name(false);
            if let ControlCommand::Subscribe { contract, .. } = &mut quote {
                contract.sec_type = "OPT".into();
                contract.exchange = "SMART".into();
            }
            shared.admit(&tx, quote).unwrap();
            hl.poll_once();
        }
        shared.admit(&tx, subscription_to_name(true)).unwrap();
        hl.poll_once();
        let query = hl.ccp.pending_named[0].0;
        name_the_option(&mut hl, query);
        hl.poll_once();
        let asked = shared.market.forget_calculation(17).expect("the model is still needed");
        assert_eq!(asked.contract.con_id, 700001);
        assert_eq!(asked.contract.strike, 100.0);
        assert_eq!(asked.contract.right, "C");
        assert_eq!(asked.contract.last_trade_date_or_contract_month, "20270115");
    }
}

#[test]
fn a_withdrawn_calculation_or_quote_opens_nothing_when_its_name_arrives() {
    for calculation in [false, true] {
        let (mut hl, shared, tx, _peer) = with_trading();
        shared.admit(&tx, subscription_to_name(calculation)).unwrap();
        hl.poll_once();
        let query = hl.ccp.pending_named[0].0;
        assert_eq!(shared.backlog(), 1);
        shared
            .admit(
                &tx,
                if calculation {
                    ControlCommand::CancelCalculation { req_id: 17 }
                } else {
                    ControlCommand::CancelMktData { req_id: 17 }
                },
            )
            .unwrap();
        hl.poll_once();
        assert!(hl.ccp.pending_named.is_empty());
        assert_eq!(shared.backlog(), 0);
        name_the_option(&mut hl, query);
        hl.poll_once();
        assert!(hl.md_requests.is_empty());
        assert!(shared.market.forget_calculation(17).is_none());
        assert!(shared.drain_refused().is_empty());
    }
}

#[test]
fn a_duplicate_quote_number_cannot_open_a_second_lookup() {
    for first_is_named in [false, true] {
        let (mut hl, shared, tx, _peer) = with_trading();
        shared.admit(&tx, subscription_to_name(false)).unwrap();
        hl.poll_once();
        if first_is_named {
            let query = hl.ccp.pending_named[0].0;
            name_the_option(&mut hl, query);
            hl.poll_once();
        }
        shared.admit(&tx, subscription_to_name(false)).unwrap();
        hl.poll_once();
        let refused = shared.drain_refused();
        assert_eq!(refused.len(), 1, "the second request is refused at once");
        assert_eq!((refused[0].0, refused[0].1), (17, 102));
        assert_eq!(hl.ccp.pending_named.len(), usize::from(!first_is_named));
        shared.admit(&tx, ControlCommand::CancelMktData { req_id: 17 }).unwrap();
        hl.poll_once();
        assert!(hl.ccp.pending_named.is_empty());
        assert!(hl.md_requests.is_empty());
        assert_eq!(shared.backlog(), 0);
    }
}

#[test]
fn an_exercise_is_named_before_its_position_and_figure_are_read() {
    for by_id in [false, true] {
        let (mut hl, shared, tx, mut peer) = with_trading();
        shared.portfolio.set_position_info(crate::types::PositionInfo {
            con_id: 700001,
            position: 2.0,
            ..Default::default()
        });
        let ControlCommand::Exercise(mut asked) = exercise(5, 1, 8, false) else { unreachable!() };
        if by_id {
            asked.contract = crate::api::Contract { con_id: 700001, ..Default::default() };
        } else {
            asked.contract.con_id = 0;
        }
        shared.admit(&tx, ControlCommand::Exercise(asked)).unwrap();
        hl.poll_once();
        assert_eq!(hl.ccp.order_naming.len(), 1, "naming precedes the position check");
        assert!(shared.drain_refused().is_empty());
        assert_eq!(hl.context.market.active_instruments().count(), 0);
        assert_eq!(shared.backlog(), 1);
        let wire = on_the_wire(&mut peer);
        assert!(wire.contains("|35=c|"));
        if by_id {
            assert!(wire.contains("|6008=700001|"), "{wire}");
        } else {
            assert!(wire.contains("|55=SPY|"), "{wire}");
        }
        let query = hl.ccp.order_naming[0].0;
        name_the_option(&mut hl, query);
        hl.poll_control_commands();
        let slot = hl.context.market.instrument_by_con_id(700001).unwrap();
        assert_eq!(own_watches(&hl).len(), 1, "the named option is watched for 493");
        assert_eq!(shared.backlog(), 1, "the internal watch is not counted twice");
        shared.market.note_stated_figures(slot, 493, vec![1.5, 1.0, 0.2]);
        hl.poll_control_commands();
        assert_eq!(exercised(&mut hl), [(5, 1, 2.0)]);
        assert!(own_watches(&hl).is_empty());
        assert!(shared.drain_refused().is_empty());
    }
}

#[test]
fn an_exercises_naming_timeout_is_its_one_refusal_before_logout() {
    let (mut hl, shared, tx, mut peer) = with_trading();
    let ControlCommand::Exercise(mut asked) = exercise(5, 1, 1, false) else { unreachable!() };
    asked.contract.con_id = 0;
    shared.admit(&tx, ControlCommand::Exercise(asked)).unwrap();
    shared.admit(&tx, ControlCommand::Logout).unwrap();
    shared.admit(&tx, ControlCommand::Shutdown).unwrap();
    hl.poll_once();
    assert!(hl.is_running(), "finishing still reads the naming answer");
    assert!(!on_the_wire(&mut peer).contains("|35=5|"));
    hl.ccp.order_naming[0].1 -= ccp::CcpState::NAMING_TIMEOUT + Duration::from_secs(1);
    hl.ccp.sweep_pending_named(&shared);
    hl.poll_once();
    assert!(!hl.is_running());
    let records =
        shared.take_records(shared.next_seq(), crate::bridge::Take::Whole { bulletins: false });
    let refused: Vec<_> = records
        .iter()
        .filter_map(|(_, record)| match record {
            crate::bridge::Record::Refused((origin, code, _)) => Some((*origin, *code)),
            _ => None,
        })
        .collect();
    assert_eq!(
        refused,
        [(crate::api::ErrorOrigin::Order { id: 5, op: crate::api::OrderOp::Exercise }, 200)]
    );
    assert!(on_the_wire(&mut peer).contains("|35=5|"));
}

#[test]
fn a_cancel_leaves_other_kinds_under_the_same_number_waiting() {
    for cancel in [
        ControlCommand::UnsubscribeTbt { req_id: 17 },
        ControlCommand::CancelHistorical { req_id: 17 },
        ControlCommand::CancelHeadTimestamp { req_id: 17 },
        ControlCommand::CancelHistogramData { req_id: 17 },
        ControlCommand::CancelRealTimeBar { req_id: 17 },
        ControlCommand::UnsubscribeDepth { req_id: 17 },
    ] {
        let (mut hl, shared, tx, _peer) = with_trading();
        shared.admit(&tx, subscription_to_name(false)).unwrap();
        hl.poll_control_commands();
        assert_eq!(hl.ccp.pending_named.len(), 1);
        shared.admit(&tx, cancel).unwrap();
        hl.poll_control_commands();
        assert_eq!(hl.ccp.pending_named.len(), 1);
        assert_eq!(shared.backlog(), 1);
    }
}

#[test]
fn a_ready_question_is_answered_before_its_cancel_in_the_same_lap() {
    let (mut hl, shared, client) = stopped();
    shared.portfolio.account_download_is_settled();
    client.req_positions();
    client.cancel_positions();
    hl.poll_once();
    assert_eq!(heard(&client), ["position_end", "question_retired:Positions"]);
}

#[test]
fn multi_account_answers_keep_the_model_label_the_caller_stated() {
    let (mut hl, shared, client) = stopped();
    shared.portfolio_for("DU2").account_download_is_settled();
    client.req_positions_multi(1, "DU2", "M1");
    client.req_account_updates_multi(2, "DU2", "M2", false);
    hl.poll_control_commands();
    let records =
        shared.take_records(shared.next_seq(), crate::bridge::Take::Dispatch { bulletins: false });
    let labels: Vec<_> = records
        .into_iter()
        .filter_map(|(_, r)| match r {
            crate::bridge::Record::Answer(crate::bridge::Answer::PositionsMulti {
                model_code,
                ..
            })
            | crate::bridge::Record::Answer(crate::bridge::Answer::AccountUpdatesMulti {
                model_code,
                ..
            }) => Some(model_code),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["M1", "M2"]);
}

#[test]
fn a_halt_closes_admission_and_refuses_every_queued_order() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    for id in 1..=200 {
        shared.admit(&tx, placement(id, described(), 0, true)).unwrap();
    }
    hl.poll_control_commands();
    hl.halt_recovery(retry::DisconnectReason::TakenOver);
    assert!(shared.admit(&tx, placement(201, spy(), 0, true)).is_err());
    hl.finish_admitted_commands();
    shared.push_closed();
    let refused = shared.orders.drain_order_inactive();
    assert_eq!(refused.len(), 200);
    assert_eq!(shared.backlog(), 0);
    assert!(shared.closed_pushed());
}

#[test]
fn a_global_cancel_is_one_command_while_the_connection_is_down() {
    let (mut hl, shared, tx, _peer) = with_trading();
    for _ in 0..200 {
        shared.admit(&tx, ControlCommand::Ping).unwrap();
    }
    for _ in 0..4 {
        hl.poll_control_commands();
    }
    assert_eq!(shared.backlog(), 0);
    shared.orders.set_replay_done();
    shared.market.set_instrument_count(200);
    hl.ccp.disconnected = true;
    hl.ccp_conn = None;
    shared.admit(&tx, ControlCommand::GlobalCancel { stated: Default::default() }).unwrap();
    hl.poll_control_commands();
    assert_eq!(shared.backlog(), 1);
}

#[test]
fn orders_waiting_on_the_same_contract_share_its_lookup_and_refusal() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.set_replay_done();
    for id in 1..=64 {
        shared.admit(&tx, placement(id, described(), 0, true)).unwrap();
    }
    hl.poll_control_commands();
    assert_eq!(hl.ccp.order_naming.len(), 1);
    let (lookup, _) = hl.ccp.order_naming.remove(0);
    hl.ccp.orders_named.push((lookup, ccp::OrderNamed::Refused(200, "no definition".into())));
    hl.poll_control_commands();
    assert_eq!(shared.drain_refused().len(), 64);
    assert_eq!(shared.backlog(), 0);
    shared.admit(&tx, placement(65, described(), 0, true)).unwrap();
    hl.poll_control_commands();
    assert_eq!(hl.ccp.order_naming.len(), 1, "a later request asks again");
}

#[test]
fn an_exercise_without_a_number_is_allocated_after_replay_in_the_loop() {
    let (mut hl, shared, client) = stopped();
    shared.orders.replay_is_pending();
    let began = std::time::Instant::now();
    client.exercise_options(0, &spy(), 1, 1, "", true, Default::default());
    assert!(began.elapsed() < Duration::from_millis(100));
    hl.poll_control_commands();
    assert_eq!(client.backlog(), 1);
    shared.orders.set_replay_done();
    hl.poll_control_commands();
    assert_eq!(client.backlog(), 0);
}

#[test]
fn a_joiner_receives_its_acknowledgement_before_the_close() {
    let (mut hl, shared, client) = stopped();
    let subscribe = |req_id| {
        let mut cmd = subscription_to_name(false);
        if let ControlCommand::Subscribe { req_id: id, contract, .. } = &mut cmd {
            *id = req_id;
            *contract = stock();
        }
        cmd
    };
    hl.take_subscription(subscribe(17));
    let slot = hl.md_requests[&17].slot;
    shared.market.push_tick_req_params(
        slot,
        crate::bridge::TickReqParams {
            min_tick: 0.01,
            bbo_exchange: "a6".into(),
            snapshot_permissions: 3,
        },
    );
    shared.market.push_subscription_failure(slot, "no entitlement".into());
    let _ = heard(&client);
    hl.take_subscription(subscribe(18));
    shared.push_closed();
    #[derive(Default)]
    struct Heard(Vec<String>);
    impl crate::api::wrapper::Wrapper for Heard {
        fn tick_req_params(&mut self, id: i64, _: f64, _: &str, _: i64) {
            self.0.push(format!("ack:{id}"));
        }
        fn error(&mut self, id: i64, _error_time: i64, _: i64, _: &str, _: &str) {
            self.0.push(format!("error:{id}"));
        }
        fn connection_closed(&mut self) {
            self.0.push("closed".into());
        }
    }
    let cut = shared.next_seq();
    let mut result = Heard::default();
    client.process_msgs(&mut result);
    assert_eq!(result.0, ["ack:18", "error:18", "closed"]);
    assert_eq!(shared.next_seq(), cut, "a read pushes no records");
}

#[test]
fn an_automatically_numbered_exercise_holds_later_commands_on_its_number() {
    let (mut hl, shared, tx, _peer) = with_trading();
    shared.orders.replay_is_pending();
    let mut contract = described();
    contract.sec_type = "OPT".into();
    shared
        .admit(
            &tx,
            ControlCommand::Exercise(Box::new(crate::types::Exercise {
                allocator: Some(Arc::new(std::sync::atomic::AtomicU64::new(7))),
                req_id: 0,
                order_id: 0,
                stated: false,
                contract,
                action: 1,
                qty: 1,
                account: "DU1".into(),
                states: Default::default(),
                override_: false,
            })),
        )
        .unwrap();
    hl.poll_control_commands();
    shared.orders.set_replay_done();
    hl.poll_control_commands();
    shared
        .admit(&tx, ControlCommand::CancelOrder { order_id: 7, stated: Default::default() })
        .unwrap();
    hl.poll_control_commands();
    assert_eq!(shared.backlog(), 2, "both commands wait until the exercise's contract is named");
    assert!(shared.drain_refused().is_empty());
    assert!(shared.orders.drain_order_inactive().is_empty());
}

#[test]
fn a_joiner_is_told_the_feed_the_venue_accepted() {
    let shared = Arc::new(SharedState::new());
    let mut hl = HotLoop::new(shared.clone(), None, None);
    let contract = ContractRef {
        con_id: 12087792, symbol: "EUR".into(), sec_type: "CASH".into(),
        exchange: "IDEALPRO".into(), currency: "USD".into(), ..Default::default()
    };
    let slot = hl.context.market.register_described(12087792, "EUR", "CASH", "IDEALPRO", "", "");
    let mut first = crate::engine::hot_loop::tests::subscription(1, contract.clone());
    if let ControlCommand::Subscribe { delayed_mode, .. } = &mut first {
        *delayed_mode = Some(3);
    }
    hl.take_subscription(first);
    let feed = shared.market.subscription_data_type(slot, 1);
    assert_eq!(feed.load(std::sync::atomic::Ordering::Relaxed), 1);
    feed.store(4, std::sync::atomic::Ordering::Relaxed);
    shared.market.push_market_data_type(slot, 4);
    hl.take_subscription(crate::engine::hot_loop::tests::subscription(2, contract));
    assert_eq!(feed.load(std::sync::atomic::Ordering::Relaxed), 4, "joining preserves the accepted feed");
    let taken: Vec<_> = shared.take_records(shared.next_seq(), crate::bridge::Take::Dispatch { bulletins: false })
        .into_iter().filter_map(|(_, record)| match record {
            crate::bridge::Record::MarketDataTaken(taken) => Some((taken.req_id, taken.data_type)),
            _ => None,
        }).collect();
    assert_eq!(taken, [(1, 1), (2, 4)]);
}
