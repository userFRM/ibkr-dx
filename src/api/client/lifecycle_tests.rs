use super::*;
use crate::types::model::ErrorOrigin;
use std::io::Read;
use std::time::Duration;

#[test]
fn closing_admission_stops_calls_before_the_join_returns() {
    let shared = Arc::new(SharedState::new());
    let (release, released) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || released.recv().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    let client = Arc::new(EClient::from_parts(Arc::clone(&shared), tx, worker, "DU1".into()));
    let closing = {
        let client = Arc::clone(&client);
        thread::spawn(move || client.disconnect())
    };
    assert!(matches!(rx.recv_timeout(Duration::from_secs(1)), Ok(ControlCommand::Logout)));
    assert!(!closing.is_finished());
    assert!(client.session_over());
    assert!(!client.is_connected());
    client.req_current_time();
    client.refuse(ErrorOrigin::Session, 321, "too late");
    client.req_ping();
    assert!(shared.drain_refused().is_empty());
    assert!(
        shared
            .take_records(shared.next_seq(), crate::bridge::Take::Whole { bulletins: true })
            .is_empty()
    );
    assert!(matches!(rx.try_recv(), Ok(ControlCommand::Shutdown)));
    assert!(rx.try_recv().is_err());
    release.send(()).unwrap();
    assert_eq!(closing.join().unwrap(), Shutdown { logout_sent: false, engine: EngineEnd::Ended });
}

#[test]
fn a_shutdown_reports_the_logout_written_and_the_same_end_each_time() {
    let pair = || {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        (crate::protocol::connection::Connection::new_raw(socket).unwrap(), peer)
    };
    let shared = Arc::new(SharedState::new());
    let (farm, _farm_peer) = pair();
    let (trading, mut venue) = pair();
    let (engine, tx) = crate::engine::hot_loop::HotLoop::with_connections(
        Arc::clone(&shared),
        None,
        "DU1".into(),
        farm,
        trading,
        None,
        None,
    );
    let client = EClient::from_parts(
        shared,
        tx,
        thread::spawn(move || engine.run_with_panic_recovery()),
        "DU1".into(),
    );
    let expected = Shutdown { logout_sent: true, engine: EngineEnd::Ended };
    assert_eq!(client.disconnect(), expected);
    assert_eq!(client.disconnect(), expected);
    venue.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut messages = String::new();
    venue.read_to_string(&mut messages).unwrap();
    assert!(messages.contains("35=5"));
}

#[test]
fn a_panicked_thread_is_ended_even_when_its_payload_cannot_be_dropped() {
    struct Payload;
    impl Drop for Payload {
        fn drop(&mut self) {
            panic!("the payload cannot be dropped");
        }
    }
    let (tx, _rx) = std::sync::mpsc::channel();
    let client = EClient::from_parts(
        Arc::new(SharedState::new()),
        tx,
        thread::spawn(|| std::panic::panic_any(Payload)),
        "DU1".into(),
    );
    let expected = Shutdown { logout_sent: false, engine: EngineEnd::Panicked };
    assert_eq!(client.disconnect(), expected);
    assert_eq!(client.disconnect(), expected);
    drop(client);
}

#[test]
fn a_cancelled_replay_wait_stays_cancelled_after_the_replay_settles() {
    let (mut client, _rx, shared) = super::tests::test_client();
    shared.orders.set_replay_done();
    client.cancel = Some(Arc::new(AtomicBool::new(true)));
    let refusal = client.next_shared_id_within(None).unwrap_err();
    assert_eq!(refusal.code, Refusal::NO_ANSWER);
    assert!(refusal.message.contains("taken back"));
}

#[test]
fn a_prepared_engine_logs_out_if_its_thread_was_not_started() {
    let shared = Arc::new(SharedState::new());
    let (farm, _farm_peer) = crate::protocol::connection::Connection::for_test();
    let (trading, mut venue) = crate::protocol::connection::Connection::for_test();
    let (engine, _tx) = crate::engine::hot_loop::HotLoop::with_connections(
        Arc::clone(&shared),
        None,
        "DU1".into(),
        farm,
        trading,
        None,
        None,
    );
    drop(PreparedEngine(Some(engine)));
    assert!(shared.closed_pushed());
    assert!(shared.logout_sent.load(Ordering::Acquire));
    venue.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut messages = String::new();
    venue.read_to_string(&mut messages).unwrap();
    assert!(messages.contains("35=5"));
}

#[test]
fn a_connect_taken_back_at_handoff_waits_for_the_engine_to_end() {
    let shared = Arc::new(SharedState::new());
    let (farm, _farm_peer) = crate::protocol::connection::Connection::for_test();
    let (trading, mut venue) = crate::protocol::connection::Connection::for_test();
    let (engine, tx) = crate::engine::hot_loop::HotLoop::with_connections(
        Arc::clone(&shared),
        None,
        "DU1".into(),
        farm,
        trading,
        None,
        None,
    );
    let (start, starting) = std::sync::mpsc::channel::<()>();
    let finished = Arc::new(AtomicBool::new(false));
    let ended = Arc::clone(&finished);
    let worker = thread::spawn(move || {
        let prepared = PreparedEngine(Some(engine));
        assert!(starting.recv().is_err(), "a cancelled login never starts the engine");
        drop(prepared);
        ended.store(true, Ordering::Release);
    });
    let mut client = EClient::from_parts(shared, tx, worker, "DU1".into());
    client.cancel = Some(Arc::new(AtomicBool::new(true)));
    let result = client.finish_connect(start);
    assert!(result.is_err());
    assert!(result.err().unwrap().to_string().contains("cancelled"));
    assert!(finished.load(Ordering::Acquire));
    venue.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut bytes = String::new();
    venue.read_to_string(&mut bytes).unwrap();
    assert!(bytes.contains("35=5"));
}

#[cfg(debug_assertions)]
#[test]
fn a_shutdown_reports_a_panic_caught_inside_the_engine() {
    let shared = Arc::new(SharedState::new());
    let mut engine = crate::engine::hot_loop::HotLoop::new(Arc::clone(&shared), None, None);
    engine.context.loop_iterations = u64::MAX;
    let (tx, rx) = std::sync::mpsc::channel();
    engine.set_control_rx(rx);
    let client = EClient::from_parts(
        shared,
        tx,
        thread::spawn(move || engine.run_with_panic_recovery()),
        "DU1".into(),
    );
    assert_eq!(client.disconnect(), Shutdown { logout_sent: false, engine: EngineEnd::Panicked });
}

#[test]
fn an_opened_session_taken_back_before_the_engine_is_logged_out() {
    let (mut conn, mut peer) = crate::protocol::connection::Connection::for_test();
    assert!(logged_out_if_taken_back(Some(&AtomicBool::new(true)), &mut conn));
    drop(conn);
    let mut messages = String::new();
    peer.read_to_string(&mut messages).unwrap();
    assert!(messages.contains("35=5"));
}

#[test]
fn a_replay_wait_accepts_a_bound_beyond_the_platform_clock() {
    let (client, _rx, shared) = super::tests::test_client();
    shared.orders.set_replay_done();
    shared.orders.note_the_venue_named(41);
    assert_eq!(client.next_shared_id_within(Some(Duration::MAX)), Ok(42));
}
