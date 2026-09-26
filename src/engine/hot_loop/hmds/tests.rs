//! The tests for this module.
//!
//! One file per module, as `api/client` already does it. Each block below
//! reaches the code it tests through `super::super`, which is the module this
//! file belongs to.

mod historical_contract_tests {
    use super::super::{hist_exchange, hist_sec_type};

    /// The substitution the engine applies to whatever the client sent.
    /// Exercised here rather than on the query builder alone, which honours
    /// these fields either way and so cannot show where they are applied.
    #[test]
    fn a_stated_security_type_reaches_the_wire_in_its_own_spelling() {
        assert_eq!(hist_sec_type("FUT"), "FUT");
        assert_eq!(hist_sec_type("OPT"), "OPT");
        assert_eq!(hist_sec_type("CASH"), "CASH");
        // Both vocabularies for a stock land on the wire spelling.
        assert_eq!(hist_sec_type("STK"), "CS");
        assert_eq!(hist_sec_type("CS"), "CS");
        // Absent keeps exactly what every caller got before.
        assert_eq!(hist_sec_type(""), "CS");
        // A valid type the enum does not carry is sent as stated, not
        // narrowed away — the subscribe path does the same.
        assert_eq!(hist_sec_type("FOP"), "FOP");
        assert_eq!(hist_sec_type("CFD"), "CFD");
        // A value shaped like a type but unknown to both the enum and the
        // gateway still reaches it, to be named there rather than silently
        // described as a stock.
        assert_eq!(hist_sec_type("NOPE"), "NOPE");
        // Anything that could break the query document is blanked instead of
        // embedded. The value lands in XML, so this is the difference between
        // a refused query and a malformed one.
        assert_eq!(hist_sec_type("FOP&"), "");
        assert_eq!(hist_sec_type("<x>"), "");
        assert_eq!(hist_sec_type("A B"), "");
        assert_eq!(hist_sec_type("VERYLONGTYPE"), "");
    }

    #[test]
    fn a_stated_venue_reaches_the_wire_and_an_absent_one_defaults() {
        assert_eq!(hist_exchange("CME"), "CME");
        assert_eq!(hist_exchange("IDEALPRO"), "IDEALPRO");
        assert_eq!(hist_exchange(""), "SMART");
    }
}
/// A reconnect resubscribes the tick-by-tick streams. Replacing the socket
/// alone leaves every stream behind on the dead one while the transport
/// reports healthy, so nothing anywhere states that the data has stopped.
#[test]
fn a_reconnect_puts_the_tick_by_tick_streams_back() {
    let mut hmds = HmdsState::new();
    let mut market = crate::engine::market_state::MarketState::new();
    let instrument = market.register(756733);
    hmds.tbt_subscriptions.push(TbtSubscription { ignore_size: false, instrument, query_id: "tbt_0".to_string(), kind: TbtType::Last, caller_req_id: 0, venue_id: 0, min_tick: 0, size_tick: 0.0, running: Default::default() });
    // One with no contract behind it: it must be reported, not resubscribed
    // against a contract id the engine does not have.
    hmds.tbt_subscriptions.push(TbtSubscription { ignore_size: false, instrument: 7, query_id: "tbt_1".to_string(), kind: TbtType::BidAsk, caller_req_id: 0, venue_id: 0, min_tick: 0, size_tick: 0.0, running: Default::default() });

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let sock = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    let mut conn = None;
    let mut hb = HeartbeatState::new();
    hmds.disconnected = true;
    hmds.reconnect(
        crate::protocol::connection::Connection::new_raw(sock).unwrap(),
        &mut conn, &market, &mut hb,
    );

    assert!(!hmds.disconnected, "the transport is live again");
    assert_eq!(hmds.tbt_subscriptions.len(), 1, "the resolvable stream is back");
    assert_eq!(
        hmds.tbt_subscriptions[0].running, Default::default(),
        "the dead session's prices go with it, or its last pair takes the next move",
    );
    assert_eq!(hmds.tbt_subscriptions[0].instrument, instrument);
    assert_ne!(hmds.tbt_subscriptions[0].query_id, "tbt_0", "under a new id, not the dead session's");
}

/// The routing for a five-second bar stream is a ticker id the session
/// issued. A reconnect that kept the routing and re-sent nothing left the
/// bars stopped with the connection reporting healthy.
///
/// A keep-up-to-date request needs only this stream restored: its bars are
/// folded from it, and the partial bar survives the reconnect. A second
/// request for the same stream leaves two subscriptions upstream for one
/// caller.
#[test]
fn a_reconnect_asks_for_the_five_second_bars_again() {
    let mut hmds = HmdsState::new();
    let market = crate::engine::market_state::MarketState::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let sock = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    let mut conn = Some(crate::protocol::connection::Connection::new_raw(sock).unwrap());
    let mut hb = HeartbeatState::new();
    hmds.send_realtime_bar_subscribe(9, 265598, "", "STK", "SMART", "TRADES", true, &mut conn, &mut hb);
    let first = hmds.rtbar_subs[0].0.clone();
    // State a keep-up-to-date request leaves behind: the stream and the
    // partial bar.
    hmds.keep_up_to_date_reqs.insert(9);
    hmds.forming_bars.push(super::FormingBar {
        req_id: 9,
        seconds: 60,
        opened_at: 0,
        daily_session: None, closed_at: None,
        bar: Default::default(),
        weighted: 0.0,
        queued: Vec::new(),
    });

    let sock2 = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer2, _) = listener.accept().unwrap();
    hmds.disconnected = true;
    hmds.reconnect(
        crate::protocol::connection::Connection::new_raw(sock2).unwrap(),
        &mut conn, &market, &mut hb,
    );

    assert_eq!(hmds.rtbar_subs.len(), 1, "the stream is asked for again");
    assert_ne!(hmds.rtbar_subs[0].0, first, "under a new query, not the dead session's");
    assert_eq!(hmds.rtbar_subs[0].1, 9, "still answering the caller's request id");
    assert_eq!(
        hmds.forming_bars.iter().filter(|f| f.req_id == 9).count(), 1,
        "and the bar it was folding is still the one being folded",
    );
    assert!(
        hmds.pending_historical.iter().all(|(_, rid)| *rid != 9),
        "nothing else is asked for on its behalf: one request, one stream",
    );
}

use super::*;

/// The diagnostic log byte-sliced a lossily decoded value, so a tag whose
/// two-hundredth byte fell inside a multi-byte character aborted the hot
/// loop — and only when debug logging was on, which is to say only while
/// someone was diagnosing an incident.
#[test]
fn a_non_utf8_xml_tag_does_not_abort_the_hot_loop() {
    // Driven through the handler that performs the slice, so the assertion
    // depends on the code under test.
    //
    // 199 ASCII bytes then one invalid byte. Lossily decoded, that byte becomes
    // a three-byte replacement character, so byte 200 falls inside it.
    let mut payload = b"<ResultSetBar>".to_vec();
    payload.extend(std::iter::repeat_n(b'a', 199 - payload.len()));
    payload.push(0xFF);

    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(&payload);
    msg.push(0x01);

    let mut hmds = HmdsState::new();
    let shared = crate::bridge::SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<crate::protocol::connection::Connection> = None;
    // The slice runs only with debug logging enabled.
    log::set_max_level(log::LevelFilter::Debug);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);
}

fn make_query_error_msg(query_id: &str, error: &str) -> Vec<u8> {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<QueryError>\n\t<id>{query_id}</id>\n\t<error>{error}</error>\n</QueryError>\n",
    );
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    msg
}

fn make_bar_msg(query_id: &str, eoq: bool) -> Vec<u8> {
    let xml = format!(
        "<ResultSetBar><id>{}</id><eoq>{}</eoq><tz>UTC</tz><Events>\
         <Bar><time>20260714-13:30:00</time><open>100.0</open><close>100.5</close>\
         <high>100.7</high><low>99.9</low><weightedAvg>100.2</weightedAvg>\
         <volume>1000</volume><count>10</count></Bar></Events></ResultSetBar>",
        query_id, if eoq { "true" } else { "false" },
    );
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    msg
}

#[test]
fn segmented_bar_reply_completes_on_eoq_true() {
    // /: a segmented bar reply carries <eoq>false> on
    // early frames and <eoq>true> on the final one. The pending entry must
    // persist through the false frames and be released on the true frame.
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_historical.push(("q7".to_string(), 21));
    hmds.held.push(HeldSeries {
        req_id: 21, fold: Fold::None, bars: Vec::new(), timezone: String::new(), actions_query: None, actions: None, complete: false,
        along: Default::default(),
    });

    hmds.process_hmds_message(&make_bar_msg("q7", false), &mut conn, &shared, &None, &mut hb);
    assert_eq!(hmds.pending_historical.len(), 1, "entry must persist through eoq=false");
    assert!(shared.reference.drain_historical_data().is_empty(), "nothing is filed before the last page");

    hmds.process_hmds_message(&make_bar_msg("q7", true), &mut conn, &shared, &None, &mut hb);
    assert!(hmds.pending_historical.is_empty(), "eoq=true must release the pending entry");

    // Both pages, filed once, whole.
    let hist = shared.reference.drain_historical_data();
    assert_eq!(hist.len(), 1, "the series is filed once its last page is in");
    assert!(hist[0].1.is_complete, "and it is the whole answer");
    assert_eq!(hist[0].1.bars.len(), 2, "with every page's bars in it");
}

#[test]
fn conadj_response_frame_is_skipped_without_disturbing_pending() {
    // /: the 6040=10022 ConAdjResponse (corporate
    // actions) is pushed once per contract on the first historical request.
    // It must be recognized and skipped, not treated as bar or completion.
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_historical.push(("q8".to_string(), 22));

    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=U\x016040=10022\x016118=");
    msg.extend_from_slice(b"<ConAdjResponse><id>ContractAdjustment1</id></ConAdjResponse>");
    msg.push(0x01);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

    assert_eq!(hmds.pending_historical.len(), 1, "ConAdjResponse must not touch pending historical");
    assert!(shared.reference.drain_historical_data().is_empty());
    assert!(shared.reference.drain_historical_errors().is_empty());
}

/// A bar request the venue refuses is told the error alone, as a gateway
/// tells it, and its caller's side is told the request is over.
#[test]
fn a_refused_bar_request_is_told_the_error_alone() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_historical.push(("hist_1003".to_string(), 11));
    hmds.keep_up_to_date_reqs.insert(11);
    // A page already held for it: a refused query takes what it held with it,
    // or the caller is answered with the error and then left waiting on a
    // series that will never complete.
    hmds.held.push(HeldSeries {
        req_id: 11, fold: Fold::None, bars: Vec::new(), timezone: String::new(), actions_query: None, actions: None, complete: false,
        along: Default::default(),
    });
    hmds.process_hmds_message(&make_bar_msg("hist_1003", false), &mut conn, &shared, &None, &mut hb);

    let msg = make_query_error_msg("hist_1003", "Invalid time length");
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

    assert!(hmds.pending_historical.is_empty(), "pending entry should be drained");
    assert!(hmds.held.is_empty(), "the held pages go with the refused query");
    assert!(!hmds.keep_up_to_date_reqs.contains(&11), "kut flag should be cleared");

    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, 11);
    assert_eq!(errors[0].1, 162);
    assert_eq!(errors[0].2, "Invalid time length");

    assert!(shared.reference.drain_historical_data().is_empty(), "and no end follows it");
    assert_eq!(over(&shared), [11], "the request is over");
}

/// The bar requests the engine has said are over, as the caller's side reads
/// them.
pub(in crate::engine::hot_loop) fn over(shared: &SharedState) -> Vec<u32> {
    shared.take_records(shared.next_seq(), crate::bridge::Take::Dispatch { bulletins: false })
        .into_iter()
        .filter_map(|(_, record)| match record {
            crate::bridge::Record::HistoricalOver(id) => Some(id),
            _ => None,
        })
        .collect()
}

/// A head timestamp the connection cannot carry is refused, not recorded as
/// pending. Pushed unconditionally, a request with no connection sat pending
/// with no answer ever coming.
#[test]
fn a_head_timestamp_with_no_connection_is_refused_not_left_pending() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    shared.reference.cache_definition(265598, crate::types::model::Contract {
        con_id: 265598, symbol: "AAPL".into(), sec_type: "STK".into(), exchange: "SMART".into(),
        ..Default::default()
    });

    let aapl = crate::types::ContractRef { con_id: 265598, ..Default::default() };
    hmds.send_head_timestamp_request(3, &aapl, "TRADES", true, false, 1, &mut conn, &mut hb, &shared);

    assert!(hmds.pending_head_ts.is_empty(), "a request that could not be sent is not left pending");
    let told = shared.reference.drain_historical_errors();
    assert!(
        told.iter().any(|(id, code, _)| *id == 3 && *code == 504),
        "the caller is told the request could not be sent: {told:?}",
    );
}


/// A head timestamp and a fundamental report are asked about the contract the
/// request states, with no definition of it held: a program that kept its
/// contracts was refused both until it happened to look the contract up. Each
/// states it as a gateway does. A head timestamp states the type and the
/// exchange in their own forms however they were spelled; a fundamental report
/// states a stock in dollars, whatever the contract, as a gateway states every
/// one it asks for.
#[test]
fn a_head_timestamp_states_the_contract_it_was_asked_about() {
    use std::io::Read;
    let aapl = crate::types::ContractRef {
        con_id: 265598, sec_type: "cs".into(), exchange: "ISLAND".into(), currency: "EUR".into(),
        ..Default::default()
    };
    type Ask = fn(&mut HmdsState, &crate::types::ContractRef, &mut Option<Connection>, &mut HeartbeatState, &SharedState);
    let rows: [(Ask, &[&str]); 2] = [
        (
            |hmds, contract, conn, hb, shared| {
                hmds.send_head_timestamp_request(3, contract, "TRADES", true, false, 1, conn, hb, shared)
            },
            &["<contractID>265598</contractID><exchange>NASDAQ</exchange><secType>STK</secType>"],
        ),
        (
            |hmds, contract, conn, hb, shared| {
                hmds.send_fundamental_data_request(3, contract.con_id as u32, "ReportSnapshot", shared, conn, hb)
            },
            &[
                "<contractID>265598</contractID><exchange>RTRSFND</exchange><secType>STK</secType>",
                "<currency>USD</currency>",
            ],
        ),
    ];
    for (ask, stated) in rows {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let (conn, mut peer) = Connection::for_test();
        ask(&mut hmds, &aapl, &mut Some(conn), &mut HeartbeatState::new(), &shared);

        assert!(shared.reference.drain_historical_errors().is_empty(), "nothing is refused");
        let mut sent = [0u8; 4096];
        let n = peer.read(&mut sent).unwrap();
        let sent = String::from_utf8_lossy(&sent[..n]);
        for stated in stated {
            assert!(sent.contains(stated), "{stated}: {sent}");
        }
    }
}

#[test]
fn query_error_releases_head_timestamp_without_sentinel() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_head_ts.push(("hts_1004".to_string(), 42, 1));

    let msg = make_query_error_msg("hts_1004", "No head timestamp");
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

    assert!(hmds.pending_head_ts.is_empty());
    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors, vec![(42, 162, "No head timestamp".to_string())]);
    // Head-ts is not a bar request — no historical_data sentinel should fire.
    assert!(shared.reference.drain_historical_data().is_empty());
}

/// The request carries no number of its own, but it is still told when it
/// cannot be made: returned as though the question had gone out, the caller
/// waited on an answer nothing was ever going to send.
#[test]
fn scanner_parameters_on_a_dead_connection_is_reported_not_dropped() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;

    hmds.send_scanner_params_request(&mut conn, &mut hb, &shared);

    assert!(!hmds.pending_scanner_params, "nothing was sent, so nothing is awaited");
    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 1, "the caller is told it was not sent");
    assert_eq!(errors[0].0, crate::bridge::ReferenceState::NO_REQUEST);
    assert_eq!(
        errors[0].1, 504,
        "the request never left, so the service reported no difficulty with it",
    );
}

// ── unknown bar_size rejects at the engine too (backstop for
// raw control-channel callers; the client validates synchronously) ──

/// A bar size or a series this client cannot name is refused at the engine
/// too, under the number for a malformed request rather than the data
/// service's own, and no end follows the refusal.
#[test]
fn engine_rejects_an_unknown_bar_size_or_series_with_an_error_alone() {
    for (bar_size, what_to_show, named) in [("1 minute", "TRADES", "bar_size"), ("1 hour", "GRAVITY", "")] {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;

        hmds.send_historical_request_ex(9, 756733, "", "2 D", bar_size, what_to_show,
            true, false, false, "SPY", "STK", "SMART", &mut conn, &mut hb, &shared);

        assert!(hmds.pending_historical.is_empty(), "{bar_size} {what_to_show}: a refused request does not go pending");
        let errors = shared.reference.drain_historical_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].1, 321,
            "the request is malformed, not a difficulty the service had with one it answered",
        );
        assert!(errors[0].2.contains(named), "got: {}", errors[0].2);
        assert!(shared.reference.drain_historical_data().is_empty(), "{bar_size} {what_to_show}: and no end follows it");
    }
}

// ── a query waits for the venue ──

/// There used to be a sweep here that failed a historical query after a
/// minute of quiet, under the venue's own number. The reference client sets
/// no deadline of its own on one — the budget it carries is the widest a long
/// can hold — so a query now waits, and is failed only where the venue or the
/// connection says something.
#[test]
fn a_historical_query_waits_rather_than_being_failed_on_a_clock() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    hmds.pending_historical.push(("hist_1010".to_string(), 21));

    assert_eq!(hmds.pending_historical_count(), 1, "still waiting on the venue");
    assert!(shared.reference.drain_historical_errors().is_empty(), "and nothing invented");
    assert!(shared.reference.drain_historical_data().is_empty());
}


#[test]
fn query_error_for_unknown_query_id_drops_nothing_and_emits_no_error() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_historical.push(("hist_1003".to_string(), 11));

    let msg = make_query_error_msg("hist_9999", "Boom");
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

    assert_eq!(hmds.pending_historical.len(), 1, "unrelated entry must stay");
    assert!(shared.reference.drain_historical_errors().is_empty());
    assert!(shared.reference.drain_historical_data().is_empty());
}
/// The helpers above are only worth anything if the request that reaches
/// the wire uses them. Reinstating the old `CS`/`SMART` constants in the
/// builder passes every test that checks the helpers or the query encoder
/// in isolation, so this drives the real function and reads the socket.
#[test]
fn the_query_on_the_wire_carries_the_contract_s_own_type_and_venue() {
    use crate::protocol::connection::Connection;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = std::net::TcpStream::connect(addr).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(Connection::new_raw(client).unwrap());

    let mut hmds = super::HmdsState::new();
    let mut hb = crate::engine::hot_loop::HeartbeatState::new();
    let shared = crate::bridge::SharedState::new();

    hmds.send_historical_request_ex(
        1, 495512563, "20260101 16:00:00", "1 D", "1 hour", "TRADES",
            true, false, false, "ES", "FUT", "CME", &mut conn, &mut hb, &shared,
    );

    let sent = String::from_utf8_lossy(&read_frame(&mut peer)).to_string();

    assert!(sent.contains("FUT"), "the contract's security type: {sent}");
    assert!(sent.contains("CME"), "the contract's venue: {sent}");
    assert!(!sent.contains("SMART"), "and not the old constant: {sent}");
}

/// One complete daily bar under a query id, stated at a day and a close.
fn adj_bar_msg(query_id: &str, day: &str, close: f64, eoq: bool) -> Vec<u8> {
    let xml = format!(
        "<ResultSetBar><id>{query_id}</id><eoq>{eoq}</eoq><tz>UTC</tz><Events>\
         <Bar><date>{day}</date><open>{close}</open><close>{close}</close>\
         <high>{close}</high><low>{close}</low><weightedAvg>{close}</weightedAvg>\
         <volume>100</volume><count>5</count></Bar></Events></ResultSetBar>",
    );
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    msg
}

/// A corporate-actions reply echoing a query id and stating one action, in the
/// shape a live session was answered with: the query as XML on tag 6118, the
/// rows as text on tag 96, a name on its own line and its record under it.
fn conadj_msg(query_id: &str, con_id: u32, action_rows: &str) -> Vec<u8> {
    conadj_reply(query_id, &format!("conc\n{con_id},-1,-1\n{action_rows}\n"))
}

/// The same reply carrying the body given, whole.
fn conadj_reply(query_id: &str, body: &str) -> Vec<u8> {
    let echoed = format!(
        "<ListOfQueries><ConAdjQuery><id>{query_id}</id></ConAdjQuery></ListOfQueries>",
    );
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=U\x016040=10022\x016118=");
    msg.extend_from_slice(echoed.as_bytes());
    msg.extend_from_slice(b"\x0196=");
    msg.extend_from_slice(body.as_bytes());
    msg.push(0x01);
    msg
}

/// A month of a stock's daily bars asked for through the engine under 42, the
/// actions it asks for first answered with `body`: the name the bars are then
/// asked under.
fn folded_and_answered(
    hmds: &mut HmdsState, conn: &mut Option<Connection>, peer: &mut std::net::TcpStream,
    shared: &SharedState, hb: &mut HeartbeatState, what_to_show: &str, body: &str,
) -> String {
    hmds.send_historical_request_ex(
        42, 756733, "", "1 M", "1 day", what_to_show, true, false, false,
        "NVDA", "STK", "SMART", conn, hb, shared,
    );
    let sent = String::from_utf8_lossy(&read_frame(peer)).to_string();
    assert!(sent.contains("10020"), "the actions are asked for before the bars: {sent}");
    let qid = hmds.pending_adjustments.iter().find(|(_, rid, _)| *rid == 42)
        .map(|(q, _, _)| q.clone())
        .expect("the actions query is outstanding under this request");
    hmds.process_hmds_message(&conadj_reply(&qid, body), conn, shared, &None, hb);
    let _ = read_frame(peer);
    hmds.pending_historical.iter().find(|(_, rid)| *rid == 42)
        .map(|(q, _)| q.clone())
        .expect("and the bars once they are in")
}

/// One complete page of bars, a bar at each stamp given at the close given —
/// a stamp `from/to` states the day a bar ends on as well — the session opens
/// given by their day, and the step the answer states where it states one.
fn page_of_bars(query_id: &str, bars: &[(&str, f64)], opens: &[&str], step: Option<&str>) -> Vec<u8> {
    let opens: String = opens.iter()
        .map(|day| format!("<Open><time>{day}-13:30:00</time><refDate>{day}</refDate></Open>"))
        .collect();
    let bars: String = bars.iter().map(|(at, close)| {
        // A bar of a day is dated, and a shorter one timed.
        let (at, end) = at.split_once('/').map_or((*at, String::new()), |(at, to)| (at, format!("<endDate>{to}</endDate>")));
        let stamp = if at.contains('-') { "time" } else { "date" };
        format!(
            "<Bar><{stamp}>{at}</{stamp}>{end}<open>{close}</open><close>{close}</close>\
             <high>{close}</high><low>{close}</low><weightedAvg>{close}</weightedAvg>\
             <volume>100</volume><count>5</count></Bar>",
        )
    }).collect();
    let step = step.map_or(String::new(), |s| format!("<approxStep>{s}</approxStep>"));
    let xml = format!(
        "<ResultSetBar><id>{query_id}</id><eoq>true</eoq><tz>US/Eastern</tz>{step}<Events>{opens}{bars}\
         </Events></ResultSetBar>",
    );
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    msg
}

/// A contract whose id changed is asked along its id history, as a gateway
/// asks it: the actions first, over the range a gateway asks them over for a
/// request made through the API; then one query per stretch of the history,
/// the newest first and from the day its id began, the next once the last is
/// in — under the id it traded as, naming the caller's id where that is
/// another, as expired where it is an older id that is not the caller's,
/// bounded to its own days, naming the ticker and the listing it traded under
/// where a gateway names them, and stating the step the first answer stated.
/// The answers are one series, and a split dated after the change moves the
/// bars of both ids.
///
/// Every row is bounded by hand from a gateway's rules:
///
/// - Thirty days back from the fourteenth of June 2024 is counted as
///   forty-three calendar days, to the second of May. The old id's last stated
///   day, the thirty-first, is carried to the day before the new one began.
///   Three days are in once the newest answers, so twenty-seven are still
///   wanted, asked as thirty-eight calendar days; its listing is one a gateway
///   leaves out.
/// - Five days of five-minute bars asked under the old id, back from the
///   fourth of June, reach back seven calendar days, to the twenty-eighth of
///   May at the hour asked; the old id's stretch is asked from there to the
///   end of its last day, with no length, under its own ticker and listing.
/// - Three days asked along two ids, all three of them answered by the newest:
///   the older is not asked.
/// - Two ids both stated open at the end are ordered with the older first, as
///   a gateway's sort orders them, and the whole request is asked under it:
///   an older stretch left open is not asked.
/// - A request that ends before the only id it could be asked under began is
///   answered that nothing is there, and nothing is asked.
/// - A stretch answered with no bar ends the series all the same.
/// - Eight weeks along two ids and a split on the Wednesday of the second
///   week: the new id's stretch is split at the split, the newer part asked
///   from its day and the older to it, and the old id's to the day the new one
///   began. One week is in once the newest answers, so seven are still wanted,
///   then five; the last is cut at eight weeks back, the nineteenth of April.
///   Every week that ends before the split, or that starts in its week and
///   ends on its day, is halved; and the two parts of the split's week are
///   joined into one bar, which opens on the Monday at the older part's open
///   and closes at the newer part's close and end, with the higher high, the
///   lower low, the volume and the count of both, and the average weighted by
///   volume: (55 x 200 + 60 x 100) / 300.
/// - Four weeks and a split on a Friday: a week the venue dates by its day and
///   the Saturday after it ends on that Friday on the exchange's clock, so the
///   split's own day, the first on the new scale, is put on the old one with
///   the rest of its week, as a gateway puts it, before the two parts are
///   joined.
/// - Three months and a split on the fifteenth of May: a month is counted as
///   thirty-one days, so two are in once the newest answers and one is still
///   wanted, asked as one month and cut at ninety-three days back, the
///   twenty-seventh of March; the two parts of May are joined on its first.
/// - Four weeks and a split whose day cannot be read: nothing is split at it,
///   and the fold refuses it, as it refuses it on a series of days.
/// - Thirty days along two ids where the newest answer states a session open
///   with no bar on the fourth of June, and one on the seventh beside a bar:
///   four days are in, not three, so twenty-six are still wanted, asked as
///   thirty-seven calendar days. An open the answer states before the day the
///   stretch began is not counted.
/// - A request whose end this client cannot read is asked as it was made, and
///   what comes back is filed as the venue served it.
#[test]
fn a_series_is_asked_along_the_ids_the_contract_traded_under() {
    struct Asked {
        states: &'static [&'static str],
        omits: &'static [&'static str],
        answered: &'static [(&'static str, f64)],
        opens: &'static [&'static str],
        step: Option<&'static str>,
    }
    let split_after_the_change = "conc\n222,20240603,-1\n111,-1,20240531\nSS\n20240610,2\n";
    type Filed = Result<&'static [(&'static str, f64, i64)], &'static str>;
    // What the row is, the caller's id, its end, length and bar, the actions'
    // answer, each stretch as asked, and what is filed or why it is not.
    type Row = (&'static str, u32, &'static str, &'static str, &'static str, &'static str, &'static [Asked], Filed);
    let rows: [Row; 12] = [
        ("days along two ids", 222, "20240614-20:00:00", "30 D", "1 day",
         "conc\n222,20240603,-1\n111,-1,20240531\nconexch\n111,VALUE,20240531\nSS\n20240610,2\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<expired>no</expired>",
                          "<endTime>20240614-20:00:00</endTime>", "<cutoffDate>20240603</cutoffDate>",
                          "<timeLength>30 d</timeLength>"],
                omits: &["startTime", "liveContractID", "approxStep"],
                answered: &[("20240603", 104.0), ("20240607", 106.0), ("20240613", 55.0)],
                opens: &[],
                step: None,
            },
            Asked {
                states: &["<contractID>111</contractID>", "<liveContractID>222</liveContractID>",
                          "<expired>yes</expired>", "<endTime>20240603-00:00:00</endTime>",
                          "<timeLength>38 d</timeLength>", "<cutoffDate>20240502</cutoffDate>",
                          "<approxStep>1d</approxStep>"],
                omits: &["startTime", "histListExch"],
                answered: &[("20240529", 100.0), ("20240531", 102.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[
            ("20240529", 50.0, 200), ("20240531", 51.0, 200), ("20240603", 52.0, 200),
            ("20240607", 53.0, 200), ("20240613", 55.0, 100),
         ])),
        ("minutes under the old id", 111, "20240604-20:00:00", "5 D", "5 mins",
         "conc\n222,20240603,-1\n111,-1,20240531\nconsym\n111,FB,20240531\n\
          conexch\n111,NASDAQ,20240531\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<liveContractID>111</liveContractID>",
                          "<expired>no</expired>", "<endTime>20240604-20:00:00</endTime>",
                          "<cutoffDate>20240603</cutoffDate>", "<timeLength>5 d</timeLength>"],
                omits: &["startTime", "approxStep", "histUnderlying", "histListExch"],
                answered: &[("20240603-13:30:00", 50.0), ("20240604-19:55:00", 51.0)],
                opens: &[],
                step: Some("300"),
            },
            Asked {
                states: &["<contractID>111</contractID>", "<expired>no</expired>",
                          "<startTime>20240528-20:00:00</startTime>",
                          "<endTime>20240603-00:00:00</endTime>",
                          "<histUnderlying>FB</histUnderlying>", "<histListExch>NASDAQ</histListExch>",
                          "<approxStep>300</approxStep>"],
                omits: &["liveContractID", "timeLength", "cutoffDate"],
                answered: &[("20240531-19:55:00", 49.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[
            ("20240531-19:55:00", 49.0, 100), ("20240603-13:30:00", 50.0, 100),
            ("20240604-19:55:00", 51.0, 100),
         ])),
        ("every day in from the newest", 222, "20240614-20:00:00", "3 D", "1 day",
         "conc\n222,20240612,-1\n111,-1,20240611\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<cutoffDate>20240612</cutoffDate>",
                          "<timeLength>3 d</timeLength>"],
                omits: &["liveContractID"],
                answered: &[("20240612", 10.0), ("20240613", 11.0), ("20240614", 12.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[("20240612", 10.0, 100), ("20240613", 11.0, 100), ("20240614", 12.0, 100)])),
        ("two ids stated open", 222, "20240614-20:00:00", "5 D", "1 day",
         "conc\n222,20200601,-1\n111,-1,-1\n", &[
            Asked {
                states: &["<contractID>111</contractID>", "<liveContractID>222</liveContractID>",
                          "<expired>no</expired>", "<timeLength>5 d</timeLength>"],
                omits: &["cutoffDate", "startTime"],
                answered: &[("20240613", 20.0), ("20240614", 21.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[("20240613", 20.0, 100), ("20240614", 21.0, 100)])),
        ("nothing within the request", 222, "20240531-20:00:00", "5 D", "1 day",
         "conc\n222,20240603,-1\n", &[], Err("HMDS query returned no data: NEWCO@BEST Last")),
        ("no bar at all", 222, "20240614-20:00:00", "5 D", "1 day", "conc\n222,-1,-1\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<timeLength>5 d</timeLength>"],
                omits: &["cutoffDate"],
                answered: &[],
                opens: &[],
                step: None,
            },
         ], Ok(&[])),
        ("weeks along two ids and a split", 222, "20240614-20:00:00", "8 W", "1 week",
         "conc\n222,20240603,-1\n111,-1,20240531\nSS\n20240612,2\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<endTime>20240614-20:00:00</endTime>",
                          "<cutoffDate>20240612</cutoffDate>", "<timeLength>8 W</timeLength>"],
                omits: &["startTime", "liveContractID", "approxStep"],
                answered: &[("20240612/20240615", 60.0)],
                opens: &[],
                step: Some("1W"),
            },
            Asked {
                states: &["<contractID>222</contractID>", "<expired>no</expired>",
                          "<endTime>20240612-00:00:00</endTime>", "<timeLength>7 W</timeLength>",
                          "<cutoffDate>20240603</cutoffDate>", "<approxStep>1W</approxStep>"],
                omits: &["startTime", "liveContractID"],
                answered: &[("20240603/20240608", 100.0), ("20240610/20240612", 110.0)],
                opens: &[],
                step: None,
            },
            Asked {
                states: &["<contractID>111</contractID>", "<liveContractID>222</liveContractID>",
                          "<expired>yes</expired>", "<endTime>20240603-00:00:00</endTime>",
                          "<timeLength>5 W</timeLength>", "<cutoffDate>20240419</cutoffDate>",
                          "<approxStep>1W</approxStep>"],
                omits: &["startTime"],
                answered: &[("20240520/20240525", 90.0), ("20240528/20240601", 95.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[
            ("20240520", 45.0, 200), ("20240528", 47.5, 200), ("20240603", 50.0, 200),
            ("20240610", 60.0, 300),
         ])),
        ("weeks and a split on a Friday", 222, "20240621-20:00:00", "4 W", "1 week",
         "conc\n222,-1,-1\nSS\n20240614,2\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<cutoffDate>20240614</cutoffDate>",
                          "<timeLength>4 W</timeLength>"],
                omits: &["startTime", "approxStep"],
                answered: &[("20240614/20240615", 50.0), ("20240617/20240622", 52.0)],
                opens: &[],
                step: Some("1W"),
            },
            Asked {
                states: &["<contractID>222</contractID>", "<endTime>20240614-00:00:00</endTime>",
                          "<timeLength>2 W</timeLength>", "<cutoffDate>20240524</cutoffDate>"],
                omits: &["startTime", "liveContractID"],
                answered: &[("20240603/20240608", 96.0), ("20240610/20240614", 98.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[("20240603", 48.0, 200), ("20240610", 25.0, 400), ("20240617", 52.0, 100)])),
        ("months and a split", 222, "20240628-20:00:00", "3 M", "1 month",
         "conc\n222,-1,-1\nSS\n20240515,2\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<cutoffDate>20240515</cutoffDate>",
                          "<timeLength>3 m</timeLength>"],
                omits: &["startTime", "approxStep"],
                answered: &[("20240515/20240601", 60.0), ("20240603/20240629", 62.0)],
                opens: &[],
                step: Some("1M"),
            },
            Asked {
                states: &["<endTime>20240515-00:00:00</endTime>", "<timeLength>1 m</timeLength>",
                          "<cutoffDate>20240327</cutoffDate>", "<approxStep>1M</approxStep>"],
                omits: &["startTime"],
                answered: &[("20240401/20240501", 90.0), ("20240501/20240515", 100.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[("20240401", 45.0, 200), ("20240501", 60.0, 300), ("20240603", 62.0, 100)])),
        ("weeks and a split nobody can date", 222, "20240621-20:00:00", "4 W", "1 week",
         "conc\n222,-1,-1\nSS\n2024061,2\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<timeLength>4 W</timeLength>"],
                omits: &["cutoffDate", "startTime"],
                answered: &[("20240610/20240615", 50.0)],
                opens: &[],
                step: Some("1W"),
            },
         ], Err("the SS in this contract's actions is dated \"2024061\", which is not a day a price \
                 can be placed before or after. Adjusting around it would hand back the price the \
                 venue served under the name of an adjusted one")),
        ("days counting a session open", 222, "20240614-20:00:00", "30 D", "1 day",
         "conc\n222,20240603,-1\n111,-1,20240531\n", &[
            Asked {
                states: &["<contractID>222</contractID>", "<cutoffDate>20240603</cutoffDate>"],
                omits: &["approxStep"],
                answered: &[("20240603", 52.0), ("20240607", 53.0), ("20240613", 55.0)],
                opens: &["20240531", "20240604", "20240607"],
                step: None,
            },
            Asked {
                states: &["<contractID>111</contractID>", "<timeLength>37 d</timeLength>",
                          "<cutoffDate>20240502</cutoffDate>"],
                omits: &["startTime"],
                answered: &[("20240531", 51.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[
            ("20240531", 51.0, 100), ("20240603", 52.0, 100), ("20240607", 53.0, 100),
            ("20240613", 55.0, 100),
         ])),
        ("an end this client cannot read", 222, "20240614 16:00:00 Foo/Bar", "30 D", "1 day",
         split_after_the_change, &[
            Asked {
                states: &["<contractID>222</contractID>", "<timeLength>30 d</timeLength>"],
                omits: &["cutoffDate", "liveContractID"],
                answered: &[("20240607", 106.0)],
                opens: &[],
                step: None,
            },
         ], Ok(&[("20240607", 106.0, 100)])),
    ];
    let sent = |peer: &mut std::net::TcpStream| String::from_utf8_lossy(&read_frame(peer)).to_string();
    for (what, caller, end, duration, bar_size, history, asked, filed) in rows {
        let (conn, mut peer) = Connection::for_test();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut conn = Some(conn);
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();

        hmds.send_historical_request_ex(
            42, caller.into(), end, duration, bar_size, "TRADES", true, false, false,
            "NEWCO", "STK", "SMART", &mut conn, &mut hb, &shared,
        );
        let actions = sent(&mut peer);
        let today: String = chrono_free_timestamp().chars().take(8).collect();
        assert!(
            actions.contains("10020") && actions.contains(&format!("<contractID>{caller}</contractID>"))
                && actions.contains("<startDate>19800101</startDate>")
                && actions.contains(&format!("<endDate>{today}</endDate>")),
            "{what}: the actions go first, from 1980 to today: {actions}",
        );
        assert!(hmds.pending_historical.is_empty(), "{what}: and no bar is asked for before they answer");
        let qid = hmds.pending_adjustments[0].0.clone();
        hmds.process_hmds_message(&conadj_reply(&qid, history), &mut conn, &shared, &None, &mut hb);

        let mut before = None;
        for (n, stretch) in asked.iter().enumerate() {
            let query = sent(&mut peer);
            for wanted in stretch.states {
                assert!(query.contains(wanted), "{what}, stretch {n} states {wanted}: {query}");
            }
            for unwanted in stretch.omits {
                assert!(!query.contains(unwanted), "{what}, stretch {n} states no {unwanted}: {query}");
            }
            let answering = hmds.pending_historical.iter().find(|(_, rid)| *rid == 42)
                .map(|(q, _)| q.clone()).expect("the number answers the stretch asked");
            assert_ne!(before.replace(answering.clone()), Some(answering.clone()), "{what}: a query of its own");
            assert!(shared.reference.drain_historical_data().is_empty(), "{what}: not whole before the last");
            hmds.process_hmds_message(
                &page_of_bars(&answering, stretch.answered, stretch.opens, stretch.step), &mut conn, &shared, &None, &mut hb,
            );
        }

        let series = shared.reference.drain_historical_data();
        let errors = shared.reference.drain_historical_errors();
        match filed {
            Ok(wanted) => {
                assert_eq!(series.len(), 1, "{what}: the stretches are filed as one series");
                assert!(series[0].1.is_complete, "{what}: and it says it is the whole answer");
                let bars: Vec<(String, f64, i64)> =
                    series[0].1.bars.iter().map(|b| (b.time.clone(), b.close, b.volume)).collect();
                let wanted: Vec<(String, f64, i64)> =
                    wanted.iter().map(|(at, close, volume)| (at.to_string(), *close, *volume)).collect();
                assert_eq!(bars, wanted, "{what}: oldest first across the stretches");
                assert!(errors.is_empty(), "{what}: {errors:?}");
                if what == "weeks along two ids and a split" {
                    let week = series[0].1.bars.last().unwrap();
                    assert_eq!(
                        (week.open, week.high, week.low, week.count, week.end.as_str()),
                        (55.0, 60.0, 55.0, 10, "20240615"),
                        "{what}: the split's week joined whole: {week:?}",
                    );
                    assert!((week.wap - 170.0 / 3.0).abs() < 1e-9, "{what}: its average {}", week.wap);
                }
            }
            Err(why) => {
                assert!(series.is_empty(), "{what}: no bar and no end: {series:?}");
                assert_eq!(errors.len(), 1, "{what}: {errors:?}");
                assert_eq!((errors[0].0, errors[0].1, errors[0].2.as_str()), (42, 162, why), "{what}");
            }
        }
        assert!(
            hmds.pending_historical.is_empty() && hmds.held.is_empty(),
            "{what}: and nothing more is asked or held",
        );
    }
}

/// A gateway holds a contract's actions for the session: a request for the
/// same contract on the day they are held through is asked along them at once,
/// with no second question, and an answer on a later day adds to what is held
/// the rows it does not state, rather than replacing it. Here the later answer
/// no longer states the first split and states a second, and the series is
/// folded by both — until the day turns on this machine's calendar, which lets
/// go of what is held.
#[test]
fn a_contract_s_actions_are_asked_once_a_day_and_held_for_the_session() {
    let (conn, mut peer) = Connection::for_test();
    let mut conn = Some(conn);
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let held = "conc\n222,20240603,-1\n111,-1,20240531\nSS\n20240610,2\n";
    let later = "conc\n222,20240603,-1\nSS\n20240612,2\n";
    // Each request's first frame on the wire, and its one bar's close once filed.
    let mut ask = |hmds: &mut HmdsState, req_id: u32, answer: Option<&str>| {
        hmds.send_historical_request_ex(
            req_id, 222, "", "1 M", "1 day", "TRADES", true, false, false,
            "NEWCO", "STK", "SMART", &mut conn, &mut hb, &shared,
        );
        let first = String::from_utf8_lossy(&read_frame(&mut peer)).to_string();
        if let Some(answer) = answer {
            let qid = hmds.pending_adjustments[0].0.clone();
            hmds.process_hmds_message(&conadj_reply(&qid, answer), &mut conn, &shared, &None, &mut hb);
            let _ = read_frame(&mut peer);
        }
        let asked = hmds.pending_historical[0].0.clone();
        hmds.process_hmds_message(
            &adj_bar_msg(&asked, "20240607", 100.0, true), &mut conn, &shared, &None, &mut hb,
        );
        let filed = shared.reference.drain_historical_data();
        (first.contains("10020"), filed[0].1.bars[0].close)
    };
    assert_eq!(ask(&mut hmds, 1, Some(held)), (true, 50.0), "the first request asks");
    assert_eq!(ask(&mut hmds, 2, None), (false, 50.0), "the second, the same day, does not");
    hmds.actions_held.get_mut(&222).unwrap().0 = "20240101".into();
    assert_eq!(ask(&mut hmds, 3, Some(later)), (true, 25.0), "a later day asks, keeps the split and adds its own");
    assert_eq!(ask(&mut hmds, 4, None), (false, 25.0), "and holds that day's answer");
    hmds.actions_held_on = Some(jiff::civil::date(2024, 1, 1));
    assert_eq!(ask(&mut hmds, 5, Some(later)), (true, 50.0), "a new day on this machine lets go of it");
}

/// A series is folded with the actions dated up to the day it is folded on,
/// today on UTC's calendar included, and with none after it, whether or not
/// the logon states NOINEFFECTCONCQUERY. An action whose day cannot be read is
/// refused by the fold rather than dropped as one after it.
#[test]
fn a_series_is_folded_with_the_actions_up_to_today_and_none_after() {
    let day = |days: i64| {
        jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date()
            .checked_add(jiff::Span::new().days(days)).unwrap()
            .strftime("%Y%m%d").to_string()
    };
    // Two days ahead rather than one, so a run across midnight UTC still
    // names a day after the one the series is folded on.
    for (features, dated, close) in [
        (&[][..], day(0), Some(120.888)),
        (&["NOINEFFECTCONCQUERY"][..], day(0), Some(120.888)),
        (&[][..], day(2), Some(1208.88)),
        (&["NOINEFFECTCONCQUERY"][..], day(2), Some(1208.88)),
        (&[][..], format!("{}9", day(2)), None),
    ] {
        let (conn, mut peer) = Connection::for_test();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut conn = Some(conn);
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        shared.reference.set_enabled_features(features.iter().map(|f| f.to_string()).collect());
        let mut hb = HeartbeatState::new();
        let bars = folded_and_answered(
            &mut hmds, &mut conn, &mut peer, &shared, &mut hb, "ADJUSTED_LAST",
            &format!("conc\n756733,-1,-1\nSS\n{dated},10\n"),
        );
        hmds.process_hmds_message(
            &adj_bar_msg(&bars, "20240607", 1208.88, true), &mut conn, &shared, &None, &mut hb,
        );
        let filed = shared.reference.drain_historical_data().first()
            .and_then(|(_, h)| h.bars.first()).map(|bar| bar.close);
        let refused = !shared.reference.drain_historical_errors().is_empty();
        assert!(
            match close {
                Some(close) => filed.is_some_and(|filed| (filed - close).abs() < 1e-6),
                None => filed.is_none() && refused,
            },
            "{features:?}, a split dated {dated}: close {filed:?}, refused {refused}",
        );
    }
}

/// A contract with no corporate action to its name is answered with the echoed
/// query and nothing else, and that answer is the one its bars are asked on.
///
/// The venue states a contract only where it has a record against it, so a
/// young listing is answered with an empty reply. Read as naming the wrong
/// contract, the answer was dropped and the series it was asked for was never
/// filed: a caller got every bar and was never told the series had ended.
#[test]
fn a_contract_with_no_actions_is_answered_and_its_series_filed() {
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(conn);
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();

    hmds.send_historical_request_ex(
        42, 649180671, "", "1 D", "5 mins", "TRADES", true, false, false,
        "NEWCO", "STK", "SMART", &mut conn, &mut hb, &shared,
    );
    let _ = read_frame(&mut peer);
    let qid = hmds.pending_adjustments.iter().find(|(_, rid, _)| *rid == 42)
        .map(|(q, _, _)| q.clone())
        .expect("the actions query is outstanding under this request");

    // The answer a live session was given for a contract with no action: the
    // query echoed, and a body naming no contract and no action.
    let echoed = format!("<ConAdjResponse>\n\t<id>{qid}</id>\n</ConAdjResponse>\n");
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=U\x016040=10022\x016118=");
    msg.extend_from_slice(echoed.as_bytes());
    msg.extend_from_slice(b"\x0196=200\n\n");
    msg.push(0x01);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);
    assert!(hmds.pending_adjustments.is_empty(), "the query is no longer outstanding");

    let bars = String::from_utf8_lossy(&read_frame(&mut peer)).to_string();
    assert!(
        bars.contains("<contractID>649180671</contractID>") && !bars.contains("cutoffDate"),
        "the bars are asked whole under the contract asked about: {bars}",
    );
    let asked = hmds.pending_historical[0].0.clone();
    hmds.process_hmds_message(&make_bar_msg(&asked, true), &mut conn, &shared, &None, &mut hb);

    let filed = shared.reference.drain_historical_data();
    assert_eq!(filed.len(), 1, "the series is filed on an answer that states no action");
    assert_eq!(filed[0].0, 42);
    assert!(filed[0].1.is_complete, "and it says it is the whole answer");
    assert!(hmds.held.is_empty(), "the hold is released");
}

/// When the venue refuses the actions the adjusted series needs, the request is
/// a bar request that failed: it is told the error, and nothing after it, as a
/// gateway tells it, rather than handed back unadjusted or left waiting on a
/// fold that will never come.
#[test]
fn an_adjusted_request_whose_actions_are_refused_is_a_stated_refusal() {
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(conn);
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();

    hmds.send_historical_request_ex(
        42, 756733, "", "1 M", "1 day", "ADJUSTED_LAST", true, false, false,
        "NVDA", "STK", "SMART", &mut conn, &mut hb, &shared,
    );
    let _ = read_frame(&mut peer);
    let qid = hmds.pending_adjustments.iter().find(|(_, rid, _)| *rid == 42)
        .map(|(q, _, _)| q.clone()).expect("the actions query is outstanding");

    // The venue rejects the actions query.
    hmds.process_hmds_message(
        &make_query_error_msg(&qid, "no permission"), &mut conn, &shared, &None, &mut hb,
    );

    assert!(hmds.held.is_empty(), "the hold is dropped, not left waiting");
    assert!(hmds.pending_historical.is_empty(), "and no bar is asked for");
    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 1, "the caller is told why");
    assert_eq!(errors[0].0, 42);
    assert!(
        shared.reference.drain_historical_data().is_empty(),
        "no unadjusted bar is handed back under the adjusted name, and no end follows",
    );
}

/// And the stream half goes with it, on the same rule the batch keeps.
///
/// A request kept up to date is two queries under one number: the batch and a
/// five-second stream beside it. What fails the request fails the stream with
/// it — the batch refusal says so and withdraws it. A refusal of the actions
/// the fold is waiting on ends the request just as finally, and left standing
/// the bars go on arriving under a number the caller has been told failed.
#[test]
fn a_kept_up_to_date_request_refused_on_its_actions_takes_its_stream_with_it() {
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(conn);
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();

    hmds.send_historical_request_ex(
        42, 756733, "", "1 D", "5 mins", "TRADES", true, false, false,
        "NVDA", "STK", "SMART", &mut conn, &mut hb, &shared,
    );
    // The stream half, as a request asked to be kept up to date carries one.
    hmds.keep_up_to_date_reqs.insert(42);
    hmds.rtbar_subs.push(("rt_1".to_string(), 42, Some(9001), 0.01, 1.0));
    hmds.forming_bars.push(FormingBar {
        req_id: 42, seconds: 300, opened_at: 0,
        daily_session: None, closed_at: None,
        bar: crate::types::RealTimeBar::default(), weighted: 0.0, queued: Vec::new(),
    });
    let _ = read_frame(&mut peer);
    let qid = hmds.pending_adjustments.iter().find(|(_, rid, _)| *rid == 42)
        .map(|(q, _, _)| q.clone()).expect("the actions query is outstanding");

    hmds.process_hmds_message(
        &make_query_error_msg(&qid, "no permission"), &mut conn, &shared, &None, &mut hb,
    );

    assert!(
        !hmds.keep_up_to_date_reqs.contains(&42),
        "the stream keeps running under a number the caller was told had failed",
    );
    assert!(hmds.rtbar_subs.is_empty(), "and its subscription record stands: {:?}", hmds.rtbar_subs.len());
    assert!(hmds.forming_bars.is_empty(), "and its part-built bar goes on folding");
}

/// The vendor states its TRADES series as adjusted for splits, though the
/// venue serves it raw: a series crossing a ten-for-one split steps by ten
/// with nothing in it saying so, and every return, moving average and
/// volatility computed over that step is wrong. Measured against a contract
/// that split ten for one on 2024-06-10, where the close before was 1208.88
/// and the close after 121.79. The request asks for the raw trades, and the
/// venue's answer is folded with the actions that move the scale before a bar
/// is handed over.
#[test]
fn a_trades_request_is_folded_with_the_actions_that_move_the_scale() {
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(conn);
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();

    // The venue states a cash dividend and a ten-for-one split. The dividend
    // is a payment out of the price rather than a restatement of it, so it
    // moves nothing; the split is what the series is folded with.
    let bars = folded_and_answered(
        &mut hmds, &mut conn, &mut peer, &shared, &mut hb, "TRADES",
        "conc\n756733,-1,-1\nCD\n20240305,0.04,USD,20240221,20240306,20240327,R,NA\nSS\n20240610,10\n",
    );
    // One complete daily bar, dated before the split.
    hmds.process_hmds_message(
        &adj_bar_msg(&bars, "20240607", 1208.88, true), &mut conn, &shared, &None, &mut hb,
    );

    let filed = shared.reference.drain_historical_data();
    assert_eq!(filed.len(), 1, "the folded series is filed once, complete");
    assert_eq!(filed[0].0, 42);
    assert!(filed[0].1.is_complete);
    let bar = &filed[0].1.bars[0];
    assert!(
        (bar.close - 120.888).abs() < 1e-6,
        "the pre-split close was not put on the split's scale: {}", bar.close,
    );
    assert_eq!(bar.volume, 1000, "the shares before the split count for ten times as many");
    assert!(hmds.held.is_empty(), "the hold is released once folded");
}

/// ADJUSTED_LAST has each cash dividend taken off the bars before it, as a
/// gateway takes it off, and TRADES has none taken off; both have the bars
/// before a rights offer multiplied by its value. The answers are the ones a
/// paper session was given for GE and SIRI, from 2021 on, and the bars are the
/// raw ones it was served around their ex-dates; every figure below was worked
/// by hand from a gateway's rule, and a volume is where the split and the
/// spin-offs leave it.
///
/// - GE: every bar is put on the scale of the spin-offs after it, 0.7803 and
///   0.7975, and those before the 1:8 split of 2 August 2021 on its scale too:
///   4.978314 before it, 0.62228925 from it. The dividend of 0.01 on 25 June is
///   restated by the split and both spin-offs to 0.04978314, and by the two
///   spin-offs the one of 0.08 on 24 September is too: both round to 0.0498.
///   Oldest first, 0.0498 comes off the bar of 24 June, whose close goes from
///   65.4648291 to 65.4150291, and the bar before it takes the ratio
///   0.99923928618; then off the bar of 23 September, from 64.07090118 to
///   64.02110118, and every bar before it takes 0.99922273608.
/// - GE with a rights offer of 0.98 stated twice for 25 June, counted once:
///   TRADES has every bar that ends before that day begins on UTC's clock
///   multiplied by it, those of 23 and 24 June, and the one of the 25th left as
///   it is.
/// - SIRI: the 1:10 split of 10 September 2024 puts every bar at ten times its
///   price. On 10 February 2022 it paid a regular dividend of 0.021962, stated
///   twice and counted once, and a special of 0.25, restated to 0.2196 and 2.5:
///   the special is taken off as the regular one is, and the two sum to 2.7196,
///   which comes off the bar of the 9th, from a close of 68.6 to 65.8804, and
///   every bar before it takes 0.96035568513.
#[test]
fn an_adjusted_series_has_its_dividends_taken_off_and_a_trades_one_does_not() {
    const GE: &str = "200\nconc\n498843743,20210802,-1\n7516,-1,20210801\nCD\n\
        20210305,0.01,USD,20210212,20210308,20210426,R,NA\n\
        20210625,0.01,USD,20210618,20210628,20210726,R,NA\nSS\n20210802,0.125,,20210310\nCD\n\
        20210924,0.08,USD,20210910,20210927,20211025,R,NA\n\
        20211220,0.08,USD,20211210,20211221,20220125,R,NA\nSO\n20211231,1,,20180626\nCD\n\
        20220307,0.08,USD,20220211,20220308,20220425,R,NA\n\
        20220627,0.08,USD,20220617,20220628,20220725,R,NA\n\
        20220926,0.08,USD,20220909,20220927,20221025,R,NA\n\
        20221214,0.08,USD,20221130,20221215,20230125,R,NA\nSO\n20230104,0.7803,,20211109\nCD\n\
        20230306,0.08,USD,20230210,20230307,20230425,R,NA\n\
        20230710,0.08,USD,20230630,20230711,20230725,R,NA\n\
        20230925,0.08,USD,20230908,20230926,20231025,R,NA\n\
        20231227,0.08,USD,20231215,20231228,20240125,R,NA\nSO\n20240402,0.7975,,20211109\nCD\n\
        20240412,0.28,USD,20240405,20240415,20240425,R,NA\n\
        20240711,0.28,USD,20240621,20240711,20240725,R,NA\n\
        20240926,0.28,USD,20240913,20240926,20241025,R,NA\n\
        20241227,0.28,USD,20241213,20241227,20250127,R,NA\n\
        20250310,0.36,USD,20250214,20250310,20250425,R,NA\n\
        20250707,0.36,USD,20250627,20250707,20250725,R,NA\n\
        20250929,0.36,USD,20250918,20250929,20251027,R,NA\n\
        20251229,0.36,USD,20251204,20251229,20260126,R,NA\n\
        20260309,0.47,USD,20260206,20260309,20260427,R,NA\n\
        20260706,0.47,USD,20260625,20260706,20260727,R,NA\n";
    const SIRI: &str = "200\nconc\n727785544,20240910,-1\n138397467,20131115,20240909\n\
        53069174,20080807,20131114\n4727823,-1,20080806\nCD\n\
        20211104,0.021962,USD,20211025,20211105,20211129,R,NA\n\
        20220210,0.021962,USD,20220126,20220211,20220225,R,NA\n\
        20220210,0.021962,USD,20220126,20220211,20220225,R,NA\n\
        20220210,0.25,USD,20220201,20220211,20220225,S,NA\n\
        20220505,0.021962,USD,20220419,20220506,20220525,R,NA\n\
        20220804,0.021962,USD,20220714,20220805,20220831,R,NA\n\
        20221109,0.0242,USD,20221101,20221111,20221130,R,NA\n\
        20230208,0.0242,USD,20230125,20230209,20230224,R,NA\n\
        20230504,0.0242,USD,20230419,20230505,20230524,R,NA\n\
        20230807,0.0242,USD,20230726,20230808,20230830,R,NA\n\
        20231106,0.0266,USD,20231025,20231107,20231129,R,NA\n\
        20240208,0.0266,USD,20240124,20240209,20240223,R,NA\n\
        20240509,0.0266,USD,20240425,20240510,20240529,R,NA\n\
        20240809,0.0266,USD,20240724,20240809,20240826,R,NA\nSS\n20240910,0.1,,20231212\nCD\n\
        20241105,0.27,USD,20241022,20241105,20241121,R,NA\n\
        20250207,0.27,USD,20250122,20250207,20250225,R,NA\n\
        20250509,0.27,USD,20250416,20250509,20250528,R,NA\n\
        20250808,0.27,USD,20250723,20250808,20250827,R,NA\n\
        20251105,0.27,USD,20251022,20251105,20251121,R,NA\n\
        20260211,0.27,USD,20260129,20260211,20260227,R,NA\n\
        20260511,0.27,USD,20260423,20260511,20260527,R,NA\n\
        20260810,0.27,USD,20260722,20260810,20260826,R,NA\n";
    // Day, open, high, low, close, average and volume, as served; the hour the
    // session opened and closed on UTC's clock.
    type Served = (&'static str, f64, f64, f64, f64, f64, i64);
    const GE_BARS: [Served; 8] = [
        ("20210623", 13.02, 13.19, 12.94, 12.95, 13.06396, 39314956),
        ("20210624", 13.06, 13.2, 12.92, 13.15, 13.08036, 39214324),
        ("20210625", 13.16, 13.24, 13.1, 13.16, 13.15533, 26921071),
        ("20210730", 13.16, 13.22, 12.92, 12.95, 13.03118, 49683320),
        ("20210802", 104.48, 107.21, 100.43, 100.6, 102.906, 20463469),
        ("20210923", 99.53, 104.08, 99.52, 102.96, 102.974, 8643277),
        ("20210924", 102.41, 104.2, 102.41, 103.8, 103.694, 4397741),
        ("20210927", 104.55, 106.34, 104.39, 105.35, 105.756, 5638415),
    ];
    const SIRI_BARS: [Served; 6] = [
        ("20220207", 6.78, 6.84, 6.71, 6.74, 6.761, 8748521),
        ("20220208", 6.75, 6.88, 6.71, 6.8, 6.812, 9772239),
        ("20220209", 6.8, 6.87, 6.8, 6.86, 6.833, 13019612),
        ("20220210", 6.55, 6.58, 6.3, 6.33, 6.409, 28886500),
        ("20220211", 6.35, 6.36, 6.2, 6.22, 6.261, 25467529),
        ("20220214", 6.25, 6.31, 6.12, 6.17, 6.183, 17590143),
    ];
    // The closes filed, oldest first, and the bar a dividend came off in full:
    // its day, open, high, low, average and volume.
    type Row = (&'static str, &'static str, u32, String, (&'static str, &'static str),
                &'static [Served], &'static [f64], (&'static str, [f64; 4], i64));
    let offered = GE.replacen("SS\n20210802", "RO\n20210625,0.98,,\n20210625,0.98,,\nSS\n20210802", 1);
    let rows: [Row; 4] = [
        ("GE", "ADJUSTED_LAST", 498843743, GE.into(), ("13:30", "20:00"), &GE_BARS, &[
            64.3700522764, 65.3641843579, 65.4636900955, 64.4190567429, 62.5536400418,
            64.0211011800, 64.5936241500, 65.5581724875,
        ], ("20210923", [61.8866490525, 64.7180651400, 61.8804261600, 64.0298132295], 13889485)),
        ("GE", "TRADES", 498843743, GE.into(), ("13:30", "20:00"), &GE_BARS, &[
            64.4691663000, 65.4648291000, 65.5146122400, 64.4691663000, 62.6022985500,
            64.0709011800, 64.5936241500, 65.5581724875,
        ], ("20210923", [61.9364490525, 64.7678651400, 61.9302261600, 64.0796132295], 13889485)),
        ("GE", "TRADES", 498843743, offered, ("13:30", "20:00"), &GE_BARS, &[
            63.1797829740, 64.1555325180, 65.5146122400, 64.4691663000, 62.6022985500,
            64.0709011800, 64.5936241500, 65.5581724875,
        ], ("20210624", [63.7164452232, 64.3994699040, 63.0334205424, 63.8157765268], 7877029)),
        ("SIRI", "ADJUSTED_LAST", 727785544, SIRI.into(), ("14:30", "21:00"), &SIRI_BARS, &[
            64.7279731778, 65.3041865889, 65.8804, 63.3, 62.2, 61.7,
        ], ("20220209", [65.2804, 65.9804, 65.2804, 65.6104], 1301961)),
    ];
    for (symbol, what_to_show, con_id, answer, (opens, closes), served, filed, (day, prices, volume)) in rows {
        let (conn, mut peer) = Connection::for_test();
        let mut conn = Some(conn);
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        hmds.send_historical_request_ex(
            42, con_id.into(), "", "1 M", "1 day", what_to_show, true, false, false,
            symbol, "STK", "SMART", &mut conn, &mut hb, &shared,
        );
        let _ = read_frame(&mut peer);
        let qid = hmds.pending_adjustments[0].0.clone();
        hmds.process_hmds_message(&conadj_reply(&qid, &answer), &mut conn, &shared, &None, &mut hb);
        let _ = read_frame(&mut peer);
        let asked = hmds.pending_historical[0].0.clone();
        let bars: String = served.iter().map(|(at, open, high, low, close, wap, volume)| format!(
            "<Bar><time>{at}-{opens}:00</time><endTime>{at}-{closes}:00</endTime><open>{open}</open>\
             <close>{close}</close><high>{high}</high><low>{low}</low><weightedAvg>{wap}</weightedAvg>\
             <volume>{volume}</volume><count>1</count></Bar>",
        )).collect();
        let xml = format!(
            "<ResultSetBar><id>{asked}</id><eoq>true</eoq><tz>US/Eastern</tz><Events>{bars}</Events>\
             </ResultSetBar>",
        );
        hmds.process_hmds_message(
            &[b"35=W\x016118=".as_slice(), xml.as_bytes(), b"\x01"].concat(), &mut conn, &shared, &None, &mut hb,
        );

        let series = shared.reference.drain_historical_data();
        assert!(shared.reference.drain_historical_errors().is_empty(), "{symbol} {what_to_show}: not refused");
        let bars = &series[0].1.bars;
        let near = |got: f64, wanted: f64| (got - wanted).abs() < 1e-8;
        let got: Vec<f64> = bars.iter().map(|b| b.close).collect();
        assert!(
            got.len() == filed.len() && got.iter().zip(filed).all(|(got, wanted)| near(*got, *wanted)),
            "{symbol} {what_to_show}: closes {got:?}, wanted {filed:?}",
        );
        let bar = bars.iter().find(|b| b.time.starts_with(day)).unwrap();
        assert!(
            near(bar.open, prices[0]) && near(bar.high, prices[1]) && near(bar.low, prices[2])
                && near(bar.wap, prices[3]) && bar.volume == volume,
            "{symbol} {what_to_show}: the bar of {day} reads {bar:?}",
        );
    }
}

/// A TRADES request whose contract states an action this client cannot name
/// is refused rather than handed back raw: an action it cannot classify is
/// one it cannot say moves nothing, and folding without it is the wrong
/// number under an adjusted name arriving by its own back door. The adjusted
/// path refuses the same shape; this states it for the default series too.
#[test]
fn a_trades_request_with_an_action_nobody_can_name_is_refused() {
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(conn);
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();

    // Asked under the documented default, which is TRADES; the venue names an
    // action this client does not know.
    let bars = folded_and_answered(
        &mut hmds, &mut conn, &mut peer, &shared, &mut hb, "",
        "conc\n756733,-1,-1\nZZ\n20240610,10,,20240522\n",
    );
    hmds.process_hmds_message(
        &adj_bar_msg(&bars, "20240607", 1208.88, true), &mut conn, &shared, &None, &mut hb,
    );

    assert!(hmds.held.is_empty(), "the hold is dropped, not left waiting");
    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 1, "the caller is told why");
    assert_eq!(errors[0].0, 42);
    assert!(
        shared.reference.drain_historical_data().is_empty(),
        "no raw bar is handed back under an adjusted name, and no end follows",
    );
    assert_eq!(over(&shared), [42], "and the request is over");
}

/// A gateway asks every bar query for a stock or a fund along the contract's
/// id history, whatever series it names, and folds it by what the series is:
/// a kind priced as the contract trades is put on the scale of its splits,
/// and any other kind has only the bars before a rights offer multiplied by
/// it. A query for another kind of contract is asked as it was made, with no
/// actions asked for, and filed as the venue served it. Every row states a
/// two-for-one split on 10 June 2024 and a rights offer of 0.9 on the 8th, and
/// is answered with one daily bar on the 7th, at a close of 100.
#[test]
fn a_series_is_asked_along_the_ids_and_folded_by_what_it_is() {
    let actions = "conc\n756733,-1,-1\nRO\n20240608,0.9,,\nSS\n20240610,2\n";
    for (sec_type, what_to_show, asked_along, close) in [
        ("STK", "MIDPOINT", true, 45.0),
        ("STK", "BID_ASK", true, 45.0),
        ("FUND", "NAV_LAST", true, 45.0),
        ("STK", "HISTORICAL_VOLATILITY", true, 90.0),
        ("STK", "OPTION_IMPLIED_VOLATILITY", true, 90.0),
        ("FUT", "TRADES", false, 100.0),
        ("CASH", "MIDPOINT", false, 100.0),
    ] {
        let (conn, mut peer) = Connection::for_test();
        let mut conn = Some(conn);
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        hmds.send_historical_request_ex(
            42, 756733, "", "1 M", "1 day", what_to_show, true, false, false,
            "NVDA", sec_type, "SMART", &mut conn, &mut hb, &shared,
        );
        let first = String::from_utf8_lossy(&read_frame(&mut peer)).to_string();
        assert_eq!(first.contains("10020"), asked_along, "{sec_type} {what_to_show}: {first}");
        if asked_along {
            let qid = hmds.pending_adjustments[0].0.clone();
            hmds.process_hmds_message(&conadj_reply(&qid, actions), &mut conn, &shared, &None, &mut hb);
            let _ = read_frame(&mut peer);
        }
        let asked = hmds.pending_historical[0].0.clone();
        hmds.process_hmds_message(
            &adj_bar_msg(&asked, "20240607", 100.0, true), &mut conn, &shared, &None, &mut hb,
        );
        let filed = shared.reference.drain_historical_data();
        let bar = &filed[0].1.bars[0];
        assert!((bar.close - close).abs() < 1e-9, "{sec_type} {what_to_show}: close {}", bar.close);
    }
}

/// One page of a bar reply, stating a zone or, given none, omitting the tag as
/// the venue's later pages do.
fn bar_page_msg(query_id: &str, eoq: bool, tz: &str) -> Vec<u8> {
    let tz_tag = if tz.is_empty() { String::new() } else { format!("<tz>{tz}</tz>") };
    let xml = format!(
        "<ResultSetBar><id>{query_id}</id><eoq>{eoq}</eoq>{tz_tag}<Events>\
         <Bar><time>20260714-13:30:00</time><open>1</open><close>1</close>\
         <high>1</high><low>1</low><weightedAvg>1</weightedAvg>\
         <volume>1</volume><count>1</count></Bar></Events></ResultSetBar>",
    );
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    msg
}

/// The venue does not put `<tz>` on every page of a series, and the zone is
/// read off the page: a page that omits it formatted nothing, and every bar on
/// it reached the caller verbatim where the first page's were written on the
/// caller's clock — one series, two spellings, decided by which page a bar
/// landed on, and a completion whose range was computed on no clock at all.
/// The zone belongs to the query: the first page that states one states it for
/// the series.
#[test]
fn a_page_that_states_no_zone_takes_the_one_the_series_stated() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<crate::protocol::connection::Connection> = None;
    hmds.pending_historical.push(("hist_1".to_string(), 7));
    hmds.held.push(HeldSeries {
        req_id: 7, fold: Fold::None, bars: Vec::new(), timezone: String::new(), actions_query: None, actions: None, complete: false,
        along: Default::default(),
    });

    hmds.process_hmds_message(&bar_page_msg("hist_1", false, "US/Eastern"), &mut conn, &shared, &None, &mut hb);
    hmds.process_hmds_message(&bar_page_msg("hist_1", false, ""), &mut conn, &shared, &None, &mut hb);
    hmds.process_hmds_message(&bar_page_msg("hist_1", true, ""), &mut conn, &shared, &None, &mut hb);

    // One series, on the one zone it stated, whose range the completion is
    // computed on.
    let filed = shared.reference.drain_historical_data();
    assert_eq!(filed.len(), 1, "the series is filed once, whole");
    assert_eq!(filed[0].1.timezone, "US/Eastern", "on the zone the series stated");
    assert_eq!(filed[0].1.bars.len(), 3, "every page's bars in it");
    assert!(filed[0].1.is_complete);
    assert!(hmds.held.is_empty(), "and nothing is held once it has answered");
}

/// One page of a bar reply holding a bar on each of the given days, in the
/// order given.
fn page_of(query_id: &str, days: &[&str], eoq: bool) -> Vec<u8> {
    let bars: String = days.iter().map(|d| format!(
        "<Bar><time>{d}-13:30:00</time><open>1</open><close>1</close><high>1</high>\
         <low>1</low><weightedAvg>1</weightedAvg><volume>1</volume><count>1</count></Bar>",
    )).collect();
    let xml = format!(
        "<ResultSetBar><id>{query_id}</id><eoq>{eoq}</eoq><tz>US/Eastern</tz>\
         <Events>{bars}</Events></ResultSetBar>",
    );
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    msg
}

/// The venue pages a long series and the pages arrive newest first, each
/// ascending within itself. Handed over in arrival order the series steps
/// backwards at every page boundary: a plot of it is a sawtooth, a return
/// between consecutive bars is nonsense once per page, and the last bar is
/// years old. The reference client hands bars over oldest first; so does this,
/// for every historical request. Measured against the paper account: three
/// years of daily bars, five pages, four backward steps.
#[test]
fn a_paged_series_is_delivered_oldest_first_whatever_order_the_pages_arrive_in() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<crate::protocol::connection::Connection> = None;
    hmds.pending_historical.push(("hist_1".to_string(), 7));
    hmds.held.push(HeldSeries {
        req_id: 7, fold: Fold::None, bars: Vec::new(), timezone: String::new(), actions_query: None, actions: None, complete: false,
        along: Default::default(),
    });

    // Newest page first, oldest last, as the venue sends them.
    hmds.process_hmds_message(&page_of("hist_1", &["20260901", "20260902"], false), &mut conn, &shared, &None, &mut hb);
    hmds.process_hmds_message(&page_of("hist_1", &["20250327", "20250328"], false), &mut conn, &shared, &None, &mut hb);
    hmds.process_hmds_message(&page_of("hist_1", &["20230905", "20230906"], true), &mut conn, &shared, &None, &mut hb);

    let filed = shared.reference.drain_historical_data();
    assert!(filed.last().is_some_and(|(_, r)| r.is_complete), "the series ends");
    let days: Vec<String> = filed.iter()
        .flat_map(|(_, r)| r.bars.iter().map(|b| b.time[..8].to_string()))
        .collect();
    assert_eq!(days.len(), 6, "every bar is present");
    assert!(
        days.windows(2).all(|w| w[0] < w[1]),
        "the series must ascend across page boundaries, oldest first: {days:?}",
    );
}

/// Read until the query XML has closed. A single `read` can legally return
/// a partial frame, which would make these tests fail intermittently while
/// production is correct.
fn read_frame(peer: &mut std::net::TcpStream) -> Vec<u8> {
    use std::io::Read;
    let mut acc = Vec::new();
    let mut buf = vec![0u8; 8192];
    for _ in 0..64 {
        match peer.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                // Stop on a complete frame, not on a field in the middle
                // of one: a split right after the query would otherwise
                // return a truncated read that happens to satisfy the
                // assertions above it.
                // A plain FIX frame ends with its checksum field; a
                // compressed one states its own length.
                let ends_with_trailer = acc.len() >= 7
                    && acc[acc.len() - 1] == 0x01
                    && acc[acc.len() - 7..acc.len() - 4] == *b"\x0110=";
                let complete = ends_with_trailer
                    || crate::protocol::fixcomp::fixcomp_length(&acc)
                        .is_some_and(|len| acc.len() >= len);
                if complete {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    acc
}
mod tick_ack_tests {
    use super::super::{parse_tick_subscription_ack, scaled_size};

    /// The venue's answer, taken from a live session. It names the
    /// subscription back, states the number it will use on every frame, and
    /// states the increments prices and sizes move in — none of which is
    /// stated anywhere else on this connection.
    #[test]
    fn the_acknowledgement_states_the_number_and_both_increments() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\t<ResultSetTickerId>\n\
                   \t\t<id>tbt_1</id>\n\t\t<rtTickerId>1</rtTickerId>\n\
                   \t\t<minTick>0.00005</minTick>\n\t\t<sizeMinTick>1</sizeMinTick>\n\
                   \t\t<eoq>false</eoq>\n\t</ResultSetTickerId>";
        let ack = parse_tick_subscription_ack(xml).expect("the acknowledgement reads");
        assert_eq!(ack.query_id, "tbt_1");
        assert_eq!(ack.venue_id, 1);
        assert_eq!(ack.min_tick, 0.00005);
        assert_eq!(ack.size_min_tick, 1.0);
    }

    /// A crypto deals in hundred-millionths, and says so.
    #[test]
    fn a_contract_dealt_in_fractions_states_a_fractional_size_increment() {
        let xml = "<ResultSetTickerId><id>tbt_2</id><rtTickerId>2</rtTickerId>\
                   <minTick>0.25</minTick><sizeMinTick>0.00000001</sizeMinTick></ResultSetTickerId>";
        let ack = parse_tick_subscription_ack(xml).expect("the acknowledgement reads");
        assert_eq!(ack.venue_id, 2, "and its number is its own, not the one asked under");
        assert_eq!(ack.size_min_tick, 0.00000001);
    }

    /// Some other reply on the same connection is not an acknowledgement.
    #[test]
    fn another_reply_is_not_read_as_an_acknowledgement() {
        assert!(parse_tick_subscription_ack("<QueryError><id>tbt_1</id></QueryError>").is_none());
        assert!(parse_tick_subscription_ack("<ResultSetBar><id>h_1</id></ResultSetBar>").is_none());
    }

    /// A size is a count of what the venue said sizes move in. Counting a
    /// crypto's size in whole ones reports it a hundred million times too large.
    #[test]
    fn a_size_is_counted_in_what_the_venue_said_it_moves_in() {
        // A share: whole ones, held in the form readers divide by.
        assert_eq!(scaled_size(100, 1.0), 100 * crate::types::QTY_SCALE);
        // A crypto: a hundred million counts is one whole unit.
        assert_eq!(scaled_size(100_000_000, 0.00000001), crate::types::QTY_SCALE);
        // Stating no increment means whole ones.
        assert_eq!(scaled_size(5, 0.0), 5 * crate::types::QTY_SCALE);
    }
}
mod counted_size_ceiling_tests {
    use super::super::scaled_size;

    /// A count past what a quantity can hold is held at the largest one. Cast
    /// straight through it comes out negative, and a size that reads as
    /// negative is a sell where there was a buy.
    #[test]
    fn a_count_past_the_ceiling_does_not_come_back_negative() {
        assert!(scaled_size(u64::MAX, 1.0) > 0, "a size came back negative");
        assert!(scaled_size(u64::MAX, 1e-8) > 0);
    }

    /// An ordinary count is untouched.
    #[test]
    fn an_ordinary_count_is_untouched() {
        assert_eq!(scaled_size(100, 1.0), 100 * crate::types::QTY_SCALE);
    }
}
mod withdrawing_one_stream_tests {
    use super::super::*;

    fn stream(caller_req_id: i64, instrument: InstrumentId, kind: TbtType) -> TbtSubscription {
        TbtSubscription {
            ignore_size: false,
            instrument,
            query_id: format!("tbt_{caller_req_id}"),
            kind,
            caller_req_id,
            venue_id: caller_req_id as u64,
            min_tick: 1,
            size_tick: 1.0,
            running: Default::default(),
        }
    }

    /// A trade says which of the two trade streams it arrived on.
    ///
    /// The callback names the stream it carries — every trade, or the
    /// exchange's own — and the subscription is what decided it. Written down
    /// at the call instead, a second request under a number already carrying
    /// a stream, which is refused, relabelled every print of the stream that
    /// was running.
    #[test]
    fn a_trade_says_which_stream_it_arrived_on() {
        let hex: String = crate::protocol::tbt_stream::A_CAPTURED_TRADE_FRAME.split_whitespace().collect();
        let frame: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let start = frame.windows(5).position(|w| w == b"35=E\x01").unwrap() + 5;
        let end = frame.windows(6).position(|w| w == b"\x018349=").unwrap();
        let number = crate::protocol::tbt_stream::Records::new(&frame[start..end])
            .and_then(|mut records| records.next_stream())
            .map(|(stream, _)| stream)
            .expect("the frame names its stream");
        for kind in [TbtType::AllLast, TbtType::Last] {
            let mut hmds = HmdsState::new();
            let shared = crate::bridge::SharedState::new();
            let mut sub = stream(1, 7, kind);
            sub.venue_id = number;
            hmds.tbt_subscriptions.push(sub);
            hmds.process_hmds_message(&frame, &mut None, &shared, &None, &mut HeartbeatState::new());
            let heard = shared.market.drain_tbt_trades();
            assert!(!heard.is_empty(), "the frame carries trades");
            assert!(
                heard.iter().all(|t| t.kind == kind),
                "each print names the stream it arrived on, {kind:?}: {heard:?}",
            );
        }
    }

    /// Two streams on one contract share frames, and each record is read as
    /// its own stream's.
    ///
    /// A frame the venue sent with every trade and every quote change open on
    /// the same crypto, at 08:43 UTC on the twenty-sixth of September 2026: ten
    /// records, quotes on the second stream with two trades on the first among
    /// them. The market stood at 84,272.75 bid, 84,273.50 offered, and the
    /// contract moves in a quarter. Read by the kind of the frame's first
    /// record, the trades were lost and every quote after the first came back
    /// at twice the market, with sizes in the tens of billions.
    ///
    /// The venue goes on sending a withdrawn stream's records, among those of
    /// the stream still open: they are stepped over by their length, and the
    /// quotes after them are still read. So is a record of a stream this
    /// session never held, measured by where the next record's moment falls.
    #[test]
    fn records_of_two_streams_in_one_frame_are_each_read_as_their_own() {
        let hex: String =
            crate::protocol::tbt_stream::A_CAPTURED_INTERLEAVED_FRAME.split_whitespace().collect();
        let frame: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let quarter = crate::types::PRICE_SCALE / 4;
        let both_trades =
            [(1, 336_982 * quarter, 26, 1_790_412_160), (1, 337_092 * quarter, 3_488, 1_790_412_183)];
        for (trades_withdrawn, expected_trades) in [(false, &both_trades[..]), (true, &[][..])] {
            let mut hmds = HmdsState::new();
            let shared = crate::bridge::SharedState::new();
            for (caller, kind) in [(1, TbtType::AllLast), (2, TbtType::BidAsk)] {
                let mut sub = stream(caller, 7, kind);
                sub.min_tick = quarter;
                sub.size_tick = 1e-8;
                hmds.tbt_subscriptions.push(sub);
            }
            if trades_withdrawn {
                hmds.send_tbt_unsubscribe(1, 7, &mut None, &mut HeartbeatState::new());
            }

            hmds.process_hmds_message(&frame, &mut None, &shared, &None, &mut HeartbeatState::new());

            let trades: Vec<_> = shared.market.drain_tbt_trades().into_iter()
                .map(|t| (t.req_id, t.price, t.size, t.timestamp)).collect();
            assert_eq!(
                trades, expected_trades,
                "the trades, on the stream that carries them (withdrawn: {trades_withdrawn})",
            );
            let quotes = shared.market.drain_tbt_quotes();
            assert_eq!(quotes.len(), 8, "eight quote changes (withdrawn: {trades_withdrawn}): {quotes:?}");
            assert!(
                quotes.iter().all(|q| q.req_id == 2 && q.bid == 337_091 * quarter && q.ask == 337_094 * quarter),
                "every quote at the market that was there: {quotes:?}",
            );
            assert_eq!(
                quotes.last().map(|q| (q.bid_size, q.ask_size)),
                Some((57_112, 2_373_231)),
                "and the last one's sizes, read from where that record starts",
            );
        }

        // A record of a stream this session never held, ahead of a quote on the
        // stream it does hold.
        let mut hmds = HmdsState::new();
        let shared = crate::bridge::SharedState::new();
        let mut held = stream(2, 7, TbtType::BidAsk);
        held.min_tick = quarter;
        held.size_tick = 1e-8;
        hmds.tbt_subscriptions.push(held);
        let field = |mut value: u64| {
            let mut groups = Vec::new();
            loop {
                groups.push((value & 0x7F) as u8);
                value >>= 7;
                if value == 0 {
                    break;
                }
            }
            groups.reverse();
            *groups.last_mut().unwrap() |= 0x80;
            groups
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload: Vec<u8> = [9, now, 5, 6, 0, 1, 1, 2, now, 337_091, 337_094, 0, 60_600, 2_373_227]
            .into_iter()
            .flat_map(field)
            .collect();
        let mut message = b"35=E\x01".to_vec();
        message.extend_from_slice(&((payload.len() * 8) as u16).to_be_bytes());
        message.extend_from_slice(&payload);

        hmds.process_hmds_message(&message, &mut None, &shared, &None, &mut HeartbeatState::new());

        let quotes: Vec<_> = shared.market.drain_tbt_quotes().into_iter()
            .map(|q| (q.req_id, q.bid, q.ask, q.bid_size, q.ask_size)).collect();
        assert_eq!(
            quotes,
            [(2, 337_091 * quarter, 337_094 * quarter, 60_600, 2_373_227)],
            "the quote after a record of a stream never held",
        );
    }

    /// A contract can carry two streams — every trade, and every quote change.
    /// Withdrawing one by naming the contract took whichever was opened first
    /// and left the caller's own running.
    #[test]
    fn withdrawing_one_stream_leaves_the_other() {
        let mut hmds = HmdsState::new();
        hmds.tbt_subscriptions.push(stream(1, 7, TbtType::Last));
        hmds.tbt_subscriptions.push(stream(2, 7, TbtType::BidAsk));

        hmds.send_tbt_unsubscribe(2, 7, &mut None, &mut HeartbeatState::new());

        assert_eq!(hmds.tbt_subscriptions.len(), 1, "one stream was withdrawn");
        assert_eq!(
            hmds.tbt_subscriptions[0].caller_req_id, 1,
            "the wrong stream was withdrawn",
        );
    }

    /// A trade stream is asked for by the name the caller used.
    ///
    /// The venue serves each of the three as a query of its own. Both trade
    /// streams went out under one name and the other was made here, by
    /// dropping the prints the venue marks as not reported to the tape — which
    /// is not what the venue means by it: that stream, asked for by name,
    /// carries those prints too. What was handed to a caller as the narrower
    /// stream was a series the venue does not serve.
    #[test]
    fn a_trade_stream_is_asked_for_by_the_name_the_caller_used() {
        assert_eq!(HmdsState::tbt_wire_kind(TbtType::Last), "Last");
        assert_eq!(HmdsState::tbt_wire_kind(TbtType::AllLast), "AllLast");
        assert_eq!(HmdsState::tbt_wire_kind(TbtType::BidAsk), "BidAsk");
        assert_eq!(HmdsState::tbt_wire_kind(TbtType::MidPoint), "MidPoint");
    }

    /// A withdrawal reaches the stream it names and no other. Where the name
    /// matched nothing it fell back on "the one stream on this contract",
    /// which was never the caller's: the venue had refused the caller's
    /// stream and it was already gone, so the fallback took the other
    /// caller's stream on the contract, and that caller was told nothing.
    #[test]
    fn a_withdrawal_naming_nothing_leaves_the_other_stream_on_the_contract() {
        let mut hmds = HmdsState::new();
        hmds.tbt_subscriptions.push(stream(2, 7, TbtType::BidAsk));
        hmds.send_tbt_unsubscribe(1, 7, &mut None, &mut HeartbeatState::new());
        assert_eq!(hmds.tbt_subscriptions.len(), 1, "the other caller's stream stands");
    }

    fn withdrawal_while_a_sibling_is_unnumbered(deferred: bool) {
        for (gone_kind, kept_kind, kept_instrument, shares) in [
            (TbtType::Last, TbtType::Last, 7, true),
            (TbtType::BidAsk, TbtType::BidAsk, 7, true),
            // The venue answers each name as a query of its own, so two
            // callers who asked by different names hold two streams and
            // neither withdrawal touches the other's.
            (TbtType::Last, TbtType::AllLast, 7, false),
            (TbtType::AllLast, TbtType::Last, 7, false),
            (TbtType::Last, TbtType::BidAsk, 7, false),
            (TbtType::Last, TbtType::Last, 8, false),
        ] {
            let mut hmds = HmdsState::new();
            let shared = crate::bridge::SharedState::new();
            let (conn, mut peer) = crate::protocol::connection::Connection::for_test();
            peer.set_read_timeout(Some(std::time::Duration::from_millis(100))).unwrap();
            let mut conn = Some(conn);
            let mut hb = HeartbeatState::new();
            let mut gone = stream(1, 7, gone_kind);
            gone.venue_id = if deferred { 0 } else { 41 };
            hmds.tbt_subscriptions.push(gone);
            let mut kept = stream(2, kept_instrument, kept_kind);
            kept.venue_id = 0;
            hmds.tbt_subscriptions.push(kept);

            hmds.send_tbt_unsubscribe(1, 7, &mut conn, &mut hb);
            if deferred {
                let by_name = super::read_frame(&mut peer);
                assert!(String::from_utf8_lossy(&by_name).contains("<id>tbt_1</id>"));
                let ack = b"35=W\x016118=<ResultSetTickerId><id>tbt_1</id><rtTickerId>41</rtTickerId>\
                    <minTick>0.01</minTick><sizeMinTick>1</sizeMinTick></ResultSetTickerId>\x01";
                hmds.process_hmds_message(ack, &mut conn, &shared, &None, &mut hb);
                assert!(hmds.tbt_withdrawn_unnumbered.is_empty());
            }
            let withdrawal = super::read_frame(&mut peer);
            if shares {
                assert!(withdrawal.is_empty(), "the sibling is still waiting for the shared number");
                assert!(!hmds.tbt_withdrawn.contains_key(&41));
            } else {
                assert!(String::from_utf8_lossy(&withdrawal).contains("<id>rtTicker:41</id>"),
                    "another contract or wire kind does not hold this stream");
                assert!(hmds.tbt_withdrawn.contains_key(&41));
            }

            let number = if shares { 41 } else { 42 };
            let ack = format!("35=W\x016118=<ResultSetTickerId><id>tbt_2</id><rtTickerId>{number}</rtTickerId>\
                <minTick>0.01</minTick><sizeMinTick>1</sizeMinTick></ResultSetTickerId>\x01");
            hmds.process_hmds_message(ack.as_bytes(), &mut conn, &shared, &None, &mut hb);
            assert_eq!(hmds.tbt_subscriptions[0].venue_id, number);
            hmds.send_tbt_unsubscribe(2, kept_instrument, &mut conn, &mut hb);
            let final_withdrawal = super::read_frame(&mut peer);
            assert!(String::from_utf8_lossy(&final_withdrawal).contains(&format!("<id>rtTicker:{number}</id>")),
                "the last caller's withdrawal reaches the venue");
            assert!(hmds.tbt_subscriptions.is_empty());
        }
    }

    /// The venue has not numbered the second caller's query yet, but it is
    /// already asking for the same stream that the first caller leaves.
    #[test]
    fn a_numbered_withdrawal_keeps_an_unnumbered_siblings_stream() {
        withdrawal_while_a_sibling_is_unnumbered(false);
    }

    /// The first acknowledgement can arrive after its caller leaves and
    /// before the second caller is numbered. That stream is still wanted.
    #[test]
    fn a_deferred_withdrawal_keeps_an_unnumbered_siblings_stream() {
        withdrawal_while_a_sibling_is_unnumbered(true);
    }

    /// A stream withdrawn before the venue has numbered it is withdrawn by
    /// that number when the acknowledgement arrives.
    #[test]
    fn a_stream_withdrawn_before_its_acknowledgement_is_withdrawn_at_it() {
        let mut hmds = HmdsState::new();
        let shared = crate::bridge::SharedState::new();
        let (conn, mut peer) = crate::protocol::connection::Connection::for_test();
        peer.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
        let mut conn = Some(conn);
        let mut hb = HeartbeatState::new();
        let mut unnumbered = stream(1, 7, TbtType::Last);
        unnumbered.venue_id = 0;
        hmds.tbt_subscriptions.push(unnumbered);

        hmds.send_tbt_unsubscribe(1, 7, &mut conn, &mut hb);
        let by_name = String::from_utf8_lossy(&super::read_frame(&mut peer)).into_owned();
        assert!(by_name.contains("<id>tbt_1</id>"), "withdrawn by name for now: {by_name}");

        let ack = b"35=W\x016118=<ResultSetTickerId><id>tbt_1</id><rtTickerId>41</rtTickerId>\
                    <minTick>0.01</minTick><sizeMinTick>1</sizeMinTick></ResultSetTickerId>\x01";
        hmds.process_hmds_message(ack, &mut conn, &shared, &None, &mut hb);
        let by_number = String::from_utf8_lossy(&super::read_frame(&mut peer)).into_owned();
        assert!(by_number.contains("<id>rtTicker:41</id>"), "withdrawn by the venue's number: {by_number:?}");
        assert!(hmds.tbt_withdrawn.contains_key(&41), "and its ticks are known as withdrawn");
        assert!(hmds.tbt_subscriptions.is_empty(), "nothing is reopened by the acknowledgement");
    }

    /// The venue answers a second query on a contract and kind with the
    /// number it gave the first, so two callers share one stream. Every
    /// record reaches both, and the first withdrawal leaves the stream
    /// running for the other: routed to whichever subscription came first,
    /// the second caller heard nothing, and the first caller's withdrawal
    /// stopped the stream the second was still reading.
    #[test]
    fn two_callers_sharing_the_venues_number_both_hear_it_and_the_last_to_leave_withdraws_it() {
        let mut hmds = HmdsState::new();
        let shared = crate::bridge::SharedState::new();
        let (conn, mut peer) = crate::protocol::connection::Connection::for_test();
        peer.set_read_timeout(Some(std::time::Duration::from_millis(300))).unwrap();
        let mut conn = Some(conn);
        let mut hb = HeartbeatState::new();
        let hex = crate::protocol::tbt_stream::A_CAPTURED_QUOTE_FRAME;
        let frame: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let start = frame.windows(5).position(|w| w == b"35=E\x01").unwrap() + 5;
        let end = frame.windows(6).position(|w| w == b"\x018349=").unwrap();
        let number = crate::protocol::tbt_stream::Records::new(&frame[start..end])
            .and_then(|mut records| records.next_stream())
            .map(|(stream, _)| stream)
            .expect("the frame names its stream");
        for caller in [1, 2] {
            let mut sub = stream(caller, 7, TbtType::BidAsk);
            sub.venue_id = number;
            sub.min_tick = (0.00005 * crate::types::PRICE_SCALE as f64).round() as i64;
            hmds.tbt_subscriptions.push(sub);
        }
        hmds.process_hmds_message(&frame, &mut conn, &shared, &None, &mut hb);
        let heard: Vec<i64> = shared.market.drain_tbt_quotes().into_iter().map(|q| q.req_id).collect();
        assert_eq!(heard.iter().filter(|r| **r == 1).count(), 5, "{heard:?}");
        assert_eq!(heard.iter().filter(|r| **r == 2).count(), 5, "both callers hear every record: {heard:?}");

        hmds.send_tbt_unsubscribe(1, 7, &mut conn, &mut hb);
        assert!(super::read_frame(&mut peer).is_empty(), "the stream is left running for the other caller");
        assert!(!hmds.tbt_withdrawn.contains_key(&number), "and is not marked withdrawn");
        hmds.send_tbt_unsubscribe(2, 7, &mut conn, &mut hb);
        let withdrawn = String::from_utf8_lossy(&super::read_frame(&mut peer)).into_owned();
        assert!(withdrawn.contains(&format!("<id>rtTicker:{number}</id>")), "the last to leave withdraws it: {withdrawn:?}");
        assert!(hmds.tbt_withdrawn.contains_key(&number));
    }

    /// A caller taken on under a number another caller already holds joins
    /// that stream where it stands. A price is not sent — a move from the
    /// last one is — so a subscription anchored at nothing reads the frames
    /// the venue is already sending as though the stream began with it, and
    /// the caller is handed prices that are those moves added up. Nothing on
    /// the wire or in the log says so: the two callers simply disagree about
    /// what the market is.
    #[test]
    fn a_caller_joining_a_running_stream_is_anchored_where_that_stream_stands() {
        let mut hmds = HmdsState::new();
        let shared = crate::bridge::SharedState::new();
        let mut conn = None;
        let mut hb = HeartbeatState::new();
        let hex = crate::protocol::tbt_stream::A_CAPTURED_QUOTE_FRAME;
        let frame: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let start = frame.windows(5).position(|w| w == b"35=E\x01").unwrap() + 5;
        let end = frame.windows(6).position(|w| w == b"\x018349=").unwrap();
        let number = crate::protocol::tbt_stream::Records::new(&frame[start..end])
            .and_then(|mut records| records.next_stream())
            .map(|(stream, _)| stream)
            .expect("the frame names its stream");
        let scaled = (0.00005 * crate::types::PRICE_SCALE as f64).round() as i64;

        // The first caller, which has been reading the stream for a while.
        let mut first = stream(1, 7, TbtType::BidAsk);
        first.venue_id = number;
        first.min_tick = scaled;
        hmds.tbt_subscriptions.push(first);
        hmds.process_hmds_message(&frame, &mut conn, &shared, &None, &mut hb);
        let opening = shared.market.drain_tbt_quotes();
        assert_eq!(opening.len(), 5, "the stream has moved on before the second caller arrives");

        // The second, acknowledged under the number the first already holds.
        let mut second = stream(2, 7, TbtType::BidAsk);
        second.venue_id = 0;
        hmds.tbt_subscriptions.push(second);
        let ack = format!(
            "35=W\x016118=<ResultSetTickerId><id>tbt_2</id><rtTickerId>{number}</rtTickerId>\
             <minTick>0.00005</minTick><sizeMinTick>1</sizeMinTick></ResultSetTickerId>\x01",
        );
        hmds.process_hmds_message(ack.as_bytes(), &mut conn, &shared, &None, &mut hb);

        hmds.process_hmds_message(&frame, &mut conn, &shared, &None, &mut hb);
        let heard = shared.market.drain_tbt_quotes();
        let quoted = |req_id: i64| -> Vec<(i64, i64)> {
            heard.iter().filter(|q| q.req_id == req_id).map(|q| (q.bid, q.ask)).collect()
        };
        assert_eq!(quoted(1).len(), 5, "the first caller hears the frame");
        assert_ne!(
            quoted(1)[0], (opening[0].bid, opening[0].ask),
            "the moves in this frame are steps from where the stream had got to",
        );
        assert_eq!(
            quoted(2), quoted(1),
            "one stream is one set of prices; the caller that joined it read the moves from zero",
        );
    }
}
mod counted_size_range_tests {
    use super::super::scaled_size;

    /// A count too large to hold as sent is still held when the increment
    /// shrinks it back into range. Cut down to fit before scaling, it came
    /// back as a number that was plausible and was not the venue's.
    #[test]
    fn an_increment_that_shrinks_a_count_keeps_it() {
        let counted = u64::MAX / 4;
        let held = scaled_size(counted, 1e-8);
        let expected = (counted as f64 * 1e-8 * crate::types::QTY_SCALE as f64).round() as i64;
        assert_eq!(held, expected, "the increment was applied to a count that had been cut");
    }
}
mod forming_bar_tests {
    use super::super::*;

    fn five(timestamp: u32, open: f64, high: f64, low: f64, close: f64, volume: f64) -> crate::types::RealTimeBar {
        crate::types::RealTimeBar {
            timestamp, open, high, low, close, volume, wap: close, count: 1,
        }
    }

    /// A caller keeping five-minute bars up to date hears its own bar as it
    /// forms, not the five-second bars it is made of.
    #[test]
    fn the_forming_bar_is_folded_from_what_the_venue_streams() {
        let mut forming = FormingBar {
            req_id: 1, seconds: 300, opened_at: 0,
            daily_session: None, closed_at: None,
            bar: Default::default(), weighted: 0.0, queued: Vec::new(),
        };
        // 14:35:00, then two more within the same five minutes.
        let first = forming.fold(&five(1_786_456_500, 10.0, 10.5, 9.5, 10.2, 100.0)).unwrap();
        assert_eq!(first.timestamp, 1_786_456_500, "the bar opens on its own boundary");

        forming.fold(&five(1_786_456_505, 10.2, 11.0, 10.1, 10.8, 50.0)).unwrap();
        let so_far = forming.fold(&five(1_786_456_510, 10.8, 10.9, 9.0, 9.4, 50.0)).unwrap();
        assert_eq!(so_far.timestamp, 1_786_456_500, "still the same bar");
        assert_eq!(so_far.open, 10.0, "opened where the first one did");
        assert_eq!(so_far.high, 11.0, "the highest of them");
        assert_eq!(so_far.low, 9.0, "the lowest of them");
        assert_eq!(so_far.close, 9.4, "the latest of them");
        assert_eq!(so_far.volume, 200.0, "all of it");
        assert_eq!(so_far.count, 3);
        assert!((so_far.wap - (10.2 * 100.0 + 10.8 * 50.0 + 9.4 * 50.0) / 200.0).abs() < 1e-9);

        // And the next five minutes start a bar of their own.
        let next = forming.fold(&five(1_786_456_800, 9.4, 9.6, 9.3, 9.5, 10.0)).unwrap();
        assert_eq!(next.timestamp, 1_786_456_800);
        assert_eq!(next.volume, 10.0, "nothing carried over");
    }

    /// A week is folded from its Monday and a month from its first day, both
    /// at midnight UTC, as a gateway folds them. Counted from the epoch
    /// instead, a week opened on a Thursday and a month on whatever day thirty
    /// days from 1970 fell on.
    #[test]
    fn a_week_and_a_month_are_folded_on_the_calendar() {
        let week = crate::control::historical::BarSize::Week1.seconds();
        let month = crate::control::historical::BarSize::Month1.seconds();
        let mut weekly = FormingBar {
            req_id: 1, seconds: week, opened_at: 0, daily_session: None, closed_at: None, bar: Default::default(), weighted: 0.0, queued: Vec::new(),
        };
        // Sunday 20 September 2026 23:59:55, then Monday 21 September 00:00.
        let sunday = weekly.fold(&five(1_789_948_795, 10.0, 10.0, 10.0, 10.0, 5.0)).unwrap();
        assert_eq!(sunday.timestamp, 1_789_344_000, "the week that opened on Monday the 14th");
        let monday = weekly.fold(&five(1_789_948_800, 11.0, 11.0, 11.0, 11.0, 7.0)).unwrap();
        assert_eq!(monday.timestamp, 1_789_948_800, "a new week on Monday the 21st");
        assert_eq!((monday.open, monday.volume), (11.0, 7.0), "nothing carried over");

        let mut monthly = FormingBar {
            req_id: 2, seconds: month, opened_at: 0, daily_session: None, closed_at: None, bar: Default::default(), weighted: 0.0, queued: Vec::new(),
        };
        // Saturday 31 January 2026 23:59:55, then Sunday 1 February 00:00.
        let january = monthly.fold(&five(1_769_903_995, 10.0, 10.0, 10.0, 10.0, 5.0)).unwrap();
        assert_eq!(january.timestamp, 1_767_225_600, "January opened on the 1st");
        let february = monthly.fold(&five(1_769_904_000, 12.0, 12.0, 12.0, 12.0, 3.0)).unwrap();
        assert_eq!(february.timestamp, 1_769_904_000, "February opens on the 1st");
        // A leap day is in its own month.
        assert_eq!(opening(month, 1_709_208_000), 1_706_745_600, "29 February 2024 is February's");

        // A stamp in the epoch's first days, before any Monday: the week opens
        // at the epoch rather than counting back past it.
        assert_eq!(opening(week, 86_400), 0);
    }

    /// The trade count comes off the wire at the full width the field carries,
    /// so two bars in one interval need not add up to one.
    ///
    /// Added plain, two counts near the top of the range overflow — on the
    /// engine thread, where a panic ends the session and every subscription on
    /// it. The field states what it can hold, so a total past that is held at
    /// the top rather than wrapped to a bar made by minus two billion trades.
    #[test]
    fn a_forming_bar_holds_a_trade_count_the_wire_states_at_the_edge() {
        let mut forming = FormingBar {
            req_id: 1, seconds: 300, opened_at: 0,
            daily_session: None, closed_at: None,
            bar: Default::default(), weighted: 0.0, queued: Vec::new(),
        };
        let mut near_the_top = five(1_786_456_500, 10.0, 10.5, 9.5, 10.2, 100.0);
        near_the_top.count = i32::MAX - 1;
        forming.fold(&near_the_top).unwrap();

        let mut second = five(1_786_456_505, 10.2, 11.0, 10.1, 10.8, 50.0);
        second.count = i32::MAX - 1;
        let so_far = forming.fold(&second).unwrap();
        assert!(so_far.count > 0, "a bar is never made by a negative number of trades");
    }
}

/// A scan response is delivered to the scan that answered.
///
/// Every response arrives under one message id, so the id cannot identify the
/// scan. The payload names the scan, and that is what routes the rows.
#[test]
fn each_scan_gets_its_own_answer() {
    let mut hmds = super::HmdsState::new();
    hmds.pending_scanner.push(("APISCAN1:10001".to_string(), 10001));
    hmds.pending_scanner.push(("APISCAN2:10002".to_string(), 10002));
    hmds.pending_scanner.push(("APISCAN3:10003".to_string(), 10003));

    for (named, expected) in [
        ("APISCAN1:10001", 10001),
        ("APISCAN2:10002", 10002),
        ("APISCAN3:10003", 10003),
    ] {
        let xml = format!("<ScanResponse>\n\t<id>{named}</id>\n</ScanResponse>");
        assert_eq!(
            hmds.scanner_answered(&xml),
            Some(expected),
            "{named} answered, so its rows belong to the request that asked for it",
        );
    }
}

/// A response naming a scan this session is not running belongs to nobody here.
///
/// A withdrawn scan can answer once more. The scan name identifies the owner,
/// so a name matching no running scan has no owner and is not delivered.
#[test]
fn an_answer_naming_no_running_scan_is_not_handed_to_another() {
    let mut hmds = super::HmdsState::new();
    hmds.pending_scanner.push(("APISCAN1:10001".to_string(), 10001));
    assert_eq!(hmds.scanner_answered("<ScanResponse></ScanResponse>"), None);
    assert_eq!(hmds.scanner_answered("<ScanResponse><id>APISCAN9:99</id></ScanResponse>"), None);
    // The one it does name still answers.
    assert_eq!(
        hmds.scanner_answered("<ScanResponse><id>APISCAN1:10001</id></ScanResponse>"),
        Some(10001),
    );
}

mod hmds_correlation_tests {
    use super::super::*;
    use crate::bridge::SharedState;
    use crate::protocol::connection::Connection;

    /// Query names are numbered, so one is a prefix of another as soon as the
    /// count reaches ten. Searching the payload for the name handed the answer
    /// for `tk_12` to whichever of `tk_1` and `tk_12` was waiting first, and
    /// the other was never answered.
    ///
    /// A news reply states what the query asked for after its name, separated
    /// from it, and is still that query's answer.
    #[test]
    fn a_query_name_that_prefixes_another_is_not_answered_by_it() {
        let answer_for = |id: &str| {
            format!("<ResultSetTick><id>{id}</id><eoq>true</eoq></ResultSetTick>")
        };

        assert!(answers(&answer_for("tk_1"), "tk_1"), "its own answer");
        assert!(
            !answers(&answer_for("tk_12"), "tk_1"),
            "tk_1 took the answer meant for tk_12",
        );
        assert!(answers(&answer_for("tk_12"), "tk_12"), "which tk_12 needs itself");
        assert!(!answers(&answer_for("tk_2"), "tk_1"), "a different query entirely");

        // The decorated form a news reply states.
        let news = "<NewsResponse><id>news_2-headlines;;NewsQuery;;0;;true;;0;;U</id></NewsResponse>";
        assert!(answers(news, "news_2"), "the reply names the query it answers");
        assert!(!answers(news, "news_2x"), "and not one whose name merely resembles it");
    }

    fn tick_msg(query_id: &str, done: bool) -> Vec<u8> {
        let xml = format!(
            "<ResultSetTick><id>{}</id><eoq>{}</eoq><tz>UTC</tz><Events>\
             <Tick><time>20260714-13:30:00</time><price>100.0</price><size>1</size></Tick>\
             </Events></ResultSetTick>",
            query_id, if done { "true" } else { "false" },
        );
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=W\x016118=");
        msg.extend_from_slice(xml.as_bytes());
        msg.push(0x01);
        msg
    }

    /// A tick query is answered in segments, each stating whether it is the
    /// last. The route is held until a segment states it is.
    #[test]
    fn a_segmented_tick_reply_keeps_its_route_until_the_venue_is_done() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        hmds.pending_ticks.push(("tk_1".to_string(), 31, "TRADES".to_string()));

        hmds.process_hmds_message(&tick_msg("tk_1", false), &mut conn, &shared, &None, &mut hb);
        assert_eq!(hmds.pending_ticks.len(), 1, "more is coming, so the route stays");

        hmds.process_hmds_message(&tick_msg("tk_1", true), &mut conn, &shared, &None, &mut hb);
        assert!(hmds.pending_ticks.is_empty(), "the last segment releases it");
        assert_eq!(
            shared.reference.drain_historical_ticks().len(), 2,
            "both segments reached the caller",
        );
    }

    /// A segment cut mid-row ends the request rather than waiting on it.
    ///
    /// The parse refuses a row it never saw closed rather than handing back
    /// what is in hand, so a cut segment carries nothing to deliver — and
    /// nothing more is coming under that number, because the venue has
    /// answered it. Waited on, the series either completed later with a hole
    /// in it under the reply's own statement that it was whole, or, where the
    /// cut segment was the last, the request stood until the connection died:
    /// the sweep that fails an unreadable reply never sees a tick payload.
    #[test]
    fn a_tick_segment_cut_mid_row_ends_the_request() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        hmds.pending_ticks.push(("tk_9".to_string(), 31, "TRADES".to_string()));

        // The last segment, cut inside a row: the opening tag is there and
        // the close never arrives.
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=W\x016118=");
        msg.extend_from_slice(
            b"<ResultSetTick><id>tk_9</id><eoq>true</eoq><tz>UTC</tz><Events>\
              <Tick><time>20260714-13:30:00</time><price>100.0</price>",
        );
        msg.push(0x01);
        hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

        assert!(
            hmds.pending_ticks.is_empty(),
            "the request waits on a reply the venue has already sent",
        );
        let errors = shared.reference.drain_historical_errors();
        assert!(errors.iter().any(|(r, ..)| *r == 31), "the caller is told: {errors:?}");
        let delivered = shared.reference.drain_historical_ticks();
        assert!(
            delivered.iter().any(|(r, _, _, done)| *r == 31 && *done),
            "and released, or a caller reading these off the callback waits on \
             a last segment that is not coming: {delivered:?}",
        );
    }

    /// Tag 96 carries gzip bytes. The parsed field map is UTF-8 lossy, which
    /// replaces every invalid byte, so the payload is read from the raw
    /// frame.
    #[test]
    fn a_compressed_fundamental_report_survives_being_read() {
        use flate2::write::GzEncoder;
        use std::io::Write;

        let report = "<ReportSnapshot><Issuer>ACME</Issuer></ReportSnapshot>";
        let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(report.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(
            String::from_utf8(compressed.clone()).is_err(),
            "the payload really is not text",
        );

        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        let named = crate::control::fundamental::fundamentals_query_id(1);
        hmds.pending_fundamental.push((named.clone(), 51));

        // Tag 95 states the length, which frames a payload containing SOH
        // bytes. The answer names the request that asked, which is how it is
        // matched.
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=U\x016040=10012\x016118=");
        msg.extend_from_slice(
            format!("<FundamentalsResponse><id>{named}</id></FundamentalsResponse>").as_bytes(),
        );
        msg.extend_from_slice(b"\x0195=");
        msg.extend_from_slice(compressed.len().to_string().as_bytes());
        msg.extend_from_slice(b"\x0196=");
        msg.extend_from_slice(&compressed);
        msg.push(0x01);
        hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

        let answered = shared.reference.drain_fundamental_data();
        assert_eq!(answered.len(), 1, "the report reached the caller");
        assert_eq!(answered[0].1, report, "and it is the report the venue sent");
    }

    /// An article response consumes the pending request whether or not its
    /// payload reads, so an unreadable one is reported to the caller.
    #[test]
    fn an_unreadable_article_is_reported_rather_than_swallowed() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        hmds.pending_articles.push(("art_1".to_string(), 61));

        let xml = "<NewsResponse><id>art_1-article_file;;NewsQuery;;0;;true;;0;;U</id></NewsResponse>";
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=U\x016040=10032\x016118=");
        msg.extend_from_slice(xml.as_bytes());
        msg.push(0x01);
        hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

        assert!(hmds.pending_articles.is_empty(), "the request is spent either way");
        assert!(
            shared.reference.drain_news_articles().is_empty(),
            "there was no article to deliver",
        );
        assert!(
            shared.reference.drain_historical_errors().iter().any(|(id, _, _)| *id == 61),
            "and the caller is told, rather than left waiting",
        );
    }

    /// A news query is registered as outstanding only if it went out.
    /// Registered regardless, the caller waited its whole deadline for an
    /// answer to a request the socket never carried.
    #[test]
    fn a_news_query_that_did_not_go_out_is_refused_not_registered() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        hmds.send_historical_news_request(7, 265598, "BRFG", "", "", 10, &shared, &mut conn, &mut hb);
        hmds.send_news_article_request(8, "BRFG", "BRFG$1", &shared, &mut conn, &mut hb);
        assert!(hmds.pending_news.is_empty() && hmds.pending_articles.is_empty(), "nothing waits on a request that never left");
        let told = shared.reference.drain_historical_errors();
        let codes: Vec<_> = told.iter().map(|(id, code, _)| (*id, *code)).collect();
        assert_eq!(codes, [(7, 504), (8, 504)], "{told:?}");
    }

    /// A command can reach the engine without passing through a request
    /// surface. Its unreadable bounds must not become an unbounded query.
    #[test]
    fn an_unreadable_news_window_is_refused_before_sending() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        for (start, end) in [("unreadable", ""), ("", "unreadable")] {
            hmds.send_historical_news_request(7, 265598, "BRFG", start, end, 10, &shared, &mut conn, &mut hb);
            assert!(hmds.pending_news.is_empty());
            let told = shared.reference.drain_historical_errors();
            assert_eq!(told.len(), 1, "{told:?}");
            assert_eq!((told[0].0, told[0].1), (7, crate::error_codes::Refusal::VALIDATION));
            assert!(told[0].2.contains("YYYYMMDD-HH:MM:SS"), "{told:?}");
            assert!(told[0].2.contains("YYYYMMDD HH:MM:SS"), "{told:?}");
        }
    }

    /// A news reply naming nothing pending is noted, not dropped in silence:
    /// a reply after the lists were cleared, a duplicate, or an id this
    /// client cannot read all looked like nothing arriving.
    #[test]
    fn a_news_reply_naming_nothing_pending_is_noted() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        let xml = "<NewsResponse><id>news_9-history;;NewsQuery;;0;;true;;0;;U</id></NewsResponse>";
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=U\x016040=10032\x016118=");
        msg.extend_from_slice(xml.as_bytes());
        msg.push(0x01);
        hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);
        assert!(
            shared.market.unread_wire().iter().any(|(c, what)| *c == "historical" && what.contains("news reply")),
            "{:?}", shared.market.unread_wire(),
        );
    }

    /// A histogram reply that names a query and cannot be read fails that
    /// query rather than leaving it waiting on an answer that has already
    /// arrived.
    #[test]
    fn an_unreadable_histogram_reply_fails_the_query_it_names() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        let qid = crate::control::histogram::histogram_query_id(
            &crate::control::histogram::HistogramRequest {
                query_id: "hg_1".to_string(),
                con_id: 265598,
                sec_type: "STK".to_string(),
                exchange: "SMART".to_string(),
                use_rth: true,
                period: "1 week".to_string(),
                end_time: "20260320-21:00:00".to_string(),
            },
        );
        hmds.pending_histogram.push((qid.clone(), 71));

        let xml = format!(
            "<ResultSetHistogram><id>{qid}</id><Events>\
             <Tick><price>not-a-number</price><size>1500</size></Tick>\
             </Events></ResultSetHistogram>",
        );
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=W\x016118=");
        msg.extend_from_slice(xml.as_bytes());
        msg.push(0x01);
        hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

        assert!(hmds.pending_histogram.is_empty(), "the request is spent either way");
        assert!(
            shared.reference.drain_histogram_data().is_empty(),
            "there were no entries to deliver",
        );
        assert!(
            shared.reference.drain_historical_errors_for_dispatch(|_| false).iter().any(|(id, _, _)| *id == 71),
            "and the caller is told, rather than left waiting",
        );
    }

    /// A news response names the query it answers, and is matched on that
    /// name. Two searches can be in flight at once.
    #[test]
    fn a_news_reply_answers_the_request_it_names() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        hmds.pending_news.push(("news_1".to_string(), 51));
        hmds.pending_news.push(("news_2".to_string(), 52));

        // The second request's response arrives first, under the id its own
        // query went out with.
        let xml = "<NewsResponse><id>news_2-headlines;;NewsQuery;;0;;true;;0;;U</id></NewsResponse>";
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=U\x016040=10032\x016118=");
        msg.extend_from_slice(xml.as_bytes());
        msg.push(0x01);
        hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

        let answered = shared.reference.drain_historical_news();
        assert_eq!(answered.len(), 1, "one answer reached a caller");
        assert_eq!(answered[0].0, 52, "and it is the caller the reply names");
        assert_eq!(
            hmds.pending_news.iter().map(|(q, _)| q.as_str()).collect::<Vec<_>>(),
            vec!["news_1"],
            "the other request is still outstanding",
        );
    }

    /// A news or a fundamentals query its answer has ended is over, as a
    /// gateway holds it over: withdrawn after the answer, nothing goes to the
    /// venue. A fundamentals withdrawal naming nothing says nothing, as a
    /// gateway's says nothing; a news withdrawal, which is this client's own,
    /// says that nothing is waiting.
    #[test]
    fn a_query_its_answer_ended_is_not_withdrawn_again() {
        use std::io::Read;
        let fundamentals = crate::control::fundamental::fundamentals_query_id(1);
        for news in [true, false] {
            let mut hmds = HmdsState::new();
            let shared = SharedState::new();
            let mut hb = HeartbeatState::new();
            let (conn, mut peer) = Connection::for_test();
            peer.set_read_timeout(Some(std::time::Duration::from_millis(50))).unwrap();
            let mut conn = Some(conn);
            let reply = if news {
                hmds.pending_news.push(("news_1".into(), 51));
                "35=U\x016040=10032\x016118=<NewsResponse><id>news_1-headlines;;NewsQuery;;0;;true;;0;;U</id>\
                 </NewsResponse>\x01".to_string()
            } else {
                hmds.pending_fundamental.push((fundamentals.clone(), 51));
                format!("35=U\x016040=10012\x016118=<FundamentalsResponse><id>{fundamentals}</id>\
                         </FundamentalsResponse>\x01")
            };
            hmds.process_hmds_message(reply.as_bytes(), &mut conn, &shared, &None, &mut hb);
            if news {
                hmds.send_news_cancel(51, &mut conn, &mut hb, &shared);
            } else {
                hmds.send_fundamental_cancel(51, &mut conn, &mut hb);
            }
            let mut sent = [0u8; 4096];
            assert!(!matches!(peer.read(&mut sent), Ok(1..)), "news {news}: nothing goes to the venue");
            let told: Vec<i32> = shared.reference.drain_historical_errors().iter().map(|e| e.1).collect();
            assert_eq!(told, if news { vec![NO_SUCH_SUBSCRIPTION] } else { vec![] }, "news {news}");
        }
    }

    /// Two callers asking the same question of the same contract are two
    /// requests, and each is answered.
    ///
    /// The id a head timestamp went out under was built from what was being
    /// asked — the contract, the venue, the series, the hours — and nothing
    /// else. Two callers describing the same request therefore sent the same
    /// id, and the reply was matched to whichever of them sat first in the
    /// list: one was handed the other's answer, a pair the venue answered once
    /// left the second waiting with nothing to release it, and a cancel from
    /// one stopped the other's query at the venue. The histogram beside it is
    /// led by its own name for this reason.
    #[test]
    fn two_head_timestamps_for_one_contract_are_told_apart() {
        let ask = |query_id: &str| {
            crate::control::historical::head_timestamp_query_id(
                &crate::control::historical::HeadTimestampRequest {
                    query_id: query_id.to_string(),
                    con_id: 265_598,
                    sec_type: "CS".into(),
                    exchange: "SMART".into(),
                    data_type: "Last",
                    use_rth: true,
                    include_expired: false,
                },
            )
        };
        assert_ne!(
            ask("tk_1000"), ask("tk_1001"),
            "the same question asked twice goes out under two names",
        );
        assert!(ask("tk_1000").starts_with("tk_1000;;"), "led by the query's own name");
    }

    /// A head timestamp goes out under an id its response names, and is
    /// matched on it. Two can be in flight at once.
    #[test]
    fn a_head_timestamp_answers_the_request_the_reply_names() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        let id_of = |con_id: u32| {
            crate::control::historical::head_timestamp_query_id(
                &crate::control::historical::HeadTimestampRequest {
                    query_id: format!("tk_{con_id}"),
                    con_id,
                    sec_type: "CS".into(),
                    exchange: "SMART".into(),
                    data_type: "Last",
                    use_rth: true,
                    include_expired: false,
                },
            )
        };
        hmds.pending_head_ts.push((id_of(1), 41, 1));
        hmds.pending_head_ts.push((id_of(2), 42, 1));

        let xml = format!(
            "<ResultSetHeadTimeStamp><id>{}</id><eoq>true</eoq>\
             <headTS>19930129-09:00:00</headTS><tz>US/Eastern</tz></ResultSetHeadTimeStamp>",
            id_of(2),
        );
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=W\x016118=");
        msg.extend_from_slice(xml.as_bytes());
        msg.push(0x01);
        hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

        let answered = shared.reference.drain_head_timestamps();
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0].0, 42, "the reply named the second request");
    }

    /// A response naming no pending query is not delivered to the oldest
    /// outstanding request.
    #[test]
    fn an_answer_naming_no_pending_query_is_not_handed_to_another() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        hmds.pending_head_ts.push(("hts_of_another_query".to_string(), 71, 1));
        hmds.pending_histogram.push(("hg_of_another_query".to_string(), 72));

        for xml in [
            "<ResultSetHeadTimeStamp><id>nobody</id><eoq>true</eoq>\
             <headTS>19930129-09:00:00</headTS><tz>US/Eastern</tz></ResultSetHeadTimeStamp>",
            "<ResultSetHistogram><id>nobody</id><eoq>true</eoq>\
             <Events><Tick><price>100.0</price><size>5</size></Tick></Events></ResultSetHistogram>",
        ] {
            let mut msg = Vec::new();
            msg.extend_from_slice(b"35=W\x016118=");
            msg.extend_from_slice(xml.as_bytes());
            msg.push(0x01);
            hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);
        }

        assert!(shared.reference.drain_head_timestamps().is_empty());
        assert!(shared.reference.drain_histogram_data().is_empty());
        assert_eq!(hmds.pending_head_ts.len(), 1, "and both are still waiting");
        assert_eq!(hmds.pending_histogram.len(), 1);
    }
/// An unreadable end-of-query page withdraws the five-second stream at the
/// venue, as a stated refusal of it does. Removed only locally, the bars kept
/// arriving under a number the caller was told had failed.
#[test]
fn an_unreadable_eoq_page_withdraws_the_stream_at_the_venue() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
    let mut conn = Some(conn);
    hmds.pending_historical.push(("hist_1".to_string(), 9));
    hmds.keep_up_to_date_reqs.insert(9);
    hmds.rtbar_subs.push(("hist_1".to_string(), 9, Some(4002), 0.01, 1.0));
    hmds.held.push(HeldSeries {
        req_id: 9, fold: Fold::None, bars: Vec::new(), timezone: String::new(), actions_query: None,
        along: Default::default(),
        actions: None, complete: false,
    });
    let xml = "<ResultSetBar><id>hist_1</id><eoq>true</eoq><tz>US/Eastern</tz>\
               <Events><Bar><time>not-a-time</time></Bar></Events></ResultSetBar>";
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

    assert!(!hmds.keep_up_to_date_reqs.contains(&9), "the request is failed on the unreadable page");
    let cancel = String::from_utf8_lossy(&super::read_frame(&mut peer)).into_owned();
    assert!(cancel.contains("ticker:4002"), "the stream is withdrawn at the venue: {cancel:?}");
}

/// The venue refusing the stream half of a request kept up to date tells the
/// program nothing and ends nothing, as a gateway keeps the refusal of its
/// five-second stream to itself: a history already in stays in, and one still
/// arriving is delivered and ended as it would have been, the pages held
/// before the refusal with it. The program was told
/// a 162 and, once the history was filed, its side let go of a request a
/// gateway still holds.
#[test]
fn a_refused_stream_half_is_told_nothing_and_leaves_the_request_kept() {
    for history_in in [true, false] {
        let mut hmds = HmdsState::new();
        let (client, _rx, shared) = crate::api::client::tests::test_client();
        let mut hb = HeartbeatState::new();
        let mut conn: Option<Connection> = None;
        shared.reference.push_historical_taken(crate::bridge::HistoricalTaken {
            req_id: 9, format_date: 1, end_date_time: String::new(), duration: "1 D".into(),
            bar_size: "1 min".into(), keep_up_to_date: true,
        });
        if history_in {
            shared.reference.push_historical_data(9, crate::control::historical::HistoricalResponse {
                query_id: String::new(), timezone: String::new(), is_complete: true, bars: Vec::new(),
            });
        } else {
            hmds.held.push(HeldSeries {
                req_id: 9, fold: Fold::None, bars: Vec::new(), timezone: String::new(),
                actions_query: None, actions: None, complete: false, along: Default::default(),
            });
        }
        client.process_msgs(&mut crate::api::wrapper::tests::RecordingWrapper::default());
        assert_eq!(client.core.historical_answered(9), history_in, "its history is in: {history_in}");
        hmds.pending_historical.push(("hist_4001".to_string(), 9));
        hmds.keep_up_to_date_reqs.insert(9);
        hmds.rtbar_subs.push(("rt_4002".to_string(), 9, None, 0.01, 1.0));
        hmds.forming_bars.push(FormingBar {
            req_id: 9, seconds: 60, opened_at: 0, daily_session: None, closed_at: None, bar: Default::default(), weighted: 0.0, queued: Vec::new(),
        });
        // A page of the history still arriving, held before the refusal.
        if !history_in {
            let page = "<ResultSetBar><id>hist_4001</id><eoq>false</eoq><tz>UTC</tz><Events>\
                        <Bar><time>20260714-13:29:00</time><open>100.0</open><close>100.5</close>\
                        <high>100.7</high><low>99.9</low><weightedAvg>100.2</weightedAvg>\
                        <volume>1000</volume><count>10</count></Bar></Events></ResultSetBar>";
            let mut msg = Vec::new();
            msg.extend_from_slice(b"35=W\x016118=");
            msg.extend_from_slice(page.as_bytes());
            msg.push(0x01);
            hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);
        }
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<QueryError>\n\t<id>rt_4002</id>\n\t<error>no</error>\n</QueryError>\n";
        let mut msg = Vec::new();
        msg.extend_from_slice(b"35=W\x016118=");
        msg.extend_from_slice(xml.as_bytes());
        msg.push(0x01);
        hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

        assert!(hmds.rtbar_subs.iter().all(|(_, rid, ..)| *rid != 9), "the stream is gone");
        assert!(hmds.rtbar_resub.iter().all(|r| r.req_id != 9), "and is not asked for again");
        assert!(hmds.keep_up_to_date_reqs.contains(&9), "the request is still kept: {history_in}");
        // The history's last page, where it is still to come.
        if !history_in {
            hmds.process_hmds_message(&super::make_bar_msg("hist_4001", true), &mut conn, &shared, &None, &mut hb);
        }
        let mut heard = crate::api::wrapper::tests::RecordingWrapper::default();
        client.process_msgs(&mut heard);
        assert!(!heard.events.iter().any(|e| e.starts_with("error:")), "nothing is told: {:?}", heard.events);
        assert_eq!(
            heard.events.iter().any(|e| e.starts_with("historical_data_end:9")), !history_in,
            "a history still arriving ends as it would have: {:?}", heard.events,
        );
        assert_eq!(
            heard.events.iter().filter(|e| e.starts_with("historical_data:9:")).count(),
            if history_in { 0 } else { 2 },
            "its pages, the one held before the refusal among them: {:?}", heard.events,
        );
        assert!(client.core.historical_answered(9), "and its caller's side holds it: {history_in}");
    }
}

/// A series that cannot be folded fails the whole request, so the stream half
/// goes with it — as it does where the venue states the refusal and where a
/// page cannot be read.
///
/// Left running, five-second bars kept arriving under a number the caller had
/// just been told had failed, the next reconnect asked for the stream again,
/// and the number answered nothing else for the rest of the session.
#[test]
fn a_series_that_cannot_be_folded_withdraws_the_stream_it_was_asked_for_alongside() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
    let mut conn = Some(conn);
    hmds.pending_historical.push(("hist_1".to_string(), 9));
    hmds.keep_up_to_date_reqs.insert(9);
    hmds.rtbar_subs.push(("hist_1".to_string(), 9, Some(4002), 0.01, 1.0));
    hmds.held.push(HeldSeries {
        req_id: 9, fold: Fold::Adjusted, bars: Vec::new(), timezone: String::new(),
        along: Default::default(),
        actions_query: None, complete: false,
        // An action the venue named and this client cannot classify. It may be
        // one that moves the scale, so the fold refuses rather than hand back
        // a raw price under an adjusted one's name.
        actions: Some(vec![crate::control::adjustments::Adjustment {
            kind: None,
            date: "20240610".into(),
            ..Default::default()
        }]),
    });
    let xml = "<ResultSetBar><id>hist_1</id><eoq>true</eoq><tz>US/Eastern</tz>\
               <Events><Bar><time>20240611  09:30:00</time><open>10</open>\
               <high>11</high><low>9</low><close>10.5</close><volume>100</volume>\
               </Bar></Events></ResultSetBar>";
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

    assert!(
        !shared.reference.drain_historical_errors().is_empty(),
        "the caller is told the series could not be folded",
    );
    assert!(!hmds.keep_up_to_date_reqs.contains(&9), "the number is freed");
    assert!(hmds.rtbar_subs.iter().all(|(_, rid, ..)| *rid != 9), "the stream is gone");
    let cancel = String::from_utf8_lossy(&super::read_frame(&mut peer)).into_owned();
    assert!(cancel.contains("ticker:4002"), "and withdrawn at the venue: {cancel:?}");
}

/// A request kept up to date is two queries, and the withdrawal takes both.
///
/// The batch and the five-second stream beside it are acknowledged separately,
/// so two records can stand under one request number. The withdrawal that runs
/// when the request fails took the one it found and left the other — and that
/// one belonged to no pending list and carried no flag, so nothing swept it.
/// The number read as busy for the rest of the session, and a caller asking
/// under it again was refused for a stream that was not running. The
/// withdrawal a caller asks for itself already takes them all.
#[test]
fn withdrawing_a_kept_up_to_date_request_leaves_no_record_under_its_number() {
    let mut hmds = HmdsState::new();
    let mut hb = HeartbeatState::new();
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
    let mut conn = Some(conn);
    hmds.keep_up_to_date_reqs.insert(9);
    // Both halves, as a live request holds them: the stream, and the batch the
    // venue also acknowledged.
    hmds.rtbar_subs.push(("rt_9".to_string(), 9, Some(4002), 0.01, 1.0));
    hmds.rtbar_subs.push(("hist_9".to_string(), 9, Some(4003), 0.01, 1.0));

    hmds.withdraw_the_stream_half(9, &mut conn, &mut hb);

    assert!(
        hmds.rtbar_subs.iter().all(|(_, rid, ..)| *rid != 9),
        "no record stands under the number: {:?}",
        hmds.rtbar_subs.iter().map(|(q, r, ..)| (q.clone(), *r)).collect::<Vec<_>>(),
    );
    let cancel = String::from_utf8_lossy(&super::read_frame(&mut peer)).into_owned();
    assert!(cancel.contains("ticker:4002"), "and the stream is withdrawn: {cancel:?}");
}

/// A withdrawal waiting for its number does not stop a stream another caller
/// is still reading.
///
/// Two callers on one contract and kind are served under one number. The
/// withdrawal that runs while the number is known guards against that and says
/// why. The one that had to wait for the number — because the caller left
/// before the venue answered — sent the cancel whoever else held it, and that
/// caller was left subscribed in every table, silent for the rest of the
/// session, and told nothing.
#[test]
fn a_withdrawal_waiting_for_its_number_leaves_a_shared_stream_running() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_millis(300))).unwrap();
    let mut conn = Some(conn);

    // A second caller already reading the number the venue is about to state.
    hmds.tbt_subscriptions.push(TbtSubscription {
        ignore_size: false, instrument: 0, query_id: "tbt_keep".to_string(),
        kind: TbtType::Last, caller_req_id: 7, venue_id: 55,
        min_tick: 0, size_tick: 0.0, running: Default::default(),
    });
    // And one that left before its own acknowledgement arrived.
    hmds.tbt_withdrawn_unnumbered.insert("tbt_gone".to_string(), (0, TbtType::Last));

    let ack = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ResultSetTickerId>\
        <id>tbt_gone</id><rtTickerId>55</rtTickerId><minTick>0.01</minTick>\
        <sizeMinTick>1</sizeMinTick><eoq>false</eoq></ResultSetTickerId>";
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(ack.as_bytes());
    msg.push(0x01);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

    assert!(
        !hmds.tbt_withdrawn.contains_key(&55),
        "the number is not marked withdrawn while another caller reads it",
    );
    assert!(
        super::read_frame(&mut peer).is_empty(),
        "and no cancel goes out for it",
    );
}
}

mod hmds_transport_tests {
    use super::super::*;
    use crate::bridge::SharedState;
    use crate::protocol::connection::Connection;

    /// One-shot requests are failed when the connection is lost. Only
    /// historical bars carry a timeout, so the rest would never complete.
    #[test]
    fn a_lost_connection_answers_the_requests_it_took_with_it() {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        let mut conn: Option<Connection> = None;
        hmds.pending_head_ts.push(("hts".to_string(), 61, 1));
        hmds.pending_fundamental.push(("fund".to_string(), 62));
        hmds.pending_ticks.push(("tk".to_string(), 63, "TRADES".to_string()));

        hmds.disconnect(&mut conn, &shared, &None);

        let errors = shared.reference.drain_historical_errors();
        for req_id in [61, 62, 63] {
            assert!(
                errors.iter().any(|(rid, ..)| *rid == req_id),
                "request {req_id} was told the connection went: {errors:?}",
            );
        }
        assert!(hmds.pending_head_ts.is_empty(), "and nothing is left waiting");
    }
}

/// A query id that is a prefix of another does not take its answer.
///
/// The bar replies are matched by the name the reply states, the same as every
/// other reply here. Matched by prefix alone, `hist_10001`'s bars go to whoever
/// is waiting on `hist_1000` — and a session that keeps one request resident
/// while thousands pass does reach five figures with the first still open.
#[test]
fn a_query_id_that_prefixes_another_does_not_take_its_bars() {
    use super::states;

    assert!(states("hist_1000", "hist_1000"), "its own answer");
    assert!(!states("hist_10001", "hist_1000"), "the longer id is another query");
    assert!(!states("hist_1000_x", "hist_1000"), "and so is one continued by a word");
    // A reply that states what it asked for after the name is still that
    // query's, which is why the separator is not simply any character.
    assert!(states("news_2-headlines;;x", "news_2"));
}

/// The query that opens a tick stream states the contract, the kind of stream
/// and where it came from, and nothing it was not given.
#[test]
fn a_tick_stream_states_the_contract_and_the_kind() {
    let q = super::HmdsState::build_tbt_query(7, 265598, "BEST", "CS", "AllLast", 0, false);
    assert!(q.contains("<id>tbt_7</id>"));
    assert!(q.contains("<contractID>265598</contractID>"));
    assert!(q.contains("<data>AllLast</data>"));
    assert!(q.contains("<source>API</source>"));
    // And nothing the caller did not ask for: no prelude and no filter where
    // it asked for neither.
    assert!(!q.contains("timeLength"), "{q}");
    assert!(!q.contains("filter"), "{q}");
}

/// A prelude and a size filter are stated where the caller asked for them.
///
/// The query carries a length for a run of past ticks before the stream, and a
/// filter for leaving out a change that moves only the size. Both were refused
/// at the surface instead, so a caller could ask for neither — and the refusal
/// said this protocol had no field for a prelude, which this query does have.
#[test]
fn a_tick_stream_states_the_prelude_and_the_filter_it_was_asked_for() {
    let asked = super::HmdsState::build_tbt_query(7, 265598, "BEST", "CS", "AllLast", 100, true);
    assert!(asked.contains("<timeLength>100 t</timeLength>"), "{asked}");
    assert!(asked.contains("<filter><ignoreSize>true</ignoreSize></filter>"), "{asked}");
}

/// A standalone request for a contract's actions shares the caller's number
/// with whatever bar request is being folded under it. The answer is matched
/// to the hold on the query the hold itself sent: matched on the number
/// alone, the standalone answer folded an unrelated series with another
/// query's actions — a different range, or a different contract entirely.
#[test]
fn a_standalone_actions_reply_is_not_folded_into_another_request_s_series() {
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(conn);
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();

    // The fold asks for its actions under its own id.
    hmds.send_historical_request_ex(
        7, 756733, "", "1 M", "1 day", "ADJUSTED_LAST", true, false, false,
        "NVDA", "STK", "SMART", &mut conn, &mut hb, &shared,
    );
    let _ = read_frame(&mut peer);
    let qid = hmds.pending_adjustments.iter().find(|(_, rid, _)| *rid == 7)
        .map(|(q, _, _)| q.clone()).expect("the actions query is outstanding");

    // A standalone question about another contract, under this same caller
    // number, answers first.
    hmds.pending_adjustments.push(("adj_9999".to_string(), 7, 999999));
    hmds.process_hmds_message(
        &conadj_msg("adj_9999", 999999, "SS\n20240610,10"), &mut conn, &shared, &None, &mut hb,
    );

    assert_eq!(hmds.held.len(), 1, "the series is still waiting on its own actions");
    assert!(hmds.held[0].actions.is_none(), "and holds no answer from another query");
    assert!(hmds.pending_historical.is_empty(), "nor are its bars asked on another query's answer");
    assert_eq!(hmds.pending_adjustments.len(), 1, "the answered question is spent");
    assert_eq!(hmds.pending_adjustments[0].0, qid, "and the series' own still waits");
}

/// A refusal of a standalone actions query is not a refusal of the bar
/// request folded under the same caller number. Refusals are matched the way
/// answers are, on the query the hold itself sent.
#[test]
fn a_refusal_of_a_standalone_actions_query_leaves_the_fold_alone() {
    let (conn, mut peer) = Connection::for_test();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(conn);
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();

    hmds.send_historical_request_ex(
        7, 756733, "", "1 M", "1 day", "ADJUSTED_LAST", true, false, false,
        "NVDA", "STK", "SMART", &mut conn, &mut hb, &shared,
    );
    let _ = read_frame(&mut peer);

    // The venue refuses the standalone question, not the fold's.
    hmds.pending_adjustments.push(("adj_9999".to_string(), 7, 999999));
    hmds.process_hmds_message(
        &make_query_error_msg("adj_9999", "no permission"), &mut conn, &shared, &None, &mut hb,
    );

    assert_eq!(hmds.held.len(), 1, "the fold's series was not what was refused");
    assert!(hmds.held[0].actions.is_none(), "and it still waits on its own actions");
    assert!(
        shared.reference.drain_historical_data().is_empty(),
        "no end for a series the venue did not refuse",
    );
    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 1, "the refused question is still reported");
    assert_eq!(errors[0].0, 7);
}

/// A standalone actions query the venue refuses, or that goes with its
/// connection, lets go of the slot its caller's request holds for the answer.
/// Nothing will fill it, and left standing it was held for the rest of the
/// session unless the caller withdrew a query the venue no longer had.
#[test]
fn a_standalone_actions_query_the_engine_gives_up_holds_nothing() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let sock = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let _peer = listener.accept().unwrap();
    let mut conn = Some(Connection::new_raw(sock).unwrap());

    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let held_for = |req_id| {
        let contract = crate::control::adjustments::AdjustedContract {
            con_id: "756733".into(), ..Default::default()
        };
        shared.reference.note_adjustments(contract, Vec::new(), req_id);
        shared.reference.take_adjustments_answering(req_id).is_some()
    };

    shared.reference.expect_adjustments(41);
    hmds.pending_adjustments.push(("adj_41".to_string(), 41, 756733));
    hmds.process_hmds_message(
        &make_query_error_msg("adj_41", "no permission"), &mut conn, &shared, &None, &mut hb,
    );
    assert!(!held_for(41), "a refused request holds nothing");

    shared.reference.expect_adjustments(42);
    hmds.pending_adjustments.push(("adj_42".to_string(), 42, 756733));
    hmds.fail_pending("the connection went", &shared);
    assert!(!held_for(42), "nor does one that went with its connection");
}

/// A series whose corporate actions could not be asked for is let go, not held.
///
/// The request is registered as outstanding only if it actually went out, so a
/// send that fails is on no path that later fails it. The caller was told the
/// actions could not be asked for and the series stayed held behind it —
/// waiting on an answer to a request that never left — until the connection
/// was torn down, when it was told a second time.
#[test]
fn a_series_whose_actions_could_not_be_asked_for_is_let_go() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    // No socket, so the send cannot go.
    let mut conn: Option<Connection> = None;

    hmds.held.push(HeldSeries {
        req_id: 21, fold: Fold::Adjusted, bars: Vec::new(), timezone: String::new(),
        along: Default::default(),
        actions_query: None, actions: None, complete: true,
    });

    hmds.send_adjustments_request(
        21, 756733, "STK", "SMART", "20240101", "20240201", &shared, &mut conn, &mut hb,
    );

    assert!(hmds.held.is_empty(), "nothing is waiting on a request that never left");
    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 1, "told once, here, rather than again at teardown");
    assert_eq!(errors[0].0, 21);
    assert!(shared.reference.drain_historical_data().is_empty(), "and no end follows it");
    assert_eq!(over(&shared), [21], "and the request is over");
}

/// A request kept up to date whose batch the venue refuses is a request that
/// failed whole: the five-second stream it rides under this same number is
/// withdrawn with it. Left running, bars keep arriving under a number the
/// caller was told had failed, and the next reconnect asks for the stream
/// again.
#[test]
fn a_refused_batch_takes_the_kept_up_to_date_stream_with_it() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let sock = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut conn = Some(Connection::new_raw(sock).unwrap());

    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();

    // One caller number, the two queries it rides, and the venue's number for
    // the stream already stated.
    hmds.pending_historical.push(("hist_3001".to_string(), 7));
    hmds.keep_up_to_date_reqs.insert(7);
    hmds.rtbar_subs.push(("rt_3002".to_string(), 7, Some(5), 0.01, 1.0));
    hmds.rtbar_resub.push(RtBarRequest {
        req_id: 7, con_id: 756733, sec_type: "STK".into(), exchange: "SMART".into(),
        what_to_show: "TRADES".into(), use_rth: true,
    });
    hmds.forming_bars.push(FormingBar {
        req_id: 7, seconds: 60, opened_at: 0, daily_session: None, closed_at: None, bar: Default::default(), weighted: 0.0, queued: Vec::new(),
    });

    hmds.process_hmds_message(
        &make_query_error_msg("hist_3001", "Invalid time length"),
        &mut conn, &shared, &None, &mut hb,
    );

    assert!(hmds.rtbar_subs.is_empty(), "the stream goes with the request that failed");
    assert!(hmds.rtbar_resub.is_empty(), "and no reconnect asks for it again");
    assert!(hmds.forming_bars.is_empty(), "nor does the bar it was folding stay behind");
    assert!(!hmds.keep_up_to_date_reqs.contains(&7));

    // The withdrawal goes out under the number the venue knows the stream by.
    let sent = String::from_utf8_lossy(&read_frame(&mut peer)).to_string();
    assert!(sent.contains("ticker:5"), "the stream is cancelled by the venue's number: {sent}");

    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, 7);
}

/// A request that failed because its connection went is failed in its stream
/// half as well: a reconnect asks again for the streams that are still wanted,
/// and one whose request the caller was told had failed is not among them.
/// Left on the reconnect list, the bars resume under a number already
/// answered, and answer whatever is next asked under it.
#[test]
fn a_disconnect_does_not_resurrect_a_failed_request_s_stream() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let market = crate::engine::market_state::MarketState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;

    // A request kept up to date, still waiting on its batch. Beside it, an
    // ordinary bar stream that is still wanted.
    hmds.pending_historical.push(("hist_4001".to_string(), 9));
    hmds.keep_up_to_date_reqs.insert(9);
    hmds.rtbar_subs.push(("rt_4002".to_string(), 9, None, 0.01, 1.0));
    hmds.rtbar_resub.push(RtBarRequest {
        req_id: 9, con_id: 756733, sec_type: "STK".into(), exchange: "SMART".into(),
        what_to_show: "TRADES".into(), use_rth: true,
    });
    hmds.forming_bars.push(FormingBar {
        req_id: 9, seconds: 60, opened_at: 0, daily_session: None, closed_at: None, bar: Default::default(), weighted: 0.0, queued: Vec::new(),
    });
    hmds.rtbar_subs.push(("rt_4003".to_string(), 10, None, 0.01, 1.0));
    hmds.rtbar_resub.push(RtBarRequest {
        req_id: 10, con_id: 265598, sec_type: "STK".into(), exchange: "SMART".into(),
        what_to_show: "TRADES".into(), use_rth: true,
    });

    hmds.disconnect(&mut conn, &shared, &None);

    assert!(
        shared.reference.drain_historical_errors().iter().any(|(rid, ..)| *rid == 9),
        "the caller was told the request failed",
    );
    assert!(
        hmds.rtbar_resub.iter().all(|r| r.req_id != 9),
        "and nothing asks for its stream again",
    );
    assert!(hmds.rtbar_subs.iter().all(|(_, rid, ..)| *rid != 9));
    assert!(hmds.forming_bars.iter().all(|f| f.req_id != 9));
    assert!(
        hmds.rtbar_resub.iter().any(|r| r.req_id == 10),
        "a stream that is still wanted survives the disconnect",
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let sock = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    hmds.reconnect(
        Connection::new_raw(sock).unwrap(),
        &mut conn, &market, &mut hb,
    );

    assert!(
        hmds.rtbar_subs.iter().all(|(_, rid, ..)| *rid != 9),
        "the failed request's stream was not asked for again",
    );
    assert!(
        hmds.rtbar_subs.iter().any(|(_, rid, ..)| *rid == 10),
        "and the wanted one was",
    );
}

/// A number already answering a historical query does not take a second one.
///
/// Both the pages a caller is handed and the series they are held in are
/// resolved by this number, and the held series is a list searched by first
/// match. A second request under a live one put two entries under one number:
/// the second request's pages extended the first request's series, two
/// contracts were sorted together and folded on the first contract's actions,
/// and the caller was handed one series with two contracts in it and two ends.
/// The second request's own series was never completed and nothing sweeps it.
///
/// A head timestamp or a histogram under the number is refused alike, as a
/// gateway refuses either under the number of a live bar request.
#[test]
fn a_number_already_answering_a_historical_query_is_not_given_another() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let (conn, _peer) = Connection::for_test();
    let mut conn = Some(conn);

    hmds.send_historical_request_ex(9, 756733, "", "2 D", "1 hour", "TRADES",
        true, false, false, "AAPL", "STK", "SMART", &mut conn, &mut hb, &shared);
    assert_eq!(hmds.held.len(), 1, "the first request is held for its pages");
    let _ = shared.reference.drain_historical_errors();

    hmds.send_historical_request_ex(9, 320227571, "", "2 D", "1 hour", "TRADES",
        true, false, false, "QQQ", "STK", "SMART", &mut conn, &mut hb, &shared);

    assert_eq!(hmds.held.len(), 1, "the second contract does not join the first's series");
    assert_eq!(
        hmds.held[0].along.asked.as_ref().map(|asked| asked.con_id), Some(756733),
        "and the series still holds only the contract that was asked for",
    );
    assert_eq!(
        hmds.pending_historical.iter().filter(|(_, rid)| *rid == 9).count()
            + hmds.pending_adjustments.iter().filter(|(_, rid, _)| *rid == 9).count(),
        1,
        "only one query is in flight under the number",
    );
    // A head timestamp and a histogram under it are refused alike, as a
    // gateway refuses them.
    hmds.send_head_timestamp_request(9, &crate::types::ContractRef {
        con_id: 756733, sec_type: "STK".into(), exchange: "SMART".into(), ..Default::default()
    }, "TRADES", true, false, 1, &mut conn, &mut hb, &shared);
    hmds.send_histogram_request(9, 756733, "STK", "SMART", true, "3 days", &mut conn, &mut hb, &shared);
    assert!(
        hmds.pending_head_ts.is_empty() && hmds.pending_histogram.is_empty(),
        "neither goes out under the number",
    );
    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 3, "the caller is told each time: {errors:?}");
    assert!(
        errors.iter().all(|(id, code, text)| {
            (*id, *code, text.as_str()) == (9, 386, "Duplicate ticker ID for API historical data query")
        }),
        "under the number that names it, in a gateway's words: {errors:?}",
    );
    // Nothing ends the request that is answering: ended, its caller's side
    // let go of what it keeps of it, and its bars went out dated as nobody
    // asked.
    assert!(
        shared.reference.drain_historical_data().is_empty(),
        "and the request that is answering is left to answer",
    );
    assert!(over(&shared).is_empty(), "and is not told it is over");
}

/// A number already running a scan does not take a second.
///
/// Both scans run at the venue and both resolve to that number, so the caller
/// asked for one scan and read two interleaved with nothing in the sequence
/// saying so -- each batch ends the way a single scan's does. The withdrawal
/// takes one entry, so the other stayed running and went on delivering rows
/// under a number the caller had withdrawn.
#[test]
fn a_number_already_running_a_scan_is_not_given_another() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let (conn, _peer) = Connection::for_test();
    let mut conn = Some(conn);

    hmds.send_scanner_subscribe(9, "STK", "STK.US.MAJOR", "TOP_PERC_GAIN", 50,
        Vec::new(), &mut conn, &mut hb, &shared);
    assert_eq!(hmds.pending_scanner.len(), 1, "the first scan is running");
    let first = hmds.pending_scanner[0].0.clone();

    hmds.send_scanner_subscribe(9, "STK", "STK.US.MAJOR", "TOP_PERC_LOSE", 50,
        Vec::new(), &mut conn, &mut hb, &shared);

    assert_eq!(hmds.pending_scanner.len(), 1, "the second scan does not join it");
    assert_eq!(hmds.pending_scanner[0].0, first, "and the running scan is the one asked for");
    let errors = shared.reference.drain_historical_errors();
    assert_eq!(errors.len(), 1, "the caller is told: {errors:?}");
    assert_eq!(errors[0].1, 385, "under the number that names it");
}

/// A page that cannot be read ends a request kept up to date whose history
/// has not arrived whole, as it ends any other.
///
/// The unreadable page was warned and the stream left to go on, which is
/// right once the history is in; before that there is no history to go on
/// from — nothing was told, later pages went on extending a series nobody could
/// be handed, and the number could never be used again.
#[test]
fn an_unreadable_page_ends_a_kept_up_to_date_request_still_assembling() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_historical.push(("q9".to_string(), 9));
    hmds.keep_up_to_date_reqs.insert(9);
    hmds.held.push(HeldSeries {
        req_id: 9, fold: Fold::None, bars: Vec::new(), timezone: String::new(), actions_query: None, actions: None, complete: false,
        along: Default::default(),
    });
    let xml = "<ResultSetBar><id>q9</id><eoq>true</eoq><tz>UTC</tz><Events><Bar><time>20260714-13:30:00</time><open>100.0</open></Bar></Events></ResultSetBar>";
    let mut msg = Vec::new();
    msg.extend_from_slice(b"35=W\x016118=");
    msg.extend_from_slice(xml.as_bytes());
    msg.push(0x01);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);
    assert!(hmds.pending_historical.iter().all(|(_, rid)| *rid != 9), "the number is released");
    assert!(hmds.held.iter().all(|a| a.req_id != 9), "and the series with it");
    assert!(!hmds.keep_up_to_date_reqs.contains(&9), "and the stream half");
    assert!(shared.reference.drain_historical_errors().iter().any(|e| e.0 == 9), "the caller is told");
    assert!(shared.reference.drain_historical_data().is_empty(), "and no end follows it");
}

/// A scan whose subscribe did not go out is refused, not recorded as running.
///
/// Recorded regardless and logged as sent, the caller waited on a scan the
/// venue never received, with no deadline to end the wait.
#[test]
fn a_scan_that_did_not_go_out_is_refused_and_not_recorded() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    hmds.send_scanner_subscribe(9, "STK", "STK.US.MAJOR", "TOP_PERC_GAIN", 50, Vec::new(), &mut None, &mut hb, &shared);
    assert!(hmds.pending_scanner.is_empty(), "nothing is running");
    let told = shared.reference.drain_historical_errors();
    assert!(told.iter().any(|e| e.0 == 9 && e.1 == crate::error_codes::Refusal::NOT_CONNECTED), "{told:?}");
}

/// A scan batch that names a contract id nobody can read is said to the
/// caller and recorded, rather than dropped in silence while the scan stays
/// subscribed.
#[test]
fn a_scan_batch_with_an_unreadable_row_is_said_not_dropped() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_scanner.push(("APISCAN1:9".to_string(), 9));
    let xml = "<ScanResponse><id>APISCAN1:9</id><scanTime>2026-09-06 13:00:00</scanTime><Contract><contractID>not-a-contract</contractID></Contract></ScanResponse>";
    let msg = fix::fix_build(&[(fix::TAG_MSG_TYPE, "U"), (6040, "10005"), (6118, xml)], 1);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);
    let told = shared.reference.drain_historical_errors();
    assert!(told.iter().any(|e| e.0 == 9 && e.1 == 162), "the caller is told: {told:?}");
    assert!(shared.market.unread_wire().iter().any(|(k, _)| *k == "scanner"), "and the batch is recorded as unread");
    assert!(hmds.scanner_batches.is_empty(), "and nothing half-read is handed on");
}

/// A refusal naming the continued spelling of a query reaches that query.
///
/// The venue does not always echo the bare name it was given — the reply to a
/// news query comes back under `<name>-headlines;;…` — and every path that
/// reads data from a reply matches on that. The refusal path matched the name
/// exactly, so a refusal spelled the same way reached nothing: the request was
/// never told why, its record stood until the connection went, and the caller
/// waited out its deadline instead of hearing the reason the venue gave.
#[test]
fn a_refusal_naming_a_continued_query_name_still_reaches_it() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_ticks.push(("tk_4".to_string(), 77, "TRADES".to_string()));

    hmds.process_hmds_message(
        &make_query_error_msg("tk_4-headlines;;more", "no permission"),
        &mut conn, &shared, &None, &mut hb,
    );

    assert!(
        hmds.pending_ticks.is_empty(),
        "the refusal reached nothing, so the request waits for a reply that has \
         already arrived",
    );
    let told = shared.reference.drain_historical_errors();
    assert!(told.iter().any(|(r, ..)| *r == 77), "and the caller is told why: {told:?}");
}

/// A refused stream is recognised however the venue spells the name back.
///
/// The venue does not always echo the bare name it was given, and every other
/// branch of this chain matches the way the paths that read data match. This
/// one asked for the two to be equal, so a refusal naming the continued
/// spelling matched nothing: the caller was never told why, the record stayed
/// until the connection went, and a stream that was never coming was waited on
/// for the rest of the session.
#[test]
fn a_refused_tick_stream_is_matched_the_way_every_other_refusal_is() {
    let shared = std::sync::Arc::new(crate::bridge::SharedState::new());
    let mut hmds = HmdsState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.tbt_subscriptions.push(TbtSubscription {
        ignore_size: false, instrument: 0, query_id: "tbt_7".to_string(),
        kind: TbtType::Last, caller_req_id: 41, venue_id: 0, min_tick: 0,
        size_tick: 0.0, running: Default::default(),
    });

    // The name it was given, continued — which is what a reply may state.
    let msg = make_query_error_msg("tbt_7.1", "no such stream");
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);

    assert!(
        hmds.tbt_subscriptions.is_empty(),
        "the refusal named this stream and was read as naming none",
    );
}

/// A stream's records are read in the shape the stream was asked for.
///
/// The same contract can carry trades, quotes and the point between them at
/// once, and a record read under the wrong shape is decoded field by field
/// into something nobody sent — a midpoint read as a trade states a size and
/// a venue the venue never wrote.
#[test]
fn a_stream_is_read_in_the_shape_it_was_asked_for() {
    use crate::protocol::tbt_stream::TbtKind;
    use crate::types::TbtType;
    assert_eq!(super::frame_kind(TbtType::MidPoint), TbtKind::MidPoint);
    assert_eq!(super::frame_kind(TbtType::BidAsk), TbtKind::BidAsk);
    assert_eq!(super::frame_kind(TbtType::Last), TbtKind::AllLast);
    assert_eq!(super::frame_kind(TbtType::AllLast), TbtKind::AllLast);
}

/// A scan batch that could not be read is a notice: the scan stays subscribed
/// and goes on, so what is said does not end the request.
#[test]
fn an_unreadable_scan_batch_does_not_end_the_scan() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    let mut hb = HeartbeatState::new();
    let mut conn: Option<Connection> = None;
    hmds.pending_scanner.push(("APISCAN1:9".to_string(), 9));
    let xml = "<ScanResponse><id>APISCAN1:9</id><scanTime>2026-09-06 13:00:00</scanTime><Contract><contractID>not-a-contract</contractID></Contract></ScanResponse>";
    let msg = fix::fix_build(&[(fix::TAG_MSG_TYPE, "U"), (6040, "10005"), (6118, xml)], 1);
    hmds.process_hmds_message(&msg, &mut conn, &shared, &None, &mut hb);
    let said: Vec<_> = shared
        .take_records(shared.next_seq(), crate::bridge::Take::Dispatch { bulletins: false })
        .into_iter()
        .filter_map(|(_, r)| match r {
            crate::bridge::Record::HistoricalError((origin, code, _)) => Some((origin, code)),
            _ => None,
        })
        .collect();
    assert_eq!(said, [(crate::types::model::ErrorOrigin::Request { id: 9, ends: false }, 162)]);
}

/// A bar kept up to date goes on from the one the history ended on, as a
/// gateway goes on from it: a five-second bar inside it is folded into it, so
/// the bar keeps the open, the high, the volume and the average the venue
/// stated for it. A day's is its session, kept across midnight UTC; a week is
/// dated rather than timed. Started from the first five-second bar instead,
/// the day's open and high were lost. A page arriving afterwards can describe
/// an earlier bar, and the bar goes on from the later one; a five-second bar
/// from before the bar in hand opened is passed over, as a gateway passes an
/// older update over. Five-second bars are sent as the stream sends them, one
/// at the history's last stamp too.
#[test]
fn a_kept_up_to_date_bar_goes_on_from_the_one_the_history_ends_on() {
    use crate::control::historical::BarSize;
    let at = |stamped: &str| crate::protocol::datetime::ib_datetime_to_unix(stamped).unwrap() as u32;
    let merged = |opened| (opened, 100.0, 100.7, 0.5, 1.5, 1030.0, (100.2 * 1000.0 + 30.0) / 1030.0, 13);
    // The bar's length, the history's last bar, the one before it, the
    // five-second bars that follow, and the bar they leave: its opening,
    // open, high, low, close, volume, average and count.
    for (size, last, before, inside, expected) in [
        (
            BarSize::Day1,
            "<time>20260923-22:00:00</time><endTime>20260924-21:00:00</endTime>",
            "<time>20260922-22:00:00</time><endTime>20260923-21:00:00</endTime>",
            vec!["20260923-22:00:00", "20260924-01:00:00", "20260924-20:59:55"],
            merged("20260923-22:00:00"),
        ),
        (
            BarSize::Hour1,
            "<time>20260714-13:30:00</time><endTime>20260714-14:00:00</endTime>",
            "<time>20260714-12:00:00</time><endTime>20260714-13:00:00</endTime>",
            vec![
                "20260714-12:59:55", "20260714-13:10:00", "20260714-13:45:00", "20260714-13:50:00",
                "20260714-13:59:55",
            ],
            merged("20260714-13:30:00"),
        ),
        (
            BarSize::Week1,
            "<date>20260309</date><endDate>20260314</endDate>",
            "<date>20260302</date><endDate>20260307</endDate>",
            vec!["20260310-14:00:00", "20260311-14:00:00", "20260313-19:59:55"],
            merged("20260309-00:00:00"),
        ),
        (
            BarSize::Sec5,
            "<time>20260714-13:30:00</time><endTime>20260714-13:30:05</endTime>",
            "<time>20260714-13:29:55</time><endTime>20260714-13:30:00</endTime>",
            vec!["20260714-13:30:00"],
            ("20260714-13:30:00", 1.0, 2.0, 0.5, 1.5, 10.0, 1.0, 1),
        ),
    ] {
        let mut hmds = HmdsState::new();
        let shared = SharedState::new();
        hmds.pending_historical.push(("kept".into(), 21));
        hmds.keep_up_to_date_reqs.insert(21);
        hmds.forming_bars.push(FormingBar {
            req_id: 21, seconds: size.seconds(), opened_at: 0, daily_session: None, closed_at: None,
            bar: Default::default(), weighted: 0.0, queued: Vec::new(),
        });
        let page = |bounds: &str| String::from_utf8(make_bar_msg("kept", true)).unwrap()
            .replace("<time>20260714-13:30:00</time>", bounds);
        for bounds in [last, before] {
            hmds.process_hmds_message(page(bounds).as_bytes(), &mut None, &shared, &None, &mut HeartbeatState::new());
        }
        let forming = &mut hmds.forming_bars[0];
        for stamped in inside {
            forming.fold(&crate::types::RealTimeBar {
                timestamp: at(stamped), open: 1.0, high: 2.0, low: 0.5, close: 1.5,
                volume: 10.0, wap: 1.0, count: 1,
            });
        }
        let bar = forming.bar;
        let (opened, open, high, low, close, volume, wap, count) = expected;
        assert_eq!(
            (bar.timestamp, bar.open, bar.high, bar.low, bar.close, bar.volume, bar.wap, bar.count),
            (at(opened), open, high, low, close, volume, wap, count),
            "{size:?}",
        );
    }
}

/// Give the engine behind a test client a connection to the data service, and
/// have its own loop take what the calls send from here on: the far end of
/// the connection, and where the calls' commands go to be taken.
fn on_the_data_service(
    rx: &crate::api::client::tests::Engine,
) -> (std::net::TcpStream, std::sync::mpsc::Sender<crate::types::ControlCommand>) {
    let (conn, peer) = Connection::for_test();
    let (into, taken) = std::sync::mpsc::channel();
    rx.engine().hmds_conn = Some(conn);
    rx.engine().set_control_rx(taken);
    (peer, into)
}

/// The engine's loop takes what the calls have sent so far.
fn taken(rx: &crate::api::client::tests::Engine, into: &std::sync::mpsc::Sender<crate::types::ControlCommand>) {
    rx.try_iter().for_each(|cmd| into.send(cmd).unwrap());
    rx.engine().poll_once();
}

/// The engine hears what the data service says.
fn the_service_says(rx: &crate::api::client::tests::Engine, shared: &SharedState, msg: &[u8]) {
    let mut held = rx.engine();
    let engine = &mut *held;
    engine.hmds.process_hmds_message(msg, &mut engine.hmds_conn, shared, &None, &mut engine.hb);
}

/// A head timestamp is written in the form its own request asked for,
/// whatever else is refused under its number, as a gateway keeps the form
/// with the request. Kept by number on the caller's side, a second request
/// under the number — here one naming a series there is none of, refused —
/// took the first one's form with it, and the first was answered in the
/// venue's spelling where it had asked for seconds since the epoch.
#[test]
fn a_head_timestamp_is_written_the_way_it_was_asked_for() {
    let (client, rx, shared) = crate::api::client::tests::test_client();
    let (_peer, into) = on_the_data_service(&rx);
    let spy = crate::api::client::tests::spy();
    client.req_head_time_stamp(11, &spy, "TRADES", true, 2);
    taken(&rx, &into);
    client.req_head_time_stamp(11, &spy, "NOSUCH", true, 1);
    taken(&rx, &into);
    let asked = rx.engine().hmds.pending_head_ts.iter().find(|(_, rid, _)| *rid == 11)
        .map(|(query, ..)| query.clone()).expect("the first is awaited");
    let answer = format!(
        "35=W\x016118=<ResultSetHeadTimeStamp><id>{asked}</id><eoq>true</eoq>\
         <headTS>20200101-00:00:00</headTS><tz>UTC</tz></ResultSetHeadTimeStamp>\x01",
    );
    the_service_says(&rx, &shared, answer.as_bytes());

    let mut w = crate::api::wrapper::tests::RecordingWrapper::default();
    client.process_msgs(&mut w);
    assert!(w.events.iter().any(|e| e.starts_with("error:11:321:")), "the second is refused: {:?}", w.events);
    assert!(
        w.events.iter().any(|e| e == "head_timestamp:11:1577836800"),
        "the first is answered in seconds since the epoch: {:?}",
        w.events,
    );
}

/// A day's bar kept up to date rolls over when the one the history stated
/// ends, as a gateway rolls it: the next opens from the five-second bar at
/// midnight UTC of its day and ends at the next midnight UTC, or at the close
/// the history stated where that falls between the two, and it is dated by
/// where it ends on the series' zone. A bar that has ended is opened again by
/// the next five-second bar.
///
/// - A future's session runs from 22:00 UTC to 21:00 the next day. A
///   five-second bar from before the history's last bar opened is passed
///   over, as a gateway passes it over. The first
///   five-second bar of the next session opens a bar at midnight UTC of its
///   own day, which ends at the history's close of 21:00 that day, before the
///   five-second bar: it is dated the day of that close, 24 September on the
///   exchange's clock. The next, after midnight UTC, opens a bar that runs to
///   the next midnight, dated the 25th, and the one after goes into it.
/// - A share asked for outside regular hours closes at midnight UTC. The next
///   session's five-second bars each open a bar at that midnight which ends
///   there, dated by it: 24 September, where the evening before closed.
///
/// The contract's own sessions are in hand, and move nothing.
///
/// And each bar reaches the caller on the callback its request is answered
/// on, whatever else the number has done:
///
/// - A five-second bar that arrives while the history is on its way is held,
///   as a gateway holds it, and goes into the history's last bar once the
///   history is in: nothing is heard of it before the history's end.
/// - A second request under the number, refused because the first is still
///   answering, leaves the first as it was: its bars are still updates, dated
///   as it asked for them.
/// - A live bar stream under the number a backfill then answers under is
///   heard as a stream before the backfill and after it.
#[test]
fn a_days_bar_kept_up_to_date_rolls_over_at_midnight_utc() {
    use crate::control::contracts::{ContractSchedule, ScheduleSession};
    // Every callback a bar can reach, with what it states.
    #[derive(Default)]
    struct Heard(Vec<String>);
    impl crate::api::Wrapper for Heard {
        fn historical_data_update(&mut self, _: i64, bar: &crate::types::model::BarData) {
            self.0.push(format!("update {} {} {}", bar.date, bar.open, bar.volume));
        }
        #[allow(clippy::too_many_arguments)]
        fn real_time_bar(
            &mut self, _: i64, time: i64, open: f64, _: f64, _: f64, _: f64, volume: f64, _: f64, _: i32,
        ) {
            self.0.push(format!("bar {time} {open} {volume}"));
        }
        fn error(&mut self, _: i64, _: i64, code: i64, _: &str, _: &str) {
            self.0.push(format!("error {code}"));
        }
    }
    // What happens, in order: a five-second bar's stamp, price in cents and
    // volume; the history arriving; the request made again under its number.
    enum Step { Five(&'static str, u32, u32), History, Again }
    use Step::{Again, Five, History};
    // Whether the request keeps its bars up to date, rather than filling in
    // behind a live bar stream under its number; the series' zone; where the
    // history's last bar opened and closed; what happens; what is heard.
    type Row = (bool, &'static str, &'static str, &'static str, &'static [Step], &'static [&'static str]);
    let rows: [Row; 5] = [
        (true, "US/Central", "20260923-22:00:00", "20260924-21:00:00", &[
            History,
            Five("20260923-21:59:55", 9_900, 1),
            Five("20260924-22:00:05", 10_000, 5), Five("20260925-00:00:05", 10_100, 7),
            Five("20260925-22:00:05", 10_200, 3),
        ], &["update 20260924 100 5", "update 20260925 101 7", "update 20260925 101 10"]),
        (true, "US/Eastern", "20260924-08:00:00", "20260925-00:00:00", &[
            History, Five("20260925-08:00:05", 10_000, 5), Five("20260925-08:00:10", 10_100, 7),
        ], &["update 20260924 100 5", "update 20260924 101 7"]),
        // The history still on its way.
        (true, "US/Central", "20260923-22:00:00", "20260924-21:00:00", &[
            Five("20260924-15:00:05", 10_100, 7), History, Five("20260924-15:00:10", 10_200, 3),
        ], &["update 20260924 100 1007", "update 20260924 100 1010"]),
        // Asked for again, in seconds since the epoch, while it answers.
        (true, "US/Central", "20260923-22:00:00", "20260924-21:00:00", &[
            History, Five("20260924-15:00:05", 10_100, 7), Again, Five("20260924-15:00:10", 10_200, 3),
        ], &["update 20260924 100 1007", "error 386", "update 20260924 100 1010"]),
        // A stream, and then a backfill under its number.
        (false, "US/Central", "20260923-22:00:00", "20260924-21:00:00", &[
            Five("20260924-15:00:05", 10_100, 7), History, Five("20260924-15:00:10", 10_200, 3),
        ], &["bar 1790262005 101 7", "bar 1790262010 102 3"]),
    ];
    for (row, (kept, zone, opened, closed, steps, heard)) in rows.into_iter().enumerate() {
        let (client, rx, shared) = crate::api::client::tests::test_client();
        let spy = crate::api::client::tests::spy();
        let (_peer, into) = on_the_data_service(&rx);
        if !kept {
            client.req_real_time_bars(21, &spy, 5, "TRADES", false);
        }
        client.req_historical_data(21, &spy, "", "1 W", "1 day", "TRADES", false, 1, kept);
        taken(&rx, &into);
        // The contract's actions, asked for first, state none; the bars are
        // asked for then, and the stream is numbered.
        let actions = rx.engine().hmds.pending_adjustments.iter().find(|(_, rid, _)| *rid == 21)
            .map(|(query, ..)| query.clone()).expect("the actions are asked for first");
        the_service_says(&rx, &shared, &conadj_msg(&actions, 756733, ""));
        let asked = rx.engine().hmds.pending_historical.iter().find(|(_, rid)| *rid == 21)
            .map(|(query, _)| query.clone()).expect("and the bars once they are in");
        rx.engine().hmds.rtbar_subs.iter_mut().find(|(_, rid, ..)| *rid == 21)
            .expect("the stream is asked for").2 = Some(4002);
        shared.reference.note_schedule_key(756733, "p4002");
        let session = |start: &str, end: &str| ScheduleSession {
            start: start.into(), end: end.into(), trade_date: end[..8].into(),
        };
        shared.reference.set_contract_schedule("p4002", ContractSchedule {
            timezone: zone.into(),
            trading_hours: vec![
                session("20260924-22:00:00", "20260925-21:00:00"),
                session("20260925-08:00:00", "20260926-00:00:00"),
            ],
            liquid_hours: Vec::new(),
        });
        let history = String::from_utf8(make_bar_msg(&asked, true)).unwrap()
            .replace("20260714-13:30:00</time>", &format!("{opened}</time><endTime>{closed}</endTime>"))
            .replace("<tz>UTC</tz>", &format!("<tz>{zone}</tz>"));
        let five = |at: &str, cents: u32, volume: u32| {
            let at = crate::protocol::datetime::ib_datetime_to_unix(at).unwrap() as u32;
            let payload = crate::control::historical::tests::single_tick_payload(cents, volume);
            let mut msg = b"8=O\x019=0\x0135=G\x01".to_vec();
            msg.extend_from_slice(&[0, 0]);
            msg.extend_from_slice(&4002u32.to_be_bytes());
            msg.extend_from_slice(&at.to_be_bytes());
            msg.push(payload.len() as u8);
            msg.extend_from_slice(&payload);
            msg.extend_from_slice(b"\x018349=AABBCCDD\x01");
            msg
        };
        let mut got = Heard::default();
        for step in steps {
            match step {
                History => the_service_says(&rx, &shared, history.as_bytes()),
                Five(at, cents, volume) => the_service_says(&rx, &shared, &five(at, *cents, *volume)),
                Again => {
                    client.req_historical_data(21, &spy, "", "1 W", "1 day", "TRADES", false, 2, true);
                    taken(&rx, &into);
                }
            }
            client.process_msgs(&mut got);
        }
        assert_eq!(got.0, heard, "row {row}");
    }
}

/// Every bar in a frame reaches the requests it belongs to.
///
/// The venue puts one record for each stream with a bar to state in a frame.
/// This frame, as captured with an all-hours and a regular-hours stream open on
/// one future, states the same five-second bar for both: the regular-hours
/// stream first, then the all-hours one. Only the first record was read, so
/// the all-hours request heard one bar in forty seconds while the other heard
/// eight.
#[test]
fn every_bar_in_a_frame_reaches_the_request_it_belongs_to() {
    let mut hmds = HmdsState::new();
    let shared = SharedState::new();
    hmds.rtbar_subs.push(("rt_1001".into(), 40, Some(1), 0.25, 1.0));
    hmds.rtbar_subs.push(("rt_1003".into(), 41, Some(2), 0.25, 1.0));
    let frame = b"8=O\x019=0064\x0135=G\x01\x01\x50\
        \x00\x00\x00\x02\x6a\xb6\xa6\x51\x0c\x0f\x3a\xa9\x1d\xb0\x14\x90\x00\x00\xe1\x80\x3f\
        \x00\x00\x00\x01\x6a\xb6\xa6\x51\x0c\x0f\x3a\xa9\x1d\xb0\x14\x90\x00\x00\xe1\x80\x3f\
        \x018349=5F23A01E\x01";
    hmds.process_hmds_message(frame, &mut None, &shared, &None, &mut HeartbeatState::new());
    let heard: Vec<(u32, u32)> = shared.market.drain_real_time_bars().iter()
        .map(|(req_id, bar)| (*req_id, bar.timestamp)).collect();
    // 20260925-16:50:25 UTC, the time both records state.
    assert_eq!(heard, [(41, 1_790_355_025), (40, 1_790_355_025)]);

    // Each record states its own time and bar, and each is read for its own.
    let records: Vec<u8> = [(2u32, 1_790_355_025u32, 10_000, 5), (1, 1_790_355_030, 10_100, 7)]
        .into_iter()
        .flat_map(|(stream, at, ticks, volume)| {
            let payload = crate::control::historical::tests::single_tick_payload(ticks, volume);
            [&stream.to_be_bytes()[..], &at.to_be_bytes(), &[payload.len() as u8], &payload].concat()
        })
        .collect();
    let mut frame = b"8=O\x019=0\x0135=G\x01".to_vec();
    frame.extend_from_slice(&(records.len() as u16 * 8).to_be_bytes());
    frame.extend_from_slice(&records);
    frame.extend_from_slice(b"\x018349=AABBCCDD\x01");
    hmds.process_hmds_message(&frame, &mut None, &shared, &None, &mut HeartbeatState::new());
    let heard: Vec<(u32, u32, f64, f64)> = shared.market.drain_real_time_bars().iter()
        .map(|(req_id, bar)| (*req_id, bar.timestamp, bar.close, bar.volume)).collect();
    assert_eq!(heard, [(41, 1_790_355_025, 2_500.0, 5.0), (40, 1_790_355_030, 2_525.0, 7.0)]);
}
