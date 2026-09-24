//! The tests for this module.
//!
//! One file per module, as `api/client` already does it. Each block below
//! reaches the code it tests through `super::super`, which is the module this
//! file belongs to.

use super::*;
use crate::types::SmartComponent;
use crate::bridge::RichOrderInfo;
use crate::types::model::OrderState as ApiOrderState;

/// Nothing follows the venue's chargeable one-shot.
///
/// The venue answers it once, under a request type of its own, and it is
/// withdrawn as soon as it completes. A
/// stream that followed one was never sent to the venue, and when the one-shot
/// went the follower was promoted onto its row — a subscription the caller was
/// told it had, on which no quote ever arrives.
#[test]
fn nothing_follows_a_chargeable_snapshot() {
    let core = ClientCore::new();
    let instrument = 3u32;

    // An ordinary stream is followed, which is the whole point of the map.
    core.instrument_to_req.lock().unwrap().insert(instrument, 10);
    assert!(
        core.follows_existing_subscription(instrument, 11, &[]),
        "a second caller watches the stream that is up",
    );

    // The one-shot is not.
    let one_shot = ClientCore::new();
    one_shot.instrument_to_req.lock().unwrap().insert(instrument, 20);
    one_shot.chargeable_snapshot_reqs.lock().unwrap().insert(20);
    assert!(
        !one_shot.follows_existing_subscription(instrument, 21, &[]),
        "a stream does not follow the one-shot",
    );
}

/// A stream is never served off the venue's one-shot, and both are recorded
/// as watching.
///
/// The one-shot was sent as a request of its own and is withdrawn the moment
/// it completes, so a stream served off it heard nothing after that. And what
/// is recorded as watching is what a reading is delivered to: recorded as
/// neither holder nor follower, the caller that asked for the one-shot was
/// sent nothing at all.
#[test]
fn a_stream_is_never_served_off_the_one_shot() {
    let instrument = 5u32;

    // A stream holds the slot and the one-shot watches beside it.
    let core = ClientCore::new();
    core.instrument_to_req.lock().unwrap().insert(instrument, 30);
    core.chargeable_snapshot_reqs.lock().unwrap().insert(31);
    assert!(
        core.take_or_follow(instrument, 31, &[], 0, 0),
        "the one-shot watches the slot the stream holds",
    );
    assert!(
        core.followers_of(instrument).contains(&31),
        "and is delivered to: {:?}", core.followers_of(instrument),
    );

    // The other way round the stream takes the slot, and the one-shot keeps
    // watching rather than being dropped.
    let other = ClientCore::new();
    other.instrument_to_req.lock().unwrap().insert(instrument, 40);
    other.chargeable_snapshot_reqs.lock().unwrap().insert(40);
    assert!(
        !other.take_or_follow(instrument, 41, &[], 0, 0),
        "the stream is sent rather than served off the one-shot",
    );
    assert_eq!(
        other.instrument_to_req.lock().unwrap().get(&instrument), Some(&41),
        "the stream holds the slot",
    );
    assert!(
        other.followers_of(instrument).contains(&40),
        "and the one-shot still hears what the venue says: {:?}",
        other.followers_of(instrument),
    );
}

/// A caller joining a contract is not erased by a withdrawal already under way.
///
/// Both decisions are about the same two maps, and a withdrawal that read the
/// watchers before it took the holder map read them from before the join: it
/// took the subscription down and answered that nothing was watching, while
/// the caller that had just joined kept a record of a feed that was gone and
/// heard nothing for the rest of its life.
#[test]
fn a_caller_that_joins_mid_withdrawal_keeps_the_subscription() {
    use std::sync::Arc;
    let core = Arc::new(ClientCore::new());
    let iid: InstrumentId = 0;

    // Request 1 holds the contract, as a first subscription leaves it.
    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    core.req_to_instrument.lock().unwrap().insert(1, iid);

    // The holder map is what a joiner writes under, so holding it here is
    // holding the withdrawal at the point the join has to be serialized
    // against.
    let holders = core.instrument_to_req.lock().unwrap();
    let c = Arc::clone(&core);
    let withdrawing = std::thread::spawn(move || c.unregister_mkt_data(1));

    std::thread::sleep(std::time::Duration::from_millis(150));

    // And a second caller joins while it waits there. Written straight into
    // the maps, because a joiner going the ordinary way would wait on the
    // holder map this test is holding — which is the point: the withdrawal
    // must read the watchers after this, not before.
    core.instrument_followers.lock().unwrap().entry(iid).or_default().push(2);
    core.req_to_instrument.lock().unwrap().insert(2, iid);
    drop(holders);

    withdrawing.join().unwrap();
    assert_eq!(
        core.instrument_to_req.lock().unwrap().get(&iid), Some(&2),
        "and that caller holds it",
    );
}

/// The venue's one-shot is still a one-shot until the slot it holds changes
/// hands.
///
/// Which number bought the burst and which number holds the slot are one
/// question: the mark is what keeps a stream from being served off it. Given
/// back first, a stream asking for that contract while the withdrawal was
/// halfway through read the one-shot as an ordinary stream, was recorded as
/// watching it, and sent nothing of its own — off a burst that was over.
#[test]
fn the_one_shot_keeps_its_kind_until_the_slot_changes_hands() {
    use std::sync::Arc;
    let core = Arc::new(ClientCore::new());
    let iid: InstrumentId = 0;

    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    core.req_to_instrument.lock().unwrap().insert(1, iid);
    core.chargeable_snapshot_reqs.lock().unwrap().insert(1);

    // The withdrawal waits on the holder map, as every decision about a
    // subscription does.
    let holders = core.instrument_to_req.lock().unwrap();
    let c = Arc::clone(&core);
    let withdrawing = std::thread::spawn(move || c.unregister_mkt_data(1));
    std::thread::sleep(std::time::Duration::from_millis(150));

    assert!(
        core.chargeable_snapshot_reqs.lock().unwrap().contains(&1),
        "while it still holds the slot, what holds the slot is still the one-shot",
    );
    drop(holders);

    withdrawing.join().unwrap();
    assert!(core.watchers_of(iid).is_empty(), "and the subscription goes with it");
    assert!(
        !core.chargeable_snapshot_reqs.lock().unwrap().contains(&1),
        "the number is an ordinary number again once the slot is given up",
    );
}

/// A request is watching the slot it took from the moment it takes it.
///
/// Which slot a number is watching is written under the maps that decide it,
/// not by whoever asked once they came back. Written after, a slot given back
/// in between was forgotten while this number pointed at nothing — and the
/// record landed afterwards, naming a slot whose next contract this caller
/// never asked about.
#[test]
fn a_request_watches_the_slot_it_took_from_the_moment_it_takes_it() {
    let core = ClientCore::new();
    let iid: InstrumentId = 4;

    assert!(!core.take_or_follow(iid, 9, &[], 0, 0), "nobody held it, so this one does");
    assert_eq!(
        core.watching(9), Some(iid),
        "and it is watching it without anything else being said",
    );
}

/// What a caller asked for is written down as it joins, not before.
///
/// A withdrawal decides which series nobody asks for any more from the
/// requests watching the contract. Written before the join, a list belonged to
/// a request that was watching nothing yet: the withdrawal running in between
/// read it as nobody's, and took the series the arriving caller had asked for.
#[test]
fn what_a_joining_caller_asked_for_is_written_down_as_it_joins() {
    let core = ClientCore::new();
    let iid: InstrumentId = 6;
    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    core.req_to_instrument.lock().unwrap().insert(1, iid);

    assert!(
        core.follows_existing_subscription(iid, 2, &[236]),
        "the second caller watches the subscription that is up",
    );
    assert_eq!(
        core.series_by_req.lock().unwrap().get(&2).map(Vec::as_slice),
        Some([236u32].as_slice()),
        "and what it asked for is its own from that moment",
    );
}

/// A slot given back takes its requests' marks with it.
///
/// The engine reclaims a slot and the requests that were watching it are
/// forgotten. Their marks were not: reused for an ordinary stream, such a
/// number was withdrawn as a snapshot the moment it had both sides of a quote,
/// or read as the venue's one-shot by every later join, or counted as still
/// asking for a series its caller has no subscription to hear.
#[test]
fn a_reclaimed_slot_takes_the_marks_of_what_was_watching_it() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    let iid: InstrumentId = 2;

    core.instrument_to_req.lock().unwrap().insert(iid, 5);
    core.req_to_instrument.lock().unwrap().insert(5, iid);
    core.chargeable_snapshot_reqs.lock().unwrap().insert(5);
    core.series_by_req.lock().unwrap().insert(5, vec![236]);
    core.snapshot_reqs.lock().unwrap().insert(5, crate::client_core::SnapshotWait::new(0, false));

    shared.market.note_released_slot(iid, u64::MAX);
    core.forget_released_slots(&shared);

    assert_eq!(core.watching(5), None, "the request is watching nothing");
    assert!(
        !core.chargeable_snapshot_reqs.lock().unwrap().contains(&5),
        "and is not the venue's one-shot",
    );
    assert!(
        !core.series_by_req.lock().unwrap().contains_key(&5),
        "and asks for no series",
    );
    assert!(
        !core.snapshot_reqs.lock().unwrap().contains_key(&5),
        "and is waiting on no snapshot to finish",
    );
}

/// A slot given back and then given again is not forgotten by the release that
/// freed it.
///
/// A release names a slot and nothing else, and the slot goes to the next
/// contract that needs one. Read afterwards, such a release forgot every
/// record of the contract that had just been given it: the venue went on
/// streaming it, no request could be found to deliver it to, and withdrawing
/// it answered that no contract was being watched under that number.
#[test]
fn a_slot_given_again_is_not_forgotten_by_the_release_that_freed_it() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    let iid: InstrumentId = 1;

    // The request that takes the slot asked for it under a number of its own.
    assert!(!core.take_or_follow(iid, 77, &[], 7, 0), "the contract now on it holds it");

    // A release decided before that is not about this occupancy.
    shared.market.note_released_slot(iid, 6);
    core.forget_released_slots(&shared);
    assert_eq!(
        core.watching(77), Some(iid),
        "the record of the contract now on the slot stands",
    );

    // And the release of this very occupancy is: the engine names the
    // occupancy that ended, which is the number this record was written under.
    shared.market.note_released_slot(iid, 7);
    core.forget_released_slots(&shared);
    assert_eq!(core.watching(77), None, "the slot it named has gone back");
}

/// A session keeps no figure for a subscription it is not holding.
///
/// The figure that tells one subscription under a number from the next is kept
/// per number, and a session that ends holds none of them. Carried over, the
/// next session's first withdrawal under a number read a figure from the
/// session before it — and the record grew for as long as the client lived.
#[test]
fn a_session_keeps_no_figure_for_a_subscription_it_is_not_holding() {
    let core = ClientCore::new();
    let iid: InstrumentId = 3;

    core.instrument_to_req.lock().unwrap().insert(iid, 5);
    core.req_to_instrument.lock().unwrap().insert(5, iid);
    core.stamp_registration(5);
    assert!(core.registration_of(5).is_some(), "it is holding one");

    core.unregister_mkt_data(5);
    assert!(core.registration_of(5).is_none(), "and holds none once it has given it up");

    core.stamp_registration(6);
    core.reset();
    assert!(core.registration_of(6).is_none(), "nor does the session that ended");
}

/// A market-data type nobody recognises does not become the venue's word.
///
/// Subscriptions stay realtime whatever it names, and the callback that
/// reports a subscription's type reads what was stored — so storing the number
/// would tell a caller their data is of a type the venue never stated and
/// their subscription is not on.
#[test]
fn an_unknown_market_data_type_is_not_kept() {
    let core = ClientCore::new();
    core.set_market_data_type(MDT_DELAYED);
    assert_eq!(core.subscription_mode(), 1);

    core.set_market_data_type(99);
    assert_eq!(core.subscription_mode(), 1, "the last known type still stands");
}

/// An account holding nothing still has a P&L, and a P&L of zero is an
/// answer. Neither an empty position list nor a zero value withholds it.
#[test]
fn an_account_with_no_positions_still_reports_its_pnl() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.set_account(&crate::types::AccountState::default());
    shared.portfolio.account_download_is_settled();

    core.subscribe_pnl(7, "").unwrap();
    let update = core.poll_pnl(&shared).into_iter().next().expect("a subscription is answered");
    assert_eq!(update.req_id, 7);
    assert_eq!(update.daily_pnl, 0.0);
    assert_eq!(update.unrealized_pnl, 0.0);
    assert_eq!(update.realized_pnl, 0.0);
    assert!(core.poll_pnl(&shared).into_iter().next().is_none(), "the same figures do not repeat");
}

/// The type a caller asks for has to reach the subscription, or asking for
/// delayed data got realtime-shaped subscriptions and no delayed ticks.
#[test]
fn the_requested_market_data_type_picks_the_subscription_mode() {
    let core = ClientCore::new();
    assert_eq!(core.subscription_mode(), 0, "realtime until asked otherwise");
    for (requested, mode) in [
        (MDT_DELAYED, 1),
        (MDT_FROZEN, 2),
        (MDT_DELAYED_FROZEN, 3),
        (MDT_REALTIME, 0),
    ] {
        core.set_market_data_type(requested);
        assert_eq!(core.subscription_mode(), mode, "type {requested}");
        assert_eq!(
            core.check_mdt_needed(requested as i64, true),
            Some(requested),
            "the callback names the type the data was asked for",
            );
    }
}

// ── Rejected/Inactive snapshot admission ──

#[test]
fn is_open_or_reactivatable_admits_genuine_inactive() {
    assert!(is_open_or_reactivatable("Inactive", ""));
}

#[test]
fn is_open_or_reactivatable_excludes_rejected_shaped_inactive() {
    // A rejected order also stringifies to "Inactive", but always carries
    // a non-empty completed_status — that is what must exclude it.
    assert!(!is_open_or_reactivatable("Inactive", "No valid bid/ask"));
}

#[test]
fn is_open_or_reactivatable_still_admits_ordinary_open_status() {
    assert!(is_open_or_reactivatable("Submitted", ""));
}

#[test]
fn is_open_or_reactivatable_still_excludes_terminal_status() {
    assert!(!is_open_or_reactivatable("Filled", ""));
    assert!(!is_open_or_reactivatable("Cancelled", ""));
}

#[test]
fn collect_open_orders_admits_inactive_but_excludes_rejected_locally_tracked() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.track_order(80, ApiContract::default(), ApiOrder { order_id: 80, ..Default::default() }, 0);
    core.track_order(81, ApiContract::default(), ApiOrder { order_id: 81, ..Default::default() }, 0);

    core.update_order_status(&shared, 80, OrderStatus::Inactive, 0.0, 100.0, 0);
    core.update_order_status(&shared, 81, OrderStatus::Rejected, 0.0, 100.0, 0);

    let result = core.collect_open_orders(&shared);
    assert!(result.iter().any(|(id, _)| *id == 80),
        "genuinely-inactive order must remain in the open-order snapshot");
    assert!(!result.iter().any(|(id, _)| *id == 81),
        "rejected order must not resurrect into the open-order snapshot");
}

#[test]
fn collect_open_orders_shared_only_admits_inactive_but_excludes_rejected() {
    let core = ClientCore::new();
    let shared = SharedState::new();

    shared.orders.push_order_info(90, RichOrderInfo {
        contract: ApiContract::default(),
        order: ApiOrder { order_id: 90, ..Default::default() },
        order_state: ApiOrderState { status: "Inactive".into(), ..Default::default() },
        last_exec: Default::default(),
    });
    shared.orders.push_order_info(91, RichOrderInfo {
        contract: ApiContract::default(),
        order: ApiOrder { order_id: 91, ..Default::default() },
        order_state: ApiOrderState {
            status: "Inactive".into(),
            completed_status: "No valid bid/ask".into(),
            ..Default::default()
        },
        last_exec: Default::default(),
    });

    let result = core.collect_open_orders(&shared);
    assert!(result.iter().any(|(id, _)| *id == 90),
        "genuinely-inactive shared-only order must be admitted to the open-order snapshot");
    assert!(!result.iter().any(|(id, _)| *id == 91),
        "rejected shared-only order must not resurrect into the open-order snapshot");
}

/// An order this client did not place still arrives through the shared
/// cache, and it carries its own filled quantity. Reporting zero made a
/// partially filled order read as untouched to anything polling
/// `req_open_orders`.
#[test]
fn a_shared_order_reports_its_filled_quantity() {
    let shared = SharedState::new();
    let core = ClientCore::new();
    let order = crate::types::model::Order {
        total_quantity: 10.0,
        filled_quantity: 4.0,
        ..Default::default()
    };
    let order_state = crate::types::model::OrderState {
        status: "Submitted".to_string(),
        ..Default::default()
    };
    shared.orders.push_order_info(55, crate::bridge::RichOrderInfo {
        contract: crate::types::model::Contract::default(),
        order,
        order_state,
        last_exec: crate::types::model::Execution::default(),
    });

    let open = core.collect_open_orders(&shared);
    let (_, tracked) = open.iter().find(|(id, _)| *id == 55).expect("the shared order");
        assert_eq!(tracked.filled, 4.0, "the filled quantity it carries");
    assert_eq!(tracked.remaining, 6.0, "and what is left of the order");
    }

/// A contract in slot 0 whose acknowledgement named BBO exchange `a6`, and
/// the map the venue stated for it.
fn shared_with_components(comps: Vec<(i32, &str)>) -> SharedState {
    let s = SharedState::new();
    s.reference.note_bbo_exchange(0, "a6", "STK");
    s.reference.set_smart_components_of(
        0, "STK",
        comps.into_iter().map(|(bit, letter)| SmartComponent {
            bit_number: bit,
            exchange: format!("EX{bit}"),
            exchange_letter: letter.to_string(),
        }).collect()
    );
    s
}

#[test]
fn render_exchange_mask_zero_is_empty() {
    let s = shared_with_components(vec![(0, "Q"), (1, "N")]);
    assert_eq!(render_exchange_mask(0, 0, &s), "");
}

#[test]
fn render_exchange_mask_single_bit() {
    let s = shared_with_components(vec![(0, "Q"), (1, "N"), (2, "P")]);
    assert_eq!(render_exchange_mask(0b001, 0, &s), "Q");
    assert_eq!(render_exchange_mask(0b100, 0, &s), "P");
}

#[test]
fn render_exchange_mask_multiple_bits() {
    let s = shared_with_components(vec![
        (0, "Q"), (1, "N"), (2, "P"), (3, "Z"),
    ]);
    // bits 0, 2, 3 set → letters in bit-order: Q, P, Z
    assert_eq!(render_exchange_mask(0b1101, 0, &s), "QPZ");
}

#[test]
fn render_exchange_mask_unknown_bit_skipped() {
    let s = shared_with_components(vec![(0, "Q")]);
    // bit 5 set, no component at bit 5 — skipped
    assert_eq!(render_exchange_mask(0b100000, 0, &s), "");
}

// ── what a P&L poll reports ──

fn seed_pnl_position(
    core: &ClientCore,
    shared: &SharedState,
    con_id: i64,
    iid: InstrumentId,
    position: f64,
    avg_cost_dollars: f64,
    last_dollars: f64,
    close_dollars: f64,
) {
    core.con_id_to_instrument.lock().unwrap().insert(con_id, iid);
    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    shared.portfolio.set_position_info(PositionInfo {
        con_id,
        position,
        avg_cost: (avg_cost_dollars * PRICE_SCALE_F) as i64,
        symbol: format!("SYM{con_id}"),
        sec_type: "STK".into(),
        currency: "USD".into(),
        multiplier: String::new(),
        ..Default::default()
    });
    let q = Quote {
        last: (last_dollars * PRICE_SCALE_F) as i64,
        close: (close_dollars * PRICE_SCALE_F) as i64,
        ..Default::default()
    };
    shared.market.push_quote(iid, &q);
}

#[test]
fn poll_pnl_no_subscription_returns_none() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    assert!(core.poll_pnl(&shared).into_iter().next().is_none());
}

/// A total missing one position is not a smaller correct total. When a
/// position cannot be priced the client-side sum is incomplete — and the
/// realized figure has already accrued for it, so the three do not even
/// agree with each other. The venue's account numbers are complete by
/// construction, so one unpriceable position sends the whole account there.
#[test]
fn one_unpriceable_position_sends_the_whole_account_to_the_gateway() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_pnl(11, "").unwrap();

    // One ordinary position that prices fine.
    seed_pnl_position(&core, &shared, 1, 0, 1.0, 100.00, 101.00, 100.00);

    // And one held overnight that this session cannot size.
    core.con_id_to_instrument.lock().unwrap().insert(2, 1);
    core.instrument_to_req.lock().unwrap().insert(1, 1);
    let q = Quote {
        last: (735.00 * PRICE_SCALE_F) as i64,
        close: (730.00 * PRICE_SCALE_F) as i64,
        ..Default::default()
    };
    shared.market.push_quote(1, &q);
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 2, qty_midnight: Some(10.0), cost_midnight: None, qty_traded: None,
        money_traded: 0.0, realized_pnl: 0.0,
    }]);
    shared.portfolio.set_account(&AccountState {
        daily_pnl: (51.0 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (351.0 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });
    shared.portfolio.account_download_is_settled();

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    assert!(
        (update.daily_pnl - 51.0).abs() < 1e-6,
        "the gateway's complete figure, not the one priceable position: daily={}",
        update.daily_pnl,
    );
}

/// `pnlSingle` loses only its daily figure when the overnight size is
/// unknown. The position, its value, the unrealized and the realized are all
/// still known, and suppressing the callback would leave every one of them
/// stale on the caller's side.
#[test]
fn an_unknown_seed_does_not_suppress_the_rest_of_a_single_callback() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl_single(21, 756733, "").unwrap();

    seed_pnl_position(&core, &shared, 756733, 0, 10.0, 700.00, 735.00, 730.00);
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 756733, qty_midnight: None, cost_midnight: None, qty_traded: None,
        money_traded: 0.0, realized_pnl: 0.0,
    }]);

    let first = core.poll_pnl_single(&shared);
    assert!(!first.is_empty(), "the known fields must still be reported");
    assert!((first[0].pos - 10.0).abs() < 1e-6, "position");
    assert!((first[0].unrealized_pnl - 350.0).abs() < 1e-6, "unrealized");

    // And a later change to a field that IS known still produces an update.
    let q = Quote {
        last: (736.00 * PRICE_SCALE_F) as i64,
        close: (730.00 * PRICE_SCALE_F) as i64,
        ..Default::default()
    };
    shared.market.push_quote(0, &q);
    let second = core.poll_pnl_single(&shared);
    assert!(!second.is_empty(), "a moved quote must still reach the caller");
        assert!((second[0].unrealized_pnl - 360.0).abs() < 1e-6, "unrealized moved");
}

 ///, consumer side. Dropping an unusable position row stops the feed
/// publishing a flat, but P&L reads the absence back as zero shares and
/// reports the whole overnight holding as sold. Held 10 at a $730 close,
/// now $735: the honest answer is 50, the flat reading is -7300, and with
/// nothing priceable the venue's account figure stands instead.
#[test]
fn an_unsizeable_overnight_position_is_not_priced_as_sold() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_pnl(7, "").unwrap();

    // A quote and a seed, but no position row — the feed dropped it.
    core.con_id_to_instrument.lock().unwrap().insert(756733, 0);
    core.instrument_to_req.lock().unwrap().insert(0, 1);
    let q = Quote {
        last: (735.00 * PRICE_SCALE_F) as i64,
        close: (730.00 * PRICE_SCALE_F) as i64,
        ..Default::default()
    };
    shared.market.push_quote(0, &q);
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 756733,
        qty_midnight: Some(10.0),
        cost_midnight: None,
        qty_traded: None,
        money_traded: 0.0,
        realized_pnl: 0.0,
    }]);
    shared.portfolio.set_account(&AccountState {
        daily_pnl: (50.0 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (350.0 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });
    shared.portfolio.account_download_is_settled();

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    assert!(
        (update.daily_pnl - 50.0).abs() < 1e-6,
        "the stated figure stands; -7300 is the flat reading: daily={}",
        update.daily_pnl,
    );
}

/// The same absence on the overnight leg. A seed row that stated no
/// quantity means the position's midnight size is unknown — not that it was
/// opened today, which is what a missing row means. Held 10 from $700, a
/// $730 close and $735 now: the intraday reading synthesizes cash from
/// average cost and reports 350, the unrealized figure, as the day's move.
#[test]
fn a_seed_without_a_quantity_is_not_read_as_opened_today() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_pnl(8, "").unwrap();

    seed_pnl_position(&core, &shared, 756733, 0, 10.0, 700.00, 735.00, 730.00);
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 756733,
        qty_midnight: None,
        cost_midnight: None,
        qty_traded: None,
        money_traded: 0.0,
        realized_pnl: 0.0,
    }]);
    shared.portfolio.set_account(&AccountState {
        daily_pnl: (50.0 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (350.0 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });
    shared.portfolio.account_download_is_settled();

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    assert!(
        (update.daily_pnl - 50.0).abs() < 1e-6,
        "350 is the intraday synthesis, not the day's move: daily={}",
        update.daily_pnl,
    );
}

/// A stated zero is a figure, not a silence.
///
/// A position marked at what it cost has made nothing, and the venue says so.
/// Read as though nothing had been said, that fell through to a figure worked
/// out here from the last print — so a caller was told a position had made
/// something on a report where the venue said it had made nothing.
#[test]
fn a_position_the_venue_says_has_made_nothing_is_reported_as_nothing() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl_single(11, 8002, "").unwrap();

    // Held at 100, and the last print is 105 — so a figure worked out here
    // would say 50. The venue marks it at what it cost and states that it has
    // made nothing, which is the answer.
    seed_pnl_position(&core, &shared, 8002, 0, 10.0, 100.0, 105.0, 100.0);
    shared.portfolio.set_position_marks(8002, Some((100.0 * PRICE_SCALE_F) as i64), None, Some(0), None);

    let updates = core.poll_pnl_single(&shared);
    let update = updates.first().expect("callback must fire");
    assert_eq!(
        update.unrealized_pnl, 0.0,
        "the venue said nothing was made, so nothing was made",
    );
}

/// A position with no midnight row and no cost has no basis to be measured
/// against, so the day's figure is held rather than invented.
///
/// The opening cash synthesized for an intraday position is `-qty * avgCost`,
/// which is nought where the cost is unknown, and there is no midnight value to
/// seed from either. Worked out anyway, the whole of what the position is worth
/// went out as the day's profit — a position bought at an unstated price and
/// marked at 105 was reported as having made its entire market value today. The
/// venue states a cost often rather than always, so this is reached with the
/// venue's own rows.
#[test]
fn a_position_with_no_seed_and_no_cost_does_not_report_its_whole_value_as_the_day() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl_single(11, 8003, "").unwrap();

    // Ten held, marked at 105, and the venue has stated no cost for them.
    seed_pnl_position(&core, &shared, 8003, 0, 10.0, 0.0, 105.0, 0.0);

    let updates = core.poll_pnl_single(&shared);
    let update = updates.first().expect("callback must fire");
    assert_eq!(
        update.daily_pnl, 0.0,
        "nothing is known to measure the day against, so nothing is claimed for it",
    );
}

/// And the account total does the same: a position it cannot price sends the
/// whole account to the venue's own figures.
///
/// Counted as priced, the client-side sum stood as the account's total while
/// booking a position's entire market value as the day's profit — and the
/// realized figure had already accrued for it, so the three did not even agree
/// with one another.
#[test]
fn a_position_with_no_seed_and_no_cost_sends_the_account_total_to_the_venues_figures() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(42, "").unwrap();

    // What the venue says the account has made, which is complete by
    // construction and is what an incomplete local sum falls back to.
    shared.portfolio.set_account(&AccountState {
        daily_pnl: (12.5 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (7.5 * PRICE_SCALE_F) as i64,
        realized_pnl: (5.0 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });
    seed_pnl_position(&core, &shared, 8004, 0, 10.0, 0.0, 105.0, 0.0);

    let update = core.poll_pnl(&shared).into_iter().next().expect("a subscription is answered");
    assert!(
        (update.daily_pnl - 12.5).abs() < 1e-6,
        "the venue's own total stands in: daily={}",
        update.daily_pnl,
    );
    assert!((update.unrealized_pnl - 7.5).abs() < 1e-6);
    assert!((update.realized_pnl - 5.0).abs() < 1e-6);
}

#[test]
fn poll_pnl_intraday_opened_position_fires_callback() {
    // An account flat at midnight that opens a position during the day.
    // Before fix: poll_pnl early-returned on empty seeds → no callback.
    // After fix: position iterated, money_traded synthesized, daily P&L = unrealized.
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(42, "").unwrap();

    // 1 share bought at $735.00, now $735.07. No midnight seed (flat at midnight).
    seed_pnl_position(&core, &shared, 756733, 0, 1.0, 735.00, 735.07, 0.0);

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    assert_eq!(update.req_id, 42);
    assert!((update.daily_pnl - 0.07).abs() < 1e-6, "daily={}", update.daily_pnl);
    assert!((update.unrealized_pnl - 0.07).abs() < 1e-6);
    assert!((update.realized_pnl - 0.0).abs() < 1e-6);
}

#[test]
fn poll_pnl_overnight_position_with_seed_unchanged() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(99, "").unwrap();

    // Held 10 SPY through midnight: qty_midnight=10, prev_close=$730, avg_cost=$700.
    // No fills today (money_traded=0). Current price $735.
    seed_pnl_position(&core, &shared, 756733, 0, 10.0, 700.00, 735.00, 730.00);
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 756733,
        qty_midnight: Some(10.0),
        cost_midnight: None,
        qty_traded: None,
        money_traded: 0.0,
        realized_pnl: 0.0,
    }]);

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    // daily = 10×735 - 10×730 - 0 = 50
    assert!((update.daily_pnl - 50.0).abs() < 1e-6, "daily={}", update.daily_pnl);
    // unrealized = 10 × (735 - 700) = 350
    assert!((update.unrealized_pnl - 350.0).abs() < 1e-6);
}

#[test]
fn poll_pnl_seeded_position_traded_intraday_uses_signed_net_cash() {
    // /: a position held at midnight AND traded intraday
    // carries a non-zero moneyTradedSinceMidnight (6822), signed SELL+/BUY-.
    // The daily formula must ADD it. Sold 3 of 10 at $110 (avg $100): the
    // seed carries +330 net cash (sell proceeds) and +30 realized.
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(31, "").unwrap();

    // Now holding 7 (was 10 at midnight), avg $100, last $110, prev close $100.
    seed_pnl_position(&core, &shared, 1, 0, 7.0, 100.00, 110.00, 100.00);
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 1,
        qty_midnight: Some(10.0),
        cost_midnight: None,
        qty_traded: None,
        money_traded: 330.0,   // +330 = sold 3 @ $110 (wire sign, SELL positive)
        realized_pnl: 30.0,
    }]);

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    // daily = 7×110 - 10×100 + 330 = 100 (70 remaining unrealized + 30 realized)
    assert!((update.daily_pnl - 100.0).abs() < 1e-6, "daily={}", update.daily_pnl);
    // unrealized = 7 × (110 - 100) = 70
    assert!((update.unrealized_pnl - 70.0).abs() < 1e-6, "unreal={}", update.unrealized_pnl);
    assert!((update.realized_pnl - 30.0).abs() < 1e-6, "real={}", update.realized_pnl);
}

#[test]
fn poll_pnl_change_detection_suppresses_duplicate() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(7, "").unwrap();
    seed_pnl_position(&core, &shared, 1, 0, 1.0, 100.0, 101.0, 0.0);
    assert!(core.poll_pnl(&shared).into_iter().next().is_some());
    // Same inputs → no callback.
    assert!(core.poll_pnl(&shared).into_iter().next().is_none());
}

#[test]
fn poll_pnl_falls_back_to_account_level_without_market_data() {
    // A client that asks only for P&L never subscribes to market data, so no
    // position has a live quote (con_id_to_instrument is empty and every
    // position hits `continue`). poll_pnl must then emit the venue's
    // account-level P&L instead of returning None forever.
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_pnl(21, "").unwrap();

    // Open position, but NO instrument mapping and NO quote pushed.
    shared.portfolio.set_position_info(PositionInfo {
        con_id: 756733,
        position: 10.0,
        avg_cost: (700.00 * PRICE_SCALE_F) as i64,
        symbol: "SPY".into(),
        sec_type: "STK".into(),
        currency: "USD".into(),
        multiplier: String::new(),
        ..Default::default()
    });

    // Gateway-pushed account-level P&L (from the DailyPnL/UnrealizedPnL/
    // RealizedPnL account-value keys).
    let acct = AccountState {
        daily_pnl: (12.50 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (35.00 * PRICE_SCALE_F) as i64,
        realized_pnl: (4.00 * PRICE_SCALE_F) as i64,
        ..Default::default()
    };
    shared.portfolio.set_account(&acct);
    shared.portfolio.account_download_is_settled();

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire from account-level P&L");
    assert_eq!(update.req_id, 21);
    assert!((update.daily_pnl - 12.50).abs() < 1e-6, "daily={}", update.daily_pnl);
    assert!((update.unrealized_pnl - 35.00).abs() < 1e-6, "unreal={}", update.unrealized_pnl);
    assert!((update.realized_pnl - 4.00).abs() < 1e-6, "real={}", update.realized_pnl);
}

/// A contract this session never quoted is valued from what the venue
/// states for it: a price, and what it was worth at midnight. The account
/// total is computed from those rather than deferred elsewhere.
#[test]
fn the_overnight_leg_is_valued_at_the_mark_the_venue_states() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(31, "").unwrap();

    // Quoted at 101.25 now, with no locally derived previous close. The venue
    // states the mark it closed the contract at, which is what the
    // overnight leg is valued against.
    seed_pnl_position(&core, &shared, 5001, 0, 10.0, 100.00, 101.25, 0.0);
    shared.portfolio.set_midnight_seeds("PLR.31".into(), vec![MidnightSeed {
        con_id: 5001,
        qty_midnight: Some(10.0),
        cost_midnight: None,
        qty_traded: Some(0.0),
        money_traded: 0.0,
        realized_pnl: 2.50,
    }]);
    shared.portfolio.set_venue_prices([(5001i64, "100.00".to_string())].into());

    // Account-level figures that would be visible if the client fell back.
    shared.portfolio.set_account(&AccountState {
        daily_pnl: (999.0 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (999.0 * PRICE_SCALE_F) as i64,
        realized_pnl: (999.0 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    // 10 × 101.25 now, against 10 × 100.00 the venue marked it at overnight.
    assert!((update.daily_pnl - 12.50).abs() < 1e-6, "daily={}", update.daily_pnl);
    assert!((update.unrealized_pnl - 12.50).abs() < 1e-6, "unreal={}", update.unrealized_pnl);
    assert!((update.realized_pnl - 2.50).abs() < 1e-6, "real={}", update.realized_pnl);
}

/// The venue states what a position was worth at midnight. The client's own
/// answer is the overnight size times a previous close, which it holds for
/// no contract it never quoted and which is the wrong figure whenever the
/// two disagree. The stated one wins.
#[test]
fn the_venues_midnight_value_beats_the_clients_previous_close() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(32, "").unwrap();

    // Quoted at 101.00 with a previous close of 90.00.
    seed_pnl_position(&core, &shared, 7001, 0, 10.0, 100.00, 101.00, 90.00);
    shared.portfolio.set_midnight_seeds("PLR.32".into(), vec![MidnightSeed {
        con_id: 7001,
        qty_midnight: Some(10.0),
        cost_midnight: Some(1000.00),
        qty_traded: Some(0.0),
        money_traded: 0.0,
        realized_pnl: 0.0,
    }]);

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    // 1010.00 against the stated 1000.00, not against 10 × 90.00.
    assert!((update.daily_pnl - 10.0).abs() < 1e-6, "daily={}", update.daily_pnl);
}

/// The table is kept as the venue wrote it and read where it is used, so text
/// that is not a price costs its own contract a valuation. What it must not do
/// is leave the rest of the account reported as the whole of it: the realized
/// figure accrues for a contract that cannot be marked while the daily and
/// unrealized ones do not, so a partial sum does not even agree with itself.
/// The account goes to the venue's figures instead.
#[test]
fn a_mark_that_does_not_read_as_a_number_sends_the_account_to_the_venue() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_pnl(33, "").unwrap();

    for (i, con_id) in [6001i64, 6002, 6003].into_iter().enumerate() {
        seed_pnl_position(&core, &shared, con_id, i as u32, 1.0, 50.00, 51.00, 0.0);
    }
    shared.portfolio.set_midnight_seeds("PLR.33".into(), vec![
        MidnightSeed {
            con_id: 6001, qty_midnight: Some(1.0), cost_midnight: None,
            qty_traded: Some(0.0), money_traded: 0.0, realized_pnl: 2.00,
        },
        MidnightSeed {
            con_id: 6002, qty_midnight: Some(1.0), cost_midnight: None,
            qty_traded: Some(0.0), money_traded: 0.0, realized_pnl: 3.00,
        },
        MidnightSeed {
            con_id: 6003, qty_midnight: Some(1.0), cost_midnight: None,
            qty_traded: Some(0.0), money_traded: 0.0, realized_pnl: 4.00,
        },
    ]);
    shared.portfolio.set_venue_prices([
        (6001i64, "50.00".to_string()),
        (6002i64, "n/a".to_string()),
        // Nothing is worth nothing. A table that has yet to mark a contract
        // says so with a zero, and valuing the holding at it reports the
        // whole position as having gone to nought overnight.
        (6003i64, "0.00".to_string()),
    ].into());

    // What the venue says about the account as a whole, which is complete by
    // construction where a sum built here is not.
    shared.portfolio.set_account(&AccountState {
        daily_pnl: (12.0 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (34.0 * PRICE_SCALE_F) as i64,
        realized_pnl: (9.0 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });
    shared.portfolio.account_download_is_settled();

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    assert!((update.daily_pnl - 12.0).abs() < 1e-6,
        "two contracts could not be marked, so the venue's total stands, daily={}",
        update.daily_pnl);
    assert!((update.unrealized_pnl - 34.0).abs() < 1e-6, "unreal={}", update.unrealized_pnl);
    assert!((update.realized_pnl - 9.0).abs() < 1e-6, "real={}", update.realized_pnl);
    assert_eq!(
        shared.portfolio.venue_price(6002).as_deref(), Some("n/a"),
        "the table holds what the venue wrote; reading it is the caller's job",
    );
}

#[test]
fn poll_pnl_prefers_quotes_over_account_level_when_priced() {
    // When market data IS subscribed, the per-position quote synthesis wins;
    // the account-level fallback must not override it.
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(22, "").unwrap();

    // Priced position: 1 share, avg 100, last 101 → daily/unrealized = 1.00.
    seed_pnl_position(&core, &shared, 1, 0, 1.0, 100.0, 101.0, 0.0);

    // Divergent account-level values that must be ignored while priced.
    let acct = AccountState {
        daily_pnl: (999.0 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (999.0 * PRICE_SCALE_F) as i64,
        ..Default::default()
    };
    shared.portfolio.set_account(&acct);

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    assert!((update.daily_pnl - 1.0).abs() < 1e-6, "daily={}", update.daily_pnl);
    assert!((update.unrealized_pnl - 1.0).abs() < 1e-6, "unreal={}", update.unrealized_pnl);
}

// ── what a single-position P&L poll reports ──

#[test]
fn poll_pnl_single_routes_quote_by_con_id() {
    // #168 (bug 3): two subscribed instruments, different prices — each req_id
    // must see the price of its own con_id, not the first non-zero quote.
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();

    seed_pnl_position(&core, &shared, 111, 0, 1.0, 100.0, 105.0, 0.0);  // SPY
    seed_pnl_position(&core, &shared, 222, 1, 1.0, 200.0, 210.0, 0.0);  // QQQ

    core.subscribe_pnl_single(50, 111, "").unwrap();
    core.subscribe_pnl_single(51, 222, "").unwrap();

    let updates = core.poll_pnl_single(&shared);
    assert_eq!(updates.len(), 2);

    let spy = updates.iter().find(|u| u.req_id == 50).expect("SPY update");
    let qqq = updates.iter().find(|u| u.req_id == 51).expect("QQQ update");
    // Unrealized = qty × (last - avg_cost). SPY: 1×(105-100)=5; QQQ: 1×(210-200)=10.
    assert!((spy.unrealized_pnl - 5.0).abs() < 1e-6);
    assert!((qqq.unrealized_pnl - 10.0).abs() < 1e-6);
    // Value = qty × last. SPY: 105; QQQ: 210.
    assert!((spy.value - 105.0).abs() < 1e-6);
    assert!((qqq.value - 210.0).abs() < 1e-6);
}

#[test]
fn poll_pnl_single_intraday_opened_position() {
    // #168 (bug 1): daily_pnl must be computed, not hardcoded 0.
    // No seed → money_traded synthesized, daily collapses to unrealized.
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    seed_pnl_position(&core, &shared, 756733, 0, 1.0, 735.00, 735.07, 0.0);
    core.subscribe_pnl_single(42, 756733, "").unwrap();

    let updates = core.poll_pnl_single(&shared);
    assert_eq!(updates.len(), 1);
    let u = &updates[0];
    assert_eq!(u.req_id, 42);
    assert!((u.daily_pnl - 0.07).abs() < 1e-6, "daily={}", u.daily_pnl);
    assert!((u.unrealized_pnl - 0.07).abs() < 1e-6);
    assert!((u.realized_pnl - 0.0).abs() < 1e-6);
}

#[test]
fn poll_pnl_single_overnight_position_with_seed() {
    // #168 (bug 2): realized_pnl must come from the seed, not hardcoded 0.
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    seed_pnl_position(&core, &shared, 756733, 0, 10.0, 700.00, 735.00, 730.00);
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 756733,
        qty_midnight: Some(10.0),
        cost_midnight: None,
        qty_traded: None,
        money_traded: 0.0,
        realized_pnl: 12.34,
    }]);
    core.subscribe_pnl_single(99, 756733, "").unwrap();

    let updates = core.poll_pnl_single(&shared);
    assert_eq!(updates.len(), 1);
    let u = &updates[0];
    // daily = 10×735 − 10×730 − 0 = 50
    assert!((u.daily_pnl - 50.0).abs() < 1e-6);
    // unrealized = 10 × (735 − 700) = 350
    assert!((u.unrealized_pnl - 350.0).abs() < 1e-6);
    assert!((u.realized_pnl - 12.34).abs() < 1e-6);
}

/// A multiplied contract opened today has a day's profit, and it is the whole
/// of what it has made.
///
/// Nothing was held overnight, so the overnight leg is worth nothing — which
/// is exact whatever a contract is worth a unit of, and the one case the
/// multiplier cannot spoil. Refused along with the rest, every intraday option
/// and future reported no day's profit at all, and went on reporting it for
/// the rest of the session because the arm that holds the last figure then
/// holds that nought.
#[test]
fn a_multiplied_position_opened_today_reports_the_day_it_has_made() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl_single(32, 4002, "").unwrap();

    // Ten option contracts bought today at 7.00 a unit — 7,000 paid — and
    // marked by the venue at 7,350 for the position.
    shared.portfolio.set_position_info(PositionInfo {
        con_id: 4002,
        position: 10.0,
        avg_cost: (700.0 * PRICE_SCALE_F) as i64,
        symbol: "SPY   260320C00600000".into(),
        sec_type: "OPT".into(),
        currency: "USD".into(),
        multiplier: "100".into(),
        ..Default::default()
    });
    shared.portfolio.set_position_marks(
        4002,
        Some((7.35 * PRICE_SCALE_F) as i64),
        Some((7350.0 * PRICE_SCALE_F) as i64),
        None,
        None,
    );
    // No midnight row at all, which is what the venue sends for a position
    // that did not exist at midnight.
    core.con_id_to_instrument.lock().unwrap().insert(4002, 0);
    shared.market.push_quote(0, &Quote {
        last: (7.35 * PRICE_SCALE_F) as i64,
        close: (7.30 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });

    let updates = core.poll_pnl_single(&shared);
    let update = updates.first().expect("callback must fire");
    assert!(
        (update.daily_pnl - 350.0).abs() < 1e-6,
        "worth 7,350 and bought for 7,000 today: {}",
        update.daily_pnl,
    );
}

/// A multiplied contract's overnight leg is valued the way today's leg is, or
/// not at all.
///
/// What today's leg is worth comes from the venue, and that figure already
/// carries the contract's multiplier. The overnight leg has no such figure on
/// every row, and sized from the overnight quantity times a previous close it
/// carries no multiplier at all — so subtracting one from the other reported
/// the day's change short by the multiplier, a hundredfold on an equity
/// option. With nothing this arithmetic can use, the last reported figure
/// stands rather than a wrong one going out.
#[test]
fn a_multiplied_position_does_not_size_its_overnight_leg_from_a_unit_price() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl_single(31, 4001, "").unwrap();

    // An option: ten contracts on a hundred units each, marked by the venue at
    // 7.35 a unit — 7,350 for the position, which is what the venue states.
    shared.portfolio.set_position_info(PositionInfo {
        con_id: 4001,
        position: 10.0,
        avg_cost: (700.0 * PRICE_SCALE_F) as i64,
        symbol: "SPY   260320C00600000".into(),
        sec_type: "OPT".into(),
        currency: "USD".into(),
        multiplier: "100".into(),
        ..Default::default()
    });
    shared.portfolio.set_position_marks(
        4001,
        Some((7.35 * PRICE_SCALE_F) as i64),
        Some((7350.0 * PRICE_SCALE_F) as i64),
        None,
        None,
    );
    // The venue's row states what was held overnight and not what it was
    // worth, which is the case this is about.
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 4001,
        qty_midnight: Some(10.0),
        cost_midnight: None,
        qty_traded: None,
        money_traded: 0.0,
        realized_pnl: 0.0,
    }]);
    core.con_id_to_instrument.lock().unwrap().insert(4001, 0);
    shared.market.push_quote(0, &Quote {
        last: (7.35 * PRICE_SCALE_F) as i64,
        close: (7.30 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });

    let updates = core.poll_pnl_single(&shared);
    let update = updates.first().expect("callback must fire");
    // Sized per unit the day's change reads as 7,350 less 73 — the whole
    // position's value against one unit's.
    assert_eq!(
        update.daily_pnl, 0.0,
        "nothing here can value the overnight leg the way the venue values today's",
    );
    assert!((update.value - 7350.0).abs() < 1e-6, "and the value is still the venue's");
}

#[test]
fn poll_pnl_single_change_detection_suppresses_duplicate() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    seed_pnl_position(&core, &shared, 1, 0, 1.0, 100.0, 101.0, 0.0);
    core.subscribe_pnl_single(7, 1, "").unwrap();
    assert_eq!(core.poll_pnl_single(&shared).len(), 1);
    // Same inputs → no emit.
    assert!(core.poll_pnl_single(&shared).is_empty());
}

/// A delayed subscription numbers its ticks as delayed.
///
/// The caller was told on `market_data_type` that the feed was delayed and
/// then handed it under the realtime numbers; the reference client numbers a
/// delayed feed under its own, from 66.
#[test]
fn a_delayed_subscription_numbers_its_ticks_as_delayed() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.mdt_by_instrument.lock().unwrap().insert(0, MDT_DELAYED);
    shared.market.push_quote(0, &Quote {
        bid: 100 * crate::types::PRICE_SCALE, ask: 101 * crate::types::PRICE_SCALE,
        last: 100 * crate::types::PRICE_SCALE + crate::types::PRICE_SCALE / 2,
        bid_size: 3 * crate::types::QTY_SCALE, timestamp_ns: 1_757_000_000_000_000_000,
        ..Default::default()
    });
    let polled = core.poll_instrument_ticks(&shared, 0, 11);
    let mut numbered: Vec<i32> = polled.ticks.iter().map(|t| t.tick_type).collect();
    numbered.sort_unstable();
    assert_eq!(numbered, vec![66, 67, 68, 69], "delayed bid, ask, last and bid size: {numbered:?}");
    assert!(polled.delayed, "and the timestamp goes out under the delayed number");
}

/// A chargeable snapshot's answer reaches the snapshot's own request and no
/// stream watching the same contract, and the snapshot ends once it has, as a
/// gateway ends it: the answer carries no open, so waiting for every kind an
/// ordinary snapshot is made of ran it to the sweep every time.
#[test]
fn a_chargeable_snapshot_is_answered_to_itself_and_ends_there() {
    use crate::types::{SeriesTick, SeriesValue};
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A stream holds the contract, and the snapshot watches it beside it.
    core.req_to_instrument.lock().unwrap().extend([(4, 0), (5, 0)]);
    core.instrument_to_req.lock().unwrap().insert(0, 4);
    core.instrument_followers.lock().unwrap().insert(0, vec![5]);
    core.chargeable_snapshot_reqs.lock().unwrap().insert(5);
    core.snapshot_reqs.lock().unwrap().insert(5, crate::client_core::SnapshotWait::new(0, false));

    core.poll_instrument_ticks(&shared, 0, 4);
    assert!(!core.check_snapshot_done(5), "nothing answered yet");

    shared.market.push_snapshot_answer(0, vec![
        SeriesTick { instrument: 0, tick_type: 1, value: SeriesValue::Price(150.25) },
        SeriesTick { instrument: 0, tick_type: 0, value: SeriesValue::Size(3.0) },
        SeriesTick { instrument: 0, tick_type: 85, value: SeriesValue::Text("1790000000000".into()) },
    ]);
    let polled = core.poll_instrument_ticks(&shared, 0, 4);
    let said: Vec<(i64, i32, f64, bool)> = polled.snapshot_ticks.iter()
        .map(|t| (t.req_id, t.tick_type, t.value, t.is_price))
        .collect();
    assert_eq!(said, [(5, 1, 150.25, true), (5, 0, 3.0, false)], "the snapshot's, and no one else's");
    let stamped: Vec<(i64, i32, &str)> = polled.snapshot_strings.iter()
        .map(|t| (t.req_id, t.tick_type, t.value.as_str()))
        .collect();
    assert_eq!(stamped, [(5, 85, "1790000000000")]);
    assert!(polled.ticks.is_empty() && polled.string_ticks.is_empty(), "the stream hears none of it");
    assert!(core.check_snapshot_done(5), "the answer is the whole of it");
}

/// A bond's yields on a delayed feed go out under the delayed numbers.
///
/// A gateway publishes a delayed feed's bid and ask yields on 103 and 104 and
/// publishes no last yield on one; they had gone out on 50, 51 and 52 whatever
/// the feed, where a program that asked for delayed data does not look.
#[test]
fn a_delayed_feed_states_its_yields_under_the_delayed_numbers() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    let push_yields = |shared: &SharedState| {
        for (tick_type, value) in [(50, 7.1752), (51, 7.2), (52, 7.1)] {
            shared.market.push_series_tick(crate::types::SeriesTick {
                instrument: 0, tick_type, value: crate::types::SeriesValue::Price(value),
            });
        }
    };
    let yields = |polled: &QuotePollResult| -> Vec<(i32, f64, bool)> {
        polled.ticks.iter().map(|t| (t.tick_type, t.value, t.is_price)).collect()
    };

    push_yields(&shared);
    let live = core.poll_instrument_ticks(&shared, 0, 11);
    assert_eq!(
        yields(&live),
        [(50, 7.1752, true), (51, 7.2, true), (52, 7.1, true)],
        "a live feed's yields are the live numbers",
    );

    core.mark_feed_delayed_for_test(0);
    push_yields(&shared);
    let delayed = core.poll_instrument_ticks(&shared, 0, 11);
    assert_eq!(
        yields(&delayed),
        [(103, 7.1752, true), (104, 7.2, true)],
        "bid and ask yields on 103 and 104, and no last yield",
    );
}

/// A holding that moves by less than a whole unit is a change.
///
/// The change key held the quantity as a whole number, so a fractional
/// holding — a crypto position — that moved inside one unit while the venue's
/// marks stood was reported nothing.
#[test]
fn poll_pnl_single_reports_a_fractional_move_in_the_holding() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.account_download_is_settled();
    let marks = |shared: &SharedState| shared.portfolio.set_position_marks(
        479624278, Some(crate::types::price_from_f64(101.0)), Some(crate::types::price_from_f64(151.5)),
        Some(crate::types::price_from_f64(1.5)), None,
    );
    seed_pnl_position(&core, &shared, 479624278, 0, 1.5, 100.0, 101.0, 0.0);
    marks(&shared);
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 479624278, qty_midnight: Some(1.5), cost_midnight: Some(150.0),
        qty_traded: None, money_traded: 0.0, realized_pnl: 0.0,
    }]);
    core.subscribe_pnl_single(7, 479624278, "").unwrap();
    assert_eq!(core.poll_pnl_single(&shared).len(), 1);
    // The holding grows inside the same whole unit; the venue's marks stand.
    seed_pnl_position(&core, &shared, 479624278, 0, 1.9, 100.0, 101.0, 0.0);
    marks(&shared);
    assert_eq!(core.poll_pnl_single(&shared).len(), 1, "a move in the holding is reported");
}

#[test]
fn poll_pnl_single_unsubscribe_clears_cache() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    seed_pnl_position(&core, &shared, 1, 0, 1.0, 100.0, 101.0, 0.0);
    core.subscribe_pnl_single(7, 1, "").unwrap();
    let _ = core.poll_pnl_single(&shared);
    core.unsubscribe_pnl_single(7);
    // Re-subscribing with same req_id must re-emit (cache cleared on unsubscribe).
    core.subscribe_pnl_single(7, 1, "").unwrap();
    assert_eq!(core.poll_pnl_single(&shared).len(), 1);
}
/// Adaptive, algo and what-if orders leave `build_order_request` through
/// their own branches, and each still reaches the extended-attribute
/// block: outside-RTH, a parent link, an OCA group and a non-DAY tif are
/// carried on all of them rather than accepted and dropped. Asserted on
/// the request the API layer produces, which is where a drop would occur.
#[test]
fn the_algo_order_types_carry_the_attributes_the_caller_set() {
    let base = ApiOrder {
        action: "BUY".into(),
        total_quantity: 100.0,
        order_type: "LMT".into(),
        lmt_price: 150.0,
        tif: "GTC".into(),
        outside_rth: true,
        parent_id: 42,
        oca_group: "bracket_1".into(),
        ..Default::default()
    };
    let cases = [
        ("adaptive", ApiOrder { algo_strategy: "Adaptive".into(), ..base.clone() }),
        ("algo", ApiOrder { algo_strategy: "Vwap".into(), ..base.clone() }),
        ("what-if", ApiOrder { what_if: true, ..base.clone() }),
    ];
    for (label, order) in cases {
        let cmd = ClientCore::build_order_request(&order, 7, 0, None)
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        let ControlCommand::Order(OrderRequest::SubmitEx { tif, attrs, .. }) = cmd else {
            panic!("{label} must route through the shared extended submission");
        };
        assert!(attrs.outside_rth, "{label} dropped outside RTH");
        assert_eq!(attrs.parent_id, 42, "{label} dropped the parent link");
        assert_eq!(attrs.oca_group_str, "bracket_1", "{label} dropped the OCA group");
        assert_eq!(tif, b'1', "{label} was submitted DAY rather than GTC");
    }
}

#[test]
fn unmodelled_risk_aversion_is_forwarded_and_known_spellings_are_folded() {
    for strategy in ["ArrivalPx", "ClosePx"] {
        for (raw, expected) in [
            ("neutral", "Neutral"), ("getdone", "Get_Done"), ("GET_DONE", "Get_Done"),
            ("aggressive", "Aggressive"), ("PASSIVE", "Passive"),
            ("Aggresive", "Aggresive"), ("Future Risk", "Future Risk"), ("", ""),
            (" passive ", " passive "),
        ] {
            let order = ApiOrder {
                action: "BUY".into(), total_quantity: 100.0,
                order_type: "LMT".into(), lmt_price: 150.0,
                algo_strategy: strategy.into(),
                algo_params: vec![
                    TagValue { tag: "riskAversion".into(), value: raw.into() },
                    TagValue { tag: "forceCompletion".into(), value: "1".into() },
                ],
                ..Default::default()
            };
            ClientCore::validate_order(&order, &crate::client_core::OrderSession::single("DU1")).unwrap();
            let cmd = ClientCore::build_order_request(&order, 7, 0, None).unwrap();
            let ControlCommand::Order(OrderRequest::SubmitEx { kind: OrderKind::Algo { algo, .. }, .. }) = cmd else {
                panic!("the caller's algorithm is carried");
            };
            match algo {
                AlgoParams::Named { strategy: name, params } => {
                    assert_eq!(name, strategy);
                    assert_eq!(params, ["riskAversion", expected, "forceCompletion", "1"]);
                }
                AlgoParams::ArrivalPx { risk_aversion, force_completion, .. }
                | AlgoParams::ClosePx { risk_aversion, force_completion, .. } => {
                    assert_eq!(risk_aversion.unwrap().as_str(), expected);
                    assert_eq!(force_completion, Some(true));
                }
                _ => panic!("the strategy stays as stated"),
            }
        }
    }
}

#[test]
fn unmodelled_algo_flags_are_forwarded_verbatim() {
    for (strategy, key) in [
        ("Vwap", "noTakeLiq"), ("Vwap", "allowPastEndTime"),
        ("Twap", "allowPastEndTime"), ("ArrivalPx", "allowPastEndTime"),
        ("ArrivalPx", "forceCompletion"), ("ClosePx", "forceCompletion"),
        ("DarkIce", "allowPastEndTime"), ("PctVol", "noTakeLiq"),
    ] {
        for raw in ["yes", "2", "", " false "] {
            let mut params = vec![TagValue { tag: key.into(), value: raw.into() }];
            if strategy == "DarkIce" {
                params.push(TagValue { tag: "displaySize".into(), value: "200".into() });
            }
            if strategy == "ArrivalPx" || strategy == "ClosePx" {
                params.push(TagValue { tag: "riskAversion".into(), value: "getdone".into() });
            }
            if strategy == "Vwap" {
                let other = if key == "noTakeLiq" { "allowPastEndTime" } else { "noTakeLiq" };
                params.push(TagValue { tag: other.into(), value: "FALSE".into() });
            }
            let order = ApiOrder {
                action: "BUY".into(), total_quantity: 100.0,
                order_type: "LMT".into(), lmt_price: 150.0,
                algo_strategy: strategy.into(), algo_params: params,
                ..Default::default()
            };
            ClientCore::validate_order(&order, &crate::client_core::OrderSession::single("DU1")).unwrap();
            let cmd = ClientCore::build_order_request(&order, 7, 0, None).unwrap();
            let ControlCommand::Order(OrderRequest::SubmitEx {
                kind: OrderKind::Algo { algo: AlgoParams::Named { strategy: name, params }, .. }, ..
            }) = cmd else {
                panic!("an unmodelled flag travels in the string parameter list");
            };
            assert_eq!(name, strategy);
            assert_eq!(&params[..2], [key, raw]);
            // One list takes one route. A value this client does not fold sends
            // the whole list down the text path, so its neighbours travel as
            // the caller wrote them too rather than half-folded.
            if strategy == "ArrivalPx" || strategy == "ClosePx" {
                assert_eq!(&params[2..], ["riskAversion", "getdone"]);
            } else if strategy == "Vwap" {
                assert_eq!(params[3], "FALSE");
            } else if strategy == "DarkIce" {
                assert_eq!(&params[2..], ["displaySize", "200"]);
            }
        }
    }
    for (raw, expected) in [("false", false), ("0", false), ("true", true), ("1", true)] {
        let algo = parse_algo_params("Vwap", &[TagValue { tag: "noTakeLiq".into(), value: raw.into() }]).unwrap();
        assert!(matches!(algo, AlgoParams::Vwap { no_take_liq: Some(value), .. } if value == expected));
    }
}

mod contract_gate_tests {
    use super::super::ClientCore;

    /// A currency pair carries no expiry, strike or right, so an order names it
    /// completely with symbol, currency, security type and destination. Options
    /// and futures do not, and an order for one would go out saying nothing
    /// about which contract it meant.
    #[test]
    fn cash_is_admitted_and_the_underspecified_types_are_not() {
        assert!(ClientCore::validate_order_contract(0, "CASH", "").is_ok(), "an FX pair is fully named");

        // A spread's legs are carried and not sent, so an order for one would
        // be an order for something else. Refused until they are encoded.
        // An instruction that is carried and not sent makes the order a
        // different one, so it is refused by name.
        use crate::types::model::Order as ApiOrder;
        let plain = ApiOrder::default();
        assert!(ClientCore::validate_supported_instructions(&plain).is_ok(), "a plain order is fine");
        // Sent now, so no longer refused.
        for (label, o) in [
            ("volatility", ApiOrder { volatility: 0.25, ..ApiOrder::default() }),
            ("volatility type", ApiOrder { volatility_type: 2, ..ApiOrder::default() }),
            ("scale", ApiOrder { scale_init_level_size: 100, scale_price_increment: 0.05,
                                 ..ApiOrder::default() }),
            ("delta neutral", ApiOrder { delta_neutral_order_type: "MKT".into(),
                                         ..ApiOrder::default() }),
            ("percent offset", ApiOrder { percent_offset: 0.5, ..ApiOrder::default() }),
            ("not held", ApiOrder { not_held: true, ..ApiOrder::default() }),
            ("open/close", ApiOrder { open_close: "O".into(), ..ApiOrder::default() }),


        ] {
            assert!(ClientCore::validate_supported_instructions(&o).is_ok(), "{label} is sent");
        }
        // Sent now, so accepted.
        for (label, o) in [
            ("hedge", ApiOrder { hedge_type: "B".into(), hedge_param: "1.5".into(),
                                 ..ApiOrder::default() }),
            ("short sale", ApiOrder { short_sale_slot: 2,
                                      designated_location: "IBKR".into(),
                                      exempt_code: 3, ..ApiOrder::default() }),
        ] {
            assert!(ClientCore::validate_supported_instructions(&o).is_ok(), "{label} is sent");
        }

        // Still refused: an instruction that cannot be acted on as given.
        for (label, mut o) in [
            ("hedge param on a kind that takes none",
             ApiOrder { hedge_type: "D".into(), hedge_param: "1.5".into(), ..ApiOrder::default() }),
            ("delta neutral with no order type",
             ApiOrder { delta_neutral_con_id: 265598, ..ApiOrder::default() }),
        ] {
            o.action = "BUY".into();
            let err = ClientCore::validate_supported_instructions(&o)
                .expect_err("{label} must be refused, not silently dropped");
            assert!(err.contains("not sent"), "{label}: {err}");
        }

        assert!(ClientCore::validate_combo_legs("STK", 0).is_ok(), "an ordinary contract has none");
        assert!(ClientCore::validate_combo_legs("BAG", 2).is_ok(), "a combination states its legs");
        assert!(ClientCore::validate_combo_legs("BAG", 0).is_err(), "a combination with none is refused");
        assert!(ClientCore::validate_order_contract(0, "cash", "").is_ok(), "and the check is case-insensitive");
        assert!(ClientCore::validate_order_contract(0, "STK", "").is_ok());
        assert!(ClientCore::validate_order_contract(0, "", "").is_ok());

        // One of a chain or one of a series has to say which one.
        for st in ["OPT", "FUT", "FOP", "WAR"] {
            assert!(
                ClientCore::validate_order_contract(0, st, "20260619|230|C|100").is_ok(),
                "{st} with an identity names one contract",
            );
            let err = ClientCore::validate_order_contract(0, st, "")
                .expect_err("and without one it names a whole chain");
            assert!(err.contains(st), "the refusal names the type: {err}");
        }
        // Everything else is named completely by its symbol and the contract id
        // and local symbol that travel with it. Requiring an expiry or a strike
        // of a kind that has neither refused it forever: an index and a crypto
        // pair could not be ordered at all.
        for st in ["IND", "CFD", "CRYPTO", "BOND", "CMDTY", "FUND"] {
            assert!(
                ClientCore::validate_order_contract(0, st, "").is_ok(),
                "{st} is named without an expiry or a strike",
            );
        }
        // A combination states its legs on the order, so it needs no identity
        // here. Stating none at all is refused by the leg check instead.
        for st in ["BAG", "COMBO"] {
            assert!(ClientCore::validate_order_contract(0, st, "").is_ok(), "{st} names its legs");
            assert!(ClientCore::validate_combo_legs(st, 0).is_err(), "{st} with no legs");
            assert!(ClientCore::validate_combo_legs(st, 2).is_ok(), "{st} with legs");
        }
    }

}
mod exchange_mask_provenance_tests {

    /// Two contracts a caller would call different have to look different
    /// here, or an order on one is sent under the other's id.
    #[test]
    fn a_description_names_one_contract_and_no_other() {
        use crate::types::model::Contract as ApiContract;
        use super::super::ClientCore;
        let spy = |exchange: &str| ApiContract {
            symbol: "SPY".into(), sec_type: "STK".into(), exchange: exchange.into(),
            currency: "USD".into(), ..Default::default()
        };
        let core = ClientCore::new();
        let key = ClientCore::description_key(&spy("SMART"));
        assert!(core.named_for(&key).is_none(), "nothing is known before the venue answers");

        let mut answered = spy("SMART");
        answered.con_id = 756733;
        core.remember_named(key.clone(), answered);
        assert_eq!(core.named_for(&key).map(|c| c.con_id), Some(756733));

        // The same symbol somewhere else is a different contract, and asking
        // under it must not find the first one.
        assert!(core.named_for(&ClientCore::description_key(&spy("ARCA"))).is_none());

        // So is the same symbol in another currency, which the identity carries.
        let mut abroad = spy("SMART");
        abroad.currency = "EUR".into();
        assert!(core.named_for(&ClientCore::description_key(&abroad)).is_none());

        // And a description that stated no currency at all is its own. The
        // identity folds "" and USD together, which is right for the slot an
        // order goes through: here it would let a lookup answered with a
        // listing in another currency satisfy an order that asked for USD.
        let mut unstated = spy("SMART");
        unstated.currency = String::new();
        assert!(
            core.named_for(&ClientCore::description_key(&unstated)).is_none(),
            "saying nothing about the currency is not the same as saying USD",
        );
    }
}

/// A halt changes what every other tick in a quote means: the prices standing
/// are the ones from before the venue stopped, not a market anyone can deal
/// on. It arrives on the trading-status tick and is written into the quote and
/// compared against the last one. Caching it without emitting a tick consumes
/// the transition, and it cannot be delivered afterwards.
#[test]
fn a_halt_the_venue_states_reaches_the_caller() {
    let core = ClientCore::new();
    let shared = SharedState::new();

    let trading = Quote { last: (735.00 * PRICE_SCALE_F) as i64, ..Default::default() };
    shared.market.push_quote(0, &trading);
    let first = core.poll_instrument_ticks(&shared, 0, 11);
    assert!(
        first.generic_ticks.is_empty(),
        "a contract that has not stopped states no halt",
    );

    shared.market.push_quote(0, &Quote { halted: 1, ..trading });
    let halted = core.poll_instrument_ticks(&shared, 0, 11);
    let tick = halted.generic_ticks.first().expect("the halt is delivered");
    assert_eq!(tick.tick_type, TICK_HALTED);
    assert_eq!(tick.value, 1.0);
    assert_eq!(tick.req_id, 11);
    assert!(!tick.is_price, "a halt is not a price");

    // And it is not repeated while nothing about it has changed.
    assert!(
        core.poll_instrument_ticks(&shared, 0, 11).generic_ticks.is_empty(),
        "a halt that is still standing is not restated",
    );

    // Trading resumes, and that is a transition too.
    shared.market.push_quote(0, &Quote { halted: 0, ..trading });
    let resumed = core.poll_instrument_ticks(&shared, 0, 11);
    assert_eq!(resumed.generic_ticks.first().expect("the resume is delivered").value, 0.0);
}

/// The summary reports what the venue stated, under the venue's names.
/// Matched against a list of sixteen names kept here instead, "All" — the
/// the venue's word for every figure it holds — matched none of them and came
/// back empty, and the figures that were not on that list went with it.
#[test]
fn an_account_summary_reports_every_figure_the_venue_stated() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    for (key, value, currency) in [
        ("NetLiquidation", "75425.51", "USD"),
        ("AccruedCash", "12.40", "USD"),
        ("SMA", "38000.00", "USD"),
        ("FullInitMarginReq", "1364.01", "USD"),
        ("TotalCashValue", "5000.00", "EUR"),
    ] {
        shared.portfolio.note_account_value(key, value, currency);
    }
    // The venue has finished stating the account, which is what the reads
    // below wait on: answered on the first figure instead, a summary asked for
    // right after connecting was handed the few tags parsed so far.
    shared.portfolio.holdings_restated_under("AR.1");
    shared.portfolio.set_account_download_complete("AR.1");
    shared.portfolio.account_download_is_settled();

    core.subscribe_account_summary(3, "All", vec![String::new()]).unwrap();
    let batch = core.prepare_account_summary(&shared, "DU1").expect("a summary");
    assert_eq!(batch.req_id, 3);
    let names: Vec<&str> = batch.entries.iter().map(|e| e.tag.as_str()).collect();
    for stated in ["NetLiquidation", "AccruedCash", "SMA", "FullInitMarginReq"] {
        assert!(names.contains(&stated), "{stated} missing from {names:?}");
    }

    // A figure stated in more than one currency is stated in each of them.
    core.unsubscribe_account_summary(3);
    core.subscribe_account_summary(4, "TotalCashValue", vec![String::new()]).unwrap();
    let batch = core.prepare_account_summary(&shared, "DU1").expect("a summary");
    assert_eq!(batch.entries.len(), 1);
    assert_eq!(batch.entries[0].tag, "TotalCashValue");
    assert_eq!(batch.entries[0].currency, "EUR");

    // And a tag the venue never stated reports nothing rather than a zero.
    core.unsubscribe_account_summary(4);
    core.subscribe_account_summary(5, "Cushion", vec![String::new()]).unwrap();
    let batch = core.prepare_account_summary(&shared, "DU1").expect("a summary");
    assert!(batch.entries.is_empty(), "{:?}", batch.entries.len());
}

/// An account summary is refused where a gateway refuses one, in its order
/// and its words: no tags, no group, a group a login that is not an advisor's
/// does not have, and `All` where the venue says the login may not ask for it.
#[test]
fn an_account_summary_is_refused_where_a_gateway_refuses_one() {
    let check = |shared: &SharedState, group: &str, tags: &str| {
        ClientCore::check_account_summary(shared, group, tags).map_err(|r| (r.code, r.message))
    };
    let shared = SharedState::new();
    assert_eq!(check(&shared, "", ""), Err((321, "Tags cannot be null".into())));
    assert_eq!(check(&shared, "", "NetLiquidation"), Err((321, "Group name cannot be null".into())));
    for group in ["Nope", "all"] {
        assert_eq!(
            check(&shared, group, "NetLiquidation"),
            Err((321, "Group name is invalid".into())),
            "{group}",
        );
    }
    assert_eq!(check(&shared, "All", "NetLiquidation"), Ok(()));
    assert_eq!(check(&shared, "AllNonProp", "NetLiquidation"), Ok(()));

    shared.reference.set_enabled_features(vec!["NOALL".into()]);
    assert_eq!(check(&shared, "All", "NetLiquidation"), Err((321, "ALL account is not supported".into())));
    shared.reference.set_advisor(true);
    assert_eq!(check(&shared, "All", "NetLiquidation"), Ok(()), "an advisor may ask for every account");
    assert_eq!(check(&shared, "MyGroup", "NetLiquidation"), Ok(()), "an advisor's groups are its own");
    shared.reference.set_advisor(false);
    shared.reference.set_enabled_features(vec!["NOALL".into(), "APIREQALLPOS".into()]);
    assert_eq!(check(&shared, "All", "NetLiquidation"), Ok(()));

    shared.reference.set_enabled_features(vec!["DYNACCTADD".into()]);
    assert_eq!(
        check(&shared, "All", "NetLiquidation"),
        Err((10200, "This API request for All is not supported for Dynamic Account Addition".into())),
    );
}

/// The account `reqAccountUpdates` names is checked as a gateway checks it:
/// ignored on a login holding one, and on one holding several, one it holds,
/// `All` where the login may ask for every account, or `AllNonProp` where the
/// venue offers it and the logon names accounts it leaves out.
#[test]
fn an_account_update_is_refused_where_a_gateway_refuses_one() {
    let check = |shared: &SharedState, accounts: &[&str], code: &str| {
        let accounts: Vec<String> = accounts.iter().map(|a| a.to_string()).collect();
        // What the logon names, as a real session holds it.
        shared.reference.set_login(accounts.clone(), shared.reference.advisor());
        ClientCore::check_account_updates(shared, &accounts, true, code).map_err(|r| (r.code, r.message))
    };
    let shared = SharedState::new();
    assert_eq!(check(&shared, &["DU1"], "U9"), Ok(()), "one account: the code is ignored");
    let several = ["DU1", "DU2"];
    assert_eq!(check(&shared, &several, "DU2"), Ok(()));
    assert_eq!(check(&shared, &several, "All"), Ok(()), "every account, as a gateway takes it");
    assert_eq!(check(&shared, &several, "all"), Err((321, "Invalid account code 'all'.".into())));
    assert_eq!(
        check(&shared, &several, ""),
        Err((321, "The account code is required for this operation.".into())),
    );
    assert_eq!(
        check(&shared, &several, "AllNonProp"),
        Err((321, "Invalid account code 'AllNonProp'.".into())),
        "not offered",
    );
    shared.reference.set_enabled_features(vec!["ALLNONPROP".into()]);
    assert_eq!(
        check(&shared, &several, "AllNonProp"),
        Err((321, "Invalid account code 'AllNonProp'.".into())),
        "offered, and the logon names no account it leaves out",
    );
    shared.reference.set_all_non_prop_leaves_out(true);
    assert_eq!(check(&shared, &several, "AllNonProp"), Ok(()));

    shared.reference.set_enabled_features(vec!["NOALL".into()]);
    assert_eq!(check(&shared, &several, "All"), Err((321, "ALL account is not supported".into())));
    shared.reference.set_advisor(true);
    assert_eq!(check(&shared, &several, "All"), Ok(()), "an advisor may ask for every account");
    shared.reference.set_advisor(false);

    // A login the venue adds accounts to holds several whatever the logon
    // named, and takes any account unless the venue says the API may not.
    shared.reference.set_enabled_features(vec!["DYNACCTADD".into()]);
    assert_eq!(
        check(&shared, &["DU1"], ""),
        Err((321, "The account code is required for this operation.".into())),
    );
    assert_eq!(check(&shared, &["DU1"], "DU7"), Ok(()));
    shared.reference.set_enabled_features(vec!["DYNACCTADD".into(), "NOAPIDYNADD".into()]);
    assert_eq!(check(&shared, &["DU1"], "DU7"), Err((321, "Invalid account code 'DU7'.".into())));
}

/// An execution filter's account is ignored on a login holding one, and on
/// one holding several — which a login the venue adds accounts to is — one
/// the login does not hold is refused unless the venue takes accounts added
/// later.
#[test]
fn an_execution_filters_account_is_refused_where_a_gateway_refuses_it() {
    let check = |shared: &SharedState, code: &str| {
        let mut filter = ExecutionFilter { acct_code: code.into(), ..Default::default() };
        ClientCore::check_execution_account(shared, &["DU1".to_string()], &mut filter)
            .map(|()| filter.acct_code)
            .map_err(|r| (r.code, r.message))
    };
    let shared = SharedState::new();
    assert_eq!(check(&shared, "DU7"), Ok(String::new()), "one account: ignored, and cleared");
    shared.reference.set_enabled_features(vec!["DYNACCTADD".into()]);
    assert_eq!(check(&shared, "DU7"), Ok("DU7".into()), "taken, and kept to filter by");
    shared.reference.set_enabled_features(vec!["DYNACCTADD".into(), "NOAPIDYNADD".into()]);
    assert_eq!(check(&shared, "DU7"), Err((321, "Invalid account code DU7.".into())));
    assert_eq!(check(&shared, "DU1"), Ok("DU1".into()));
}

/// The account a P&L request names is checked in a gateway's order and words
/// — blank, not held, `All` where the login may not ask for every account or
/// is one the venue adds accounts to — before the request is subscribed.
#[test]
fn a_profit_request_is_refused_in_a_gateways_order() {
    let check = |shared: &SharedState, accounts: &[&str], account: &str| {
        let accounts: Vec<String> = accounts.iter().map(|a| a.to_string()).collect();
        // What the logon names, as a real session holds it.
        shared.reference.set_login(accounts.clone(), shared.reference.advisor());
        ClientCore::check_pnl_account(shared, &accounts, "DU1", account).map_err(|r| (r.code, r.message))
    };
    let shared = SharedState::new();
    assert_eq!(check(&shared, &["DU1"], "DU1"), Ok(()));
    assert_eq!(check(&shared, &["DU1"], " "), Err((321, "Account must not be empty".into())));
    assert_eq!(check(&shared, &["DU1"], "All"), Err((321, "Invalid account code".into())), "one account");
    let several = ["DU1", "DU2"];
    let ours = |_: &str| -> Result<(), (i32, String)> { Ok(()) };
    assert_eq!(check(&shared, &several, "all"), ours("all"), "a gateway takes every account");
    assert_eq!(check(&shared, &several, "DU2"), ours("DU2"));

    shared.reference.set_enabled_features(vec!["NOALL".into()]);
    assert_eq!(check(&shared, &several, "All"), Err((321, "Invalid account code".into())));
    shared.reference.set_advisor(true);
    assert_eq!(check(&shared, &several, "All"), ours("All"));
    shared.reference.set_advisor(false);

    shared.reference.set_enabled_features(vec!["DYNACCTADD".into()]);
    assert_eq!(
        check(&shared, &["DU1"], "All"),
        Err((321, "This API request for All is not supported for Dynamic Account Addition".into())),
    );
    assert_eq!(check(&shared, &["DU1"], "DU7"), ours("DU7"), "any account passes a gateway's check here");
}

/// Two summaries at once is a gateway's limit, and a third is refused under
/// the number and in the words a gateway uses.
#[test]
fn a_third_account_summary_is_refused_as_a_gateway_refuses_it() {
    let core = ClientCore::new();
    core.subscribe_account_summary(3, "NetLiquidation", vec![String::new()]).unwrap();
    core.subscribe_account_summary(4, "NetLiquidation", vec![String::new()]).unwrap();
    let refused = core.subscribe_account_summary(5, "NetLiquidation", vec![String::new()]).unwrap_err();
    assert_eq!(refused.code, 322);
    assert_eq!(
        refused.message,
        "Maximum number of account summary requests exceeded; desubscribe to previous request first",
    );
}

/// Each profit request keeps its own subscription; only a reused number is refused.
#[test]
fn profit_requests_coexist_and_only_a_repeated_number_is_refused() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl(7, "").unwrap();
    core.subscribe_pnl(8, "").unwrap();
    assert_eq!(core.subscribe_pnl(7, "").unwrap_err().code, 102);
    assert_eq!(core.poll_pnl(&shared).iter().map(|u| u.req_id).collect::<Vec<_>>(), [7, 8]);
    assert!(core.poll_pnl(&shared).is_empty());
    core.unsubscribe_pnl(7);
    shared.portfolio.set_account(&crate::types::AccountState { daily_pnl: PRICE_SCALE, ..Default::default() });
    assert_eq!(core.poll_pnl(&shared).iter().map(|u| u.req_id).collect::<Vec<_>>(), [8]);
}

#[test]
fn two_account_summaries_receive_their_own_requested_values() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.note_account_value("NetLiquidation", "75425.51", "USD");
    shared.portfolio.note_account_value("TotalCashValue", "5000.00", "EUR");
    shared.portfolio.account_download_is_settled();
    core.subscribe_account_summary(3, "NetLiquidation", vec![String::new()]).unwrap();
    core.subscribe_account_summary(4, "TotalCashValue", vec![String::new()]).unwrap();
    assert!(core.subscribe_account_summary(5, "All", vec![String::new()]).is_err(), "two is the venue's limit");

    for (req_id, tag, value, currency) in [
        (3, "NetLiquidation", "75425.51", "USD"),
        (4, "TotalCashValue", "5000.00", "EUR"),
    ] {
        let batch = core.prepare_account_summary(&shared, "DU1").expect("both requests receive");
        assert_eq!(batch.req_id, req_id);
        assert_eq!(batch.entries.len(), 1);
        let entry = &batch.entries[0];
        assert_eq!((entry.tag.as_str(), entry.value.as_str(), entry.currency.as_str()), (tag, value, currency));
    }
    assert!(core.prepare_account_summary(&shared, "DU1").is_none());
    shared.portfolio.note_account_value("TotalCashValue", "5100.00", "EUR");
    for (when, _) in core.last_account_summary.lock().unwrap().values_mut() {
        *when -= std::time::Duration::from_secs(180);
    }
    let batch = core.prepare_account_summary(&shared, "DU1").expect("the second subscription keeps receiving too");
    assert_eq!(batch.req_id, 4);
    assert_eq!(batch.entries[0].value, "5100.00");
    core.unsubscribe_account_summary(99);
    assert!(core.subscribe_account_summary(5, "All", vec![String::new()]).is_err(), "the initial batches leave both subscribed");
    core.unsubscribe_account_summary(3);
    core.subscribe_account_summary(4, "NetLiquidation", vec![String::new()]).unwrap();
    let batch = core.prepare_account_summary(&shared, "DU1").unwrap();
    assert_eq!(batch.req_id, 4, "reusing a request replaces its tags, not the other subscription");
    assert_eq!(batch.entries[0].tag, "NetLiquidation");
    core.subscribe_account_summary(5, "All", vec![String::new()]).unwrap();
    core.reset();
    assert!(core.prepare_account_summary(&shared, "DU1").is_none());
    core.subscribe_account_summary(3, "All", vec![String::new()]).unwrap();
    core.subscribe_account_summary(4, "All", vec![String::new()]).unwrap();
    assert!(core.prepare_account_summary(&shared, "DU1").is_some(), "reset forgets the last delivery too");
}

#[test]
fn an_account_summary_reports_changes_until_cancelled() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.note_account_value("TotalCashValue", "5000.00", "EUR");
    shared.portfolio.note_account_value("TotalCashValue", "1000.00", "USD");
    shared.portfolio.account_download_is_settled();
    core.subscribe_account_summary(3, "TotalCashValue", vec![String::new()]).unwrap();
    assert_eq!(core.prepare_account_summary(&shared, "DU1").unwrap().entries.len(), 2);

    shared.portfolio.note_account_value("TotalCashValue", "5100.00", "EUR");
    assert!(core.prepare_account_summary(&shared, "DU1").is_none(), "updates wait three minutes");
    core.last_account_summary.lock().unwrap().get_mut(&3).unwrap().0 -= std::time::Duration::from_secs(180);
    let batch = core.prepare_account_summary(&shared, "DU1").expect("the subscription still receives");
    assert_eq!(batch.req_id, 3);
    assert_eq!(batch.entries.len(), 1, "only the currency whose value changed");
    assert_eq!(batch.entries[0].value, "5100.00");
    assert_eq!(batch.entries[0].currency, "EUR");

    core.last_account_summary.lock().unwrap().get_mut(&3).unwrap().0 -= std::time::Duration::from_secs(180);
    assert!(core.prepare_account_summary(&shared, "DU1").is_none(), "unchanged figures do not repeat");
    shared.portfolio.note_account_value("TotalCashValue", "5200.00", "EUR");
    core.last_account_summary.lock().unwrap().get_mut(&3).unwrap().0 -= std::time::Duration::from_secs(180);
    assert_eq!(core.prepare_account_summary(&shared, "DU1").unwrap().entries[0].value, "5200.00");

    core.unsubscribe_account_summary(3);
    shared.portfolio.note_account_value("TotalCashValue", "5300.00", "EUR");
    assert!(core.prepare_account_summary(&shared, "DU1").is_none(), "cancellation ends the updates");
    core.subscribe_account_summary(3, "TotalCashValue", vec![String::new()]).unwrap();
    assert_eq!(core.prepare_account_summary(&shared, "DU1").unwrap().entries.len(), 2, "a new subscription gets the full batch");
}

/// `$LEDGER` is the venue's word for the per-currency cash rows rather than the
/// name of a figure. Matched as a literal name it matched none of them, and the
/// standard way to read per-currency cash, exchange rate and per-currency profit
/// came back as an end with no rows at all.
#[test]
fn a_ledger_tag_answers_with_the_currency_bucket_it_names() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    for (key, value, currency) in [
        ("CashBalance", "5000.00", "EUR"),
        ("ExchangeRate", "1.08", "EUR"),
        ("CashBalance", "7500.00", "BASE"),
        ("NetLiquidation", "75425.51", "USD"),
    ] {
        shared.portfolio.note_account_value(key, value, currency);
    }
    shared.portfolio.account_download_is_settled();

    core.subscribe_account_summary(3, "$LEDGER", vec![String::new()]).unwrap();
    let batch = core.prepare_account_summary(&shared, "DU1").expect("a ledger request is answered");
    assert_eq!(
        batch.entries.iter().map(|e| (e.tag.as_str(), e.currency.as_str())).collect::<Vec<_>>(),
        [("CashBalance", "BASE")],
        "naming no currency asks for the base bucket",
    );
    core.unsubscribe_account_summary(3);

    core.subscribe_account_summary(4, "$LEDGER:EUR", vec![String::new()]).unwrap();
    let batch = core.prepare_account_summary(&shared, "DU1").expect("a ledger request is answered");
    assert_eq!(
        batch.entries.iter().map(|e| (e.tag.as_str(), e.value.as_str())).collect::<Vec<_>>(),
        [("CashBalance", "5000.00"), ("ExchangeRate", "1.08")],
        "every ledger figure the venue stated in the currency named",
    );
    core.unsubscribe_account_summary(4);

    core.subscribe_account_summary(5, "$LEDGER:ALL", vec![String::new()]).unwrap();
    let batch = core.prepare_account_summary(&shared, "DU1").expect("a ledger request is answered");
    assert_eq!(
        batch.entries.iter().map(|e| (e.tag.as_str(), e.currency.as_str())).collect::<Vec<_>>(),
        [("CashBalance", "EUR"), ("ExchangeRate", "EUR"), ("CashBalance", "BASE")],
        "ALL is every currency, and the account's own figures are not ledger rows",
    );
}

/// A quote is per unit and a contract may be worth many of them. Valued from
/// the price alone, an option holding came out at a hundredth of what it is
/// worth and the account total with it, so such a position goes to the venue's
/// own figures instead.
#[test]
fn an_option_holding_is_not_valued_from_a_per_unit_price() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_pnl(41, "").unwrap();

    seed_pnl_position(&core, &shared, 7001, 0, 2.0, 3.00, 4.00, 3.00);
    shared.portfolio.set_position_info(PositionInfo {
        con_id: 7001,
        position: 2.0,
        avg_cost: (300.0 * PRICE_SCALE_F) as i64,
        symbol: "SPY   260320C00500000".into(),
        sec_type: "OPT".into(),
        currency: "USD".into(),
        multiplier: "100".into(),
        ..Default::default()
    });
    shared.portfolio.set_account(&AccountState {
        daily_pnl: (200.0 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (200.0 * PRICE_SCALE_F) as i64,
        ..Default::default()
    });
    shared.portfolio.account_download_is_settled();

    let update = core.poll_pnl(&shared).into_iter().next().expect("callback must fire");
    assert!((update.daily_pnl - 200.0).abs() < 1e-6,
        "the venue's total, not two dollars of per-unit move, daily={}",
        update.daily_pnl);
}

/// This subscription does not depend on a market-data one. Answered only from
/// a live quote, a caller who never asked for market data heard nothing at all
/// and was told nothing either, though the venue states its own mark for every
/// position it reports.
#[test]
fn a_position_pnl_is_answered_without_a_market_data_subscription() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    // A book the download has stated whole, which is the only book a profit
    // is worked out from.
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl_single(9, 8001, "").unwrap();

    // No entry in con_id_to_instrument: nothing here subscribed to quotes.
    shared.portfolio.set_position_info(PositionInfo {
        con_id: 8001,
        position: 10.0,
        avg_cost: (100.0 * PRICE_SCALE_F) as i64,
        symbol: "SYM8001".into(),
        sec_type: "STK".into(),
        currency: "USD".into(),
        multiplier: String::new(),
        market_price: (105.0 * PRICE_SCALE_F) as i64,
        market_price_stated: true,
        market_value: (1050.0 * PRICE_SCALE_F) as i64,
        unrealized_pnl: (50.0 * PRICE_SCALE_F) as i64,
        unrealized_stated: true,
        realized_pnl: 0,
    });
    shared.portfolio.set_midnight_seeds(String::new(), vec![MidnightSeed {
        con_id: 8001,
        qty_midnight: Some(10.0),
        cost_midnight: Some(1000.0),
        qty_traded: Some(0.0),
        money_traded: 0.0,
        realized_pnl: 0.0,
    }]);

    let updates = core.poll_pnl_single(&shared);
    let update = updates.first().expect("the venue's mark answers it");
    assert_eq!(update.req_id, 9);
    assert!((update.pos - 10.0).abs() < 1e-6);
    assert!((update.value - 1050.0).abs() < 1e-6, "value={}", update.value);
    assert!((update.unrealized_pnl - 50.0).abs() < 1e-6, "unreal={}", update.unrealized_pnl);
    assert!((update.daily_pnl - 50.0).abs() < 1e-6, "daily={}", update.daily_pnl);
}

#[test]
fn news_is_asked_for_from_the_providers_the_logon_named() {
    let (tx, rx) = std::sync::mpsc::channel();
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.reference.set_news_providers(vec![
        crate::types::NewsProvider { code: "DJNL".into(), name: "Dow Jones".into() },
        crate::types::NewsProvider { code: "BRFUPDN".into(), name: "Briefing".into() },
    ]);

    // Every provider named on this attempt, and nothing if news was not asked
    // for: they ride the request, and the engine asks the venue for them.
    let next = std::cell::Cell::new(265598i64);
    let asked = |tick_list: &str| -> Option<String> {
        let con_id = next.get();
        next.set(con_id + 1);
        let _ = core.register_mkt_data(
            &shared, &tx, con_id, con_id, "SPY", "SMART", "STK", "USD", &Default::default(),
            false, false, tick_list, 0, None, None, false,
        );
        let mut named = None;
        while let Ok(cmd) = rx.try_recv() {
            if let ControlCommand::Subscribe { news, .. } = cmd {
                named = news;
            }
        }
        named
    };

    assert_eq!(asked("1292"), None, "1292 is not 292");
    assert_eq!(asked("100,101"), None, "nor is anything else");
    assert_eq!(
        asked("292").as_deref(), Some("DJNL*BRFUPDN"),
        "the providers the logon named",
    );

    // A caller naming its own set overrides that; emptying it returns to the
    // logon's answer.
    core.set_news_providers("BRFG");
    assert_eq!(asked("292").as_deref(), Some("BRFG"));
    core.set_news_providers("");
    assert_eq!(asked("292").as_deref(), Some("DJNL*BRFUPDN"));

    // An entry naming its own providers asks those, whatever the session
    // names, and joined the way the venue separates them.
    assert_eq!(asked("mdoff,292:BRFG+DJNL").as_deref(), Some("BRFG*DJNL"));
    core.set_news_providers("DJNL");
    assert_eq!(asked("292:BRFUPDN").as_deref(), Some("BRFUPDN"));
    assert_eq!(asked("292:").as_deref(), Some("DJNL"), "an entry naming none takes the session's");
}

/// A generic tick list read one whole entry at a time.
///
/// Only a bare `292` was read as the headlines. `292:BRFG+DJNL`, the form that
/// names the providers to ask, was read as a series that is not a number, so
/// the caller was warned about a word it had written correctly and no
/// headlines were asked for.
#[test]
fn a_tick_list_names_its_headlines_with_or_without_their_providers() {
    assert_eq!(
        parse_generic_tick_list(" mdoff, 292:BRFG+DJNL ,233,1292,233,x"),
        GenericTicks {
            news: true,
            news_providers: "BRFG*DJNL".into(),
            series: vec![233, 1292],
            unread: vec!["x"],
        },
    );
    let bare = parse_generic_tick_list("292");
    assert!(bare.news && bare.news_providers.is_empty() && bare.series.is_empty());
    assert!(!parse_generic_tick_list("2920").news, "2920 is a series of its own");
    assert_eq!(parse_generic_tick_list("2920").series, vec![2920]);
    assert_eq!(parse_generic_tick_list(""), GenericTicks::default());
}

/// A mask with bits set and no letters to show for them is one the venue has
/// not named its exchanges for yet. Caching it as delivered leaves it equal to
/// the next mask, so it is never rendered again once the names arrive and the
/// quote's exchange is lost for the life of the subscription.
#[test]
fn an_exchange_mask_is_rendered_once_the_venue_names_its_bits() {
    let core = ClientCore::new();
    let shared = SharedState::new();

    shared.market.push_quote(0, &Quote {
        bid: (150.0 * PRICE_SCALE_F) as i64,
        bid_exch_mask: 0b101,
        ..Default::default()
    });
    let before = core.poll_instrument_ticks(&shared, 0, 5);
    assert!(
        !before.string_ticks.iter().any(|t| t.tick_type == TICK_BID_EXCHANGE),
        "nothing names those bits yet, so nothing is stated about them",
    );

    shared.reference.set_smart_components_of(0, "STK", vec![
        SmartComponent { bit_number: 0, exchange: "ARCA".into(), exchange_letter: "P".into() },
        SmartComponent { bit_number: 2, exchange: "NASDAQ".into(), exchange_letter: "Q".into() },
    ]);
    let after = core.poll_instrument_ticks(&shared, 0, 5);
    let rendered = after.string_ticks.iter()
        .find(|t| t.tick_type == TICK_BID_EXCHANGE)
        .expect("the same mask is rendered once its bits are named");
    assert_eq!(rendered.value, "PQ");
}

/// An adjustable stop states what the contract states, not only what the
/// order states.
///
/// Its legs, its listing exchange and the contract it hedges against are
/// stated on the contract rather than on the order, and every other order type
/// picks them up on the way through. Built from the order's own attributes
/// alone, an adjustable stop on a combination reached the encoder with no legs
/// at all.
#[test]
fn an_adjustable_stop_carries_what_the_contract_states() {
    let order = ApiOrder {
        action: "SELL".into(),
        total_quantity: 1.0,
        order_type: "STP".into(),
        aux_price: 11.0,
        tif: "DAY".into(),
        adjusted_order_type: "TRAIL".into(),
        adjusted_stop_price: 11.5,
        trigger_price: 12.0,
        ..Default::default()
    };
    let contract = crate::types::model::Contract {
        symbol: "SPX".into(),
        sec_type: "BAG".into(),
        exchange: "SMART".into(),
        currency: "USD".into(),
        primary_exchange: "CBOE".into(),
        combo_legs: vec![
            crate::types::model::ComboLeg {
                con_id: 111, ratio: 1, action: "BUY".into(), exchange: "SMART".into(),
                ..Default::default()
            },
            crate::types::model::ComboLeg {
                con_id: 222, ratio: 1, action: "SELL".into(), exchange: "SMART".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let cmd = ClientCore::build_order_request(&order, 7, 0, Some(&contract)).unwrap();
    let ControlCommand::Order(OrderRequest::SubmitEx { attrs, .. }) = cmd else {
        panic!("an adjustable stop routes through the shared extended submission");
    };
    assert_eq!(attrs.combo_legs.len(), 2, "the legs the contract named");
    assert_eq!(attrs.combo_legs[0].con_id, 111);
    assert_eq!(attrs.combo_legs[1].con_id, 222);
    assert_eq!(attrs.primary_exchange, "CBOE", "the listing exchange it names");
}

/// A price stated for a leg the combination does not have is refused.
///
/// The prices travel as their own list, in leg order, and are put on the legs
/// one by one. Two lists paired the wrong way round ran past each other: the
/// extra prices were dropped without a word and the order went out priced on
/// whichever legs happened to line up, reported back to the caller as placed
/// exactly as they had written it.
#[test]
fn a_price_for_a_leg_the_combination_does_not_have_is_refused() {
    let leg = |con_id: i64, action: &str| crate::types::model::ComboLeg {
        con_id, ratio: 1, action: action.into(), exchange: "SMART".into(),
        ..Default::default()
    };
    let contract = crate::types::model::Contract {
        symbol: "SPX".into(), sec_type: "BAG".into(), exchange: "SMART".into(),
        currency: "USD".into(), combo_legs: vec![leg(111, "BUY"), leg(222, "SELL")],
        ..Default::default()
    };
    let priced = |prices: Vec<f64>| ApiOrder {
        action: "BUY".into(), total_quantity: 1.0, order_type: "LMT".into(),
        lmt_price: 0.0, tif: "DAY".into(), order_combo_legs: prices, ..Default::default()
    };

    let why = ClientCore::build_order_request(&priced(vec![1.0, 2.0, 3.0]), 7, 0, Some(&contract))
        .expect_err("a third price on a two-legged combination has nowhere to go");
    assert!(why.message.contains("3 legs"), "{why}");
    assert!(why.message.contains("has 2"), "{why}");

    // A contract with no legs at all states nothing with this list, and the
    // reference client sends none for one either.
    assert!(
        ClientCore::build_order_request(&priced(vec![1.0, 2.0]), 7, 0, None).is_ok(),
        "a price list on a contract that is not a combination is not a refusal",
    );
}

/// Prices stated for a combination's legs are answered as a gateway answers
/// them: leg by leg as it reads them, then the combination's own price, then
/// whether it is a combination a gateway prices by its legs at all — which,
/// with no routing parameter to make it non-guaranteed, none placed here is.
/// Priced on later legs alone, the prices are read and not sent.
#[test]
fn prices_stated_for_the_legs_are_answered_as_a_gateway_answers_them() {
    let leg = |con_id: i64| crate::types::model::ComboLeg {
        con_id, ratio: 1, action: "BUY".into(), exchange: "SMART".into(),
        ..Default::default()
    };
    let contract = crate::types::model::Contract {
        symbol: "SPX".into(), sec_type: "BAG".into(), exchange: "SMART".into(),
        currency: "USD".into(), combo_legs: vec![leg(111), leg(222)],
        ..Default::default()
    };
    let answered = |order_type: &str, lmt_price: f64, prices: Vec<f64>| {
        let order = ApiOrder {
            action: "BUY".into(), total_quantity: 1.0, order_type: order_type.into(),
            lmt_price, tif: "DAY".into(), order_combo_legs: prices, ..Default::default()
        };
        ClientCore::build_order_request(&order, 7, 0, Some(&contract))
    };
    let refused = |order_type: &str, lmt_price: f64, prices: Vec<f64>| {
        let why = answered(order_type, lmt_price, prices).expect_err("refused");
        (why.code, why.message)
    };
    let leg_invalid = |at: usize, code: i32, text: &str| (321, format!(
        "The combo details for leg '{at}' are invalid. - CodeMsgPair::[m_code={code}m_msg={text}]m_sysMsg={text}]",
    ));
    let only_limit = "Only LMT or REL+LMT order allows using per-leg prices.";
    let all_needed = "All leg prices are needed when specifying per-leg prices.";
    assert_eq!(refused("MKT", 0.0, vec![1.0, 2.0]), leg_invalid(0, 10055, only_limit));
    assert_eq!(refused("MKT", 0.0, vec![f64::MAX, 2.0]), leg_invalid(1, 10055, only_limit));
    assert_eq!(refused("LMT", 0.0, vec![1.0]), leg_invalid(1, 10056, all_needed));
    assert_eq!(refused("LMT", 0.0, vec![1.0, f64::MAX]), leg_invalid(1, 10056, all_needed));
    assert_eq!(
        refused("LMT", 3.0, vec![1.0, 2.0]),
        (10054, "Can't specify combo price when using per-leg prices.".to_string()),
    );
    assert_eq!(
        refused("LMT", 0.0, vec![1.0, 2.0]),
        (10058, "Combo per-leg prices are only supported for non-guaranteed smart combo with \
                 two legs and feature \"IECOMBOPERLEGPRICE\" enabled.".to_string()),
    );
    match answered("LMT", 1.5, vec![f64::MAX, 2.0]) {
        Ok(ControlCommand::Order(OrderRequest::SubmitEx { kind: OrderKind::Limit { price }, attrs, .. })) => {
            assert_eq!(price, 3 * PRICE_SCALE / 2, "the combination's own price");
            assert!(attrs.combo_legs.iter().all(|l| l.price.is_none()), "no leg's price: {:?}", attrs.combo_legs);
        }
        other => panic!("placed at its own price: {other:?}"),
    }
}

/// A discretionary amount below nought is refused as a gateway refuses it.
/// Taken, it was dropped on the way out and the order went without the
/// discretion the caller named, reported as placed.
#[test]
fn a_discretionary_amount_below_nought_is_refused_under_168() {
    let order = ApiOrder {
        action: "BUY".into(), total_quantity: 1.0, order_type: "LMT".into(),
        lmt_price: 1.0, tif: "DAY".into(), discretionary_amt: -0.01, ..Default::default()
    };
    let session = crate::client_core::OrderSession::single("DU1");
    let why = ClientCore::validate_order(&order, &session).expect_err("no such amount");
    assert_eq!(why.code, 168);
    assert_eq!(
        why.message,
        "Discretionary amount does not conform to the minimum price variation for this contract",
    );
    let nought = ApiOrder { discretionary_amt: 0.0, ..order };
    assert!(ClientCore::validate_order(&nought, &session).is_ok(), "nought is no discretion");
}

/// A passive relative order is taken as the caller states it: the offset on
/// the auxiliary price, the cap on the limit price, which is the pair the
/// venue's own shape for the type is built from.
#[test]
fn a_passive_relative_order_is_built_from_the_prices_it_states() {
    let scale = crate::types::PRICE_SCALE;
    let order = ApiOrder {
        action: "BUY".into(),
        total_quantity: 100.0,
        order_type: "PASSV REL".into(),
        aux_price: 0.01,
        lmt_price: 150.0,
        tif: "DAY".into(),
        ..Default::default()
    };
    ClientCore::validate_order(&order, &crate::client_core::OrderSession::single("DU123")).unwrap();
    let cmd = ClientCore::build_order_request(&order, 7, 0, None).unwrap();
    let ControlCommand::Order(OrderRequest::SubmitEx { kind, .. }) = cmd else {
        panic!("a passive relative order routes through the shared extended submission");
    };
    let crate::types::OrderKind::PassiveRel { offset, price_cap } = kind else {
        panic!("built as the wrong kind: {kind:?}");
    };
    assert_eq!(offset, scale / 100, "the offset the caller stated");
    assert_eq!(price_cap, 150 * scale, "the cap the caller stated");
}

/// A pegged-to-best order is taken as the caller states it: one price, on
/// the limit-price field. Stated without one there is nothing to send, and
/// the order is refused rather than sent malformed.
#[test]
fn a_peg_best_order_is_built_from_the_price_it_states() {
    let scale = crate::types::PRICE_SCALE;
    let order = ApiOrder {
        action: "BUY".into(),
        total_quantity: 100.0,
        order_type: "PEG BEST".into(),
        lmt_price: 150.0,
        tif: "DAY".into(),
        ..Default::default()
    };
    ClientCore::validate_order(&order, &crate::client_core::OrderSession::single("DU123")).unwrap();
    let cmd = ClientCore::build_order_request(&order, 7, 0, None).unwrap();
    let ControlCommand::Order(OrderRequest::SubmitEx { kind, .. }) = cmd else {
        panic!("a pegged-to-best order routes through the shared extended submission");
    };
    let crate::types::OrderKind::PegBest { price } = kind else {
        panic!("built as the wrong kind: {kind:?}");
    };
    assert_eq!(price, 150 * scale, "the price the caller stated");

    let unpriced = ApiOrder { lmt_price: 0.0, ..order };
    let err = ClientCore::validate_order(&unpriced, &crate::client_core::OrderSession::single("DU123")).unwrap_err();
    assert!(err.message.contains("lmt_price"), "a pegged-to-best order with no price is refused: {err}");
}

/// A snapshot ends on the venue having stated what one is made of, or on the
/// wait running out from when it was ASKED FOR. Waiting on the quiet instead
/// ended one on a pause, and never ended one the venue said nothing about.
#[test]
fn a_snapshot_ends_on_the_venue_or_on_the_wait_from_asking() {
    let core = ClientCore::new();
    core.snapshot_reqs.lock().unwrap().insert(1, crate::client_core::SnapshotWait::new(0, false));
    // Bid, ask, last, open — four of the five.
    for kind in [1, 2, 4, 14] {
        core.note_snapshot_tick(1, kind);
        assert!(!core.check_snapshot_done(1), "kind {kind} still leaves one to come");
    }
    core.note_snapshot_tick(1, 9);
    assert!(core.check_snapshot_done(1), "the close was the last of them");
    assert!(!core.check_snapshot_done(1), "and it is only said once");

    // What a kind CARRIED does not matter, only that it came: a pair states
    // its last as minus one and a contract yet to open states its open as
    // nothing, and both are the venue answering.
    core.snapshot_reqs.lock().unwrap().insert(3, crate::client_core::SnapshotWait::new(0, false));
    for kind in [1, 2, 4, 14, 9] {
        core.note_snapshot_tick(3, kind);
    }
    assert!(core.check_snapshot_done(3), "every kind was stated, whatever it said");

    // A contract the venue says nothing about is let go of on the wait, and
    // the wait is measured from asking — so one that never heard anything is
    // swept rather than held for ever.
    let long_ago = std::time::Instant::now() - std::time::Duration::from_secs(12);
    core.snapshot_reqs.lock().unwrap().insert(
        2, crate::client_core::SnapshotWait { asked_at: long_ago, ..crate::client_core::SnapshotWait::new(0, false) },
    );
    assert!(core.check_snapshot_done(2), "nothing was ever stated, and the wait is up");
    assert!(core.snapshot_reqs.lock().unwrap().is_empty(), "and nothing is left waiting");
}

/// And a snapshot on a delayed or frozen feed ends the same way, once the
/// venue has also stated the last trade's time.
///
/// Those feeds state their bid, ask, last, close and open under numbers of
/// their own — which is what the caller was told to expect. Only the realtime
/// numbers were read, so a snapshot on either feed could not be completed by
/// anything the venue said: it ran to the eleven-second sweep every time,
/// however promptly the venue answered. A gateway waits on a delayed feed for
/// the time as well (88).
#[test]
fn a_delayed_snapshot_ends_on_the_venue_too() {
    let core = ClientCore::new();
    core.mark_feed_delayed_for_test(4);
    core.snapshot_reqs.lock().unwrap().insert(2, crate::client_core::SnapshotWait::new(4, false));
    // The delayed numbering: bid, ask, last, open, close, then the time.
    for kind in [66, 67, 68, 76, 75] {
        core.note_snapshot_tick(2, kind);
        assert!(!core.check_snapshot_done(2), "delayed kind {kind} leaves one to come");
    }
    core.note_snapshot_tick(2, 88);
    assert!(
        core.check_snapshot_done(2),
        "the delayed time was the last of them, and the venue had said everything",
    );
}

/// A snapshot of a contract a gateway marks as an option also waits for the
/// venue's option model, 13, or 83 on a delayed feed; one of any other type
/// does not. The bid's, ask's and last's greeks a gateway computes itself are
/// not produced here, so nothing waits for them.
#[test]
fn an_options_snapshot_waits_for_the_model() {
    for sec_type in ["OPT", "FOP", "IOPT", "WAR", "EC"] {
        assert!(crate::client_core::marked_as_option(sec_type), "{sec_type}");
    }
    for sec_type in ["STK", "FUT", "CASH", "IND", "BAG", ""] {
        assert!(!crate::client_core::marked_as_option(sec_type), "{sec_type}");
    }

    let core = ClientCore::new();
    core.snapshot_reqs.lock().unwrap().insert(1, crate::client_core::SnapshotWait::new(0, true));
    for kind in [1, 2, 4, 14, 9] {
        core.note_snapshot_tick(1, kind);
    }
    for greek in [10, 11, 12] {
        core.note_snapshot_tick(1, greek);
    }
    assert!(!core.check_snapshot_done(1), "an option's snapshot waits for its model");
    core.note_snapshot_tick(1, 13);
    assert!(core.check_snapshot_done(1), "the model was the last of them");

    core.mark_feed_delayed_for_test(5);
    core.snapshot_reqs.lock().unwrap().insert(2, crate::client_core::SnapshotWait::new(5, true));
    for kind in [66, 67, 68, 76, 75, 88] {
        core.note_snapshot_tick(2, kind);
    }
    assert!(!core.check_snapshot_done(2), "a delayed option's snapshot waits for its model");
    core.note_snapshot_tick(2, 83);
    assert!(core.check_snapshot_done(2), "stated on the delayed number");

    core.snapshot_reqs.lock().unwrap().insert(3, crate::client_core::SnapshotWait::new(0, false));
    for kind in [1, 2, 4, 14, 9] {
        core.note_snapshot_tick(3, kind);
    }
    assert!(core.check_snapshot_done(3), "a stock's snapshot waits for no model");
}


/// The venue restates the day's executions at every logon, so the same one
/// reaches the record more than once. It is stored once, known by its id.
#[test]
fn an_execution_is_stored_once_under_its_id() {
    let core = ClientCore::new();
    let stated = |id: &str| crate::types::model::Execution { exec_id: id.into(), ..Default::default() };
    for id in ["0001f4e8.1", "0001f4e8.1", "0001f4e8.2"] {
        core.push_execution(Default::default(), stated(id), Default::default());
    }
    let stored = core.snapshot_executions(&Default::default());
    let ids: Vec<&str> = stored.iter().map(|s| s.execution.exec_id.as_str()).collect();
    assert_eq!(ids, ["0001f4e8.1", "0001f4e8.2"], "each execution once, by id");
}

/// A replace carries every number a shape is defined by, and names each in
/// the slot the shape's submit reads it from.
///
/// The trail of a trailing stop limit, a peg's offset and cap, a snap's offset
/// and a midprice cap used to be refused as numbers the replace had nowhere to
/// put. Measured on a paper session, each shape placed and replaced, the venue
/// takes them on the tags the submit states them on, so the replace carries
/// them, and a trail moved between a percentage and an amount carries the unit
/// it is now stated in.
#[test]
fn a_replace_carries_every_number_a_trail_is_stated_in() {
    let core = ClientCore::new();
    let placed = ApiOrder {
        order_id: 42, action: "BUY".into(), total_quantity: 1.0,
        order_type: "TRAIL LIMIT".into(), aux_price: 5.0, lmt_price_offset: 1.0,
        tif: "DAY".into(), ..Default::default()
    };
    core.track_order(42, ApiContract::default(), placed.clone(), 0);
    let wider = ApiOrder { aux_price: 9.0, lmt_price_offset: 2.0, ..placed };
    assert!(core.modify_refusal(42, &wider, None).is_none(), "the trail and the limit offset travel");
    assert_eq!(
        (ClientCore::replace_price(&wider), ClientCore::replace_trigger(&wider)),
        (2 * PRICE_SCALE, 9 * PRICE_SCALE),
        "the limit offset is the price the replace names, the trail its trigger",
    );
    let unset = ApiOrder { lmt_price_offset: f64::MAX, lmt_price: f64::MAX, aux_price: f64::MAX, ..wider };
    assert_eq!((ClientCore::replace_price(&unset), ClientCore::replace_trigger(&unset)), (0, 0), "unset names nothing");

    let pct = ApiOrder {
        order_id: 43, action: "SELL".into(), total_quantity: 1.0,
        order_type: "TRAIL".into(), trailing_percent: 1.0, tif: "DAY".into(), ..Default::default()
    };
    core.track_order(43, ApiContract::default(), pct.clone(), 0);
    assert!(
        core.modify_refusal(43, &ApiOrder { trailing_percent: 2.0, ..pct.clone() }, None).is_none(),
        "a percent travels on the trail tag with its unit beside it",
    );
    assert!(
        core.modify_refusal(43, &ApiOrder { aux_price: 2.0, trailing_percent: f64::MAX, ..pct }, None).is_none(),
        "and a percentage trail moved to an amount goes, stating its unit",
    );
}

/// A field the caller never mentioned is not a field stated wrongly.
///
/// The models a caller builds an order from carry this API's own "not set"
/// value as the default for a trailing percentage, so an ordinary limit order
/// arrives with it in that field. Checked as a number, it fails every bound
/// there is — and every order that never mentioned a trailing percentage was
/// refused for the one thing it did not say.
#[test]
fn an_unset_trailing_percentage_is_not_a_wrong_one() {
    let plain = ApiOrder {
        action: "BUY".into(), total_quantity: 1.0, order_type: "LMT".into(),
        lmt_price: 100.0, tif: "DAY".into(), trailing_percent: f64::MAX,
        ..Default::default()
    };
    assert!(
        ClientCore::validate_order(&plain, &crate::client_core::OrderSession::single("DU1")).is_ok(),
        "an order that states no trailing percentage is not refused for it",
    );

    // What the check exists for still fails.
    for bad in [f64::NAN, f64::INFINITY, -1.0, 1e12] {
        let order = ApiOrder { trailing_percent: bad, ..plain.clone() };
        assert!(
            ClientCore::validate_order(&order, &crate::client_core::OrderSession::single("DU1")).is_err(),
            "a trailing percentage of {bad} is not a percentage",
        );
    }
}

/// A modify of an order the venue named is judged against the venue's
/// statement of it, not against nothing.
///
/// An order named at connect is in no book of this client's. Compared against
/// nothing, there is no group to move and the replace goes; compared against
/// the venue's statement, a replace moving its group is refused as a gateway
/// refuses it, and a change of type goes.
#[test]
fn a_modify_of_a_venue_named_order_is_judged_against_the_venues_statement() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    let named = ApiOrder {
        order_id: 42, action: "BUY".into(), total_quantity: 1.0,
        order_type: "PEG MID".into(), lmt_price: 100.0, tif: "DAY".into(),
        oca_group: "A".into(), ..Default::default()
    };
    shared.orders.push_order_info(42, crate::bridge::RichOrderInfo {
        contract: ApiContract::default(),
        order: named.clone(),
        order_state: crate::types::model::OrderState { status: "Submitted".into(), ..Default::default() },
        last_exec: Default::default(),
    });
    let moved = ApiOrder { oca_group: "B".into(), ..named.clone() };
    assert!(core.modify_refusal(42, &moved, None).is_none(), "against nothing there is no group to move");
    let why = core.modify_refusal(42, &moved, Some(&shared)).expect("against the venue's statement it moves the group");
    assert_eq!((why.code, why.message.as_str()), (10326, "OCA group revision is not allowed"));
    let retyped = ApiOrder { order_type: "REL".into(), ..named };
    assert!(core.modify_refusal(42, &retyped, Some(&shared)).is_none(), "a change of type goes");
}

/// The types a replace restates as themselves, under the names the venue's
/// statement of an order carries.
///
/// A midprice order the venue named reads as `MIDPRICE`, the reference name,
/// where the table knew only the wire's `MIDPX`; a snap to the market and a
/// snap to the primary were each placed, replaced twice and withdrawn on a
/// paper session and are admitted on that answer.
#[test]
fn a_venue_named_order_restates_itself_under_the_reference_name() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    for (id, name) in [(42u64, "MIDPRICE"), (43, "SNAP MKT"), (44, "SNAP PRI"), (45, "SNAP PRIM"), (46, "SNAP MIDPT"), (47, "PEG MIDPT")] {
        let named = ApiOrder {
            order_id: id as i64, action: "BUY".into(), total_quantity: 1.0,
            order_type: name.into(), lmt_price: 100.0, aux_price: 0.05, tif: "DAY".into(), ..Default::default()
        };
        shared.orders.push_order_info(id, crate::bridge::RichOrderInfo {
            contract: ApiContract::default(),
            order: named.clone(),
            order_state: crate::types::model::OrderState { status: "Submitted".into(), ..Default::default() },
            last_exec: Default::default(),
        });
        let moved = ApiOrder { lmt_price: 101.0, aux_price: 0.10, ..named };
        assert!(core.modify_refusal(id, &moved, Some(&shared)).is_none(), "{name} restates itself");
    }
}

/// The terms a restatement replaced come back when the venue refuses it, and
/// stay put when it refuses something else.
///
/// The record takes the attempt ahead of the venue's answer, so the terms the
/// venue holds are kept beside it. Three things used to spend or misapply
/// them: a fill arriving while the replacement was still outstanding, a
/// refused cancellation rolling the terms back over a replacement the venue
/// may since have taken, and a replacement kept for an order the venue was
/// never asked to change.
fn working_at(price: f64) -> (ClientCore, SharedState, ApiOrder) {
    let core = ClientCore::new();
    let order = |price: f64| ApiOrder {
        order_id: 42, action: "BUY".into(), total_quantity: 100.0,
        order_type: "LMT".into(), lmt_price: price, tif: "DAY".into(),
        ..Default::default()
    };
    core.track_order(42, ApiContract::default(), order(price), 0);
    (core, SharedState::new(), order(price + 1.0))
}

fn refusal(reject_type: u8) -> crate::types::CancelReject {
    crate::types::CancelReject {
        order_id: 42, instrument: 0, reject_type, reason_code: 0,
        answers_a_live_change: true, still_working: Some(crate::types::OrderStatus::Submitted), timestamp_ns: 0,
    }
}

fn price_on_record(core: &ClientCore) -> f64 {
    core.open_orders.lock().unwrap().get(&42).expect("tracked").order.lmt_price
}

#[test]
fn a_fill_is_not_the_venues_answer_to_a_replacement() {
    let (core, shared, revision) = working_at(100.0);
    core.restate_order(Some(&shared), 42, ApiContract::default(), revision, 0);
    // The venue fills part of the order it holds while it is still deciding.
    core.update_order_status(
        &shared, 42, crate::types::OrderStatus::PartiallyFilled, 10.0, 90.0, 0,
    );
    // And then refuses the change.
    core.retire_rejected(&refusal(2));

    assert_eq!(price_on_record(&core), 100.0, "the record states what the venue holds");
}

#[test]
fn a_refused_cancellation_does_not_roll_back_a_replacement() {
    let (core, shared, revision) = working_at(100.0);
    core.restate_order(Some(&shared), 42, ApiContract::default(), revision, 0);
    core.retire_rejected(&refusal(1));

    assert_eq!(
        price_on_record(&core), 101.0,
        "a cancellation the venue would not make changed no terms",
    );
}

#[test]
fn the_venue_taking_the_replacement_settles_it() {
    let (core, shared, revision) = working_at(100.0);
    core.restate_order(Some(&shared), 42, ApiContract::default(), revision, 0);
    // The venue fills part of the order it holds while it is still deciding,
    // and then takes the replacement. The fill is not the answer; the taking
    // is, and it is stated rather than read off a status.
    core.update_order_status(
        &shared, 42, crate::types::OrderStatus::PartiallyFilled, 10.0, 90.0, 0,
    );
    core.settle_replacement(42);
    // A refusal behind the acceptance names something the venue has answered.
    core.retire_rejected(&refusal(2));

    assert_eq!(
        price_on_record(&core), 101.0,
        "the terms the venue took are not rolled back by a stale refusal",
    );
}

#[test]
fn a_restatement_that_never_left_is_undone() {
    let (core, shared, revision) = working_at(100.0);
    core.restate_order(Some(&shared), 42, ApiContract::default(), revision, 0);
    core.undo_restatement(42);

    assert_eq!(price_on_record(&core), 100.0, "the record states what the venue holds");
    core.retire_rejected(&refusal(2));
    assert_eq!(price_on_record(&core), 100.0, "and there is nothing left to put back twice");
}

/// A slot the engine has given back is not answered from the cache.
///
/// The cache answers "which slot does this contract hold" without asking the
/// engine, which is what keeps a placement off a round trip. The slot goes to
/// the next contract that needs one, so an answer from the cache after that
/// names another contract altogether: the order is recorded against the new
/// occupant and its fill moves that contract's position.
#[test]
fn a_slot_the_engine_gave_back_is_not_answered_from_the_cache() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.cache_instrument(756733, 4);
    core.cache_instrument(265598, 5);

    shared.market.note_released_slot(4, u64::MAX);
    core.forget_released_slots(&shared);

    assert_eq!(core.cached_instrument(&shared, 756733), None, "the freed slot is not answered");
    assert_eq!(core.cached_instrument(&shared, 265598), Some(5), "the others stand");
}

/// A replacement of an order the venue replayed records what the venue said.
///
/// The order was placed by an earlier session, so this client has no record of
/// it and one has to be made. Made from the attempt alone it read as a fresh
/// order — nothing filled, its whole quantity outstanding, a status of its own
/// invention, and slot zero, which is a real slot and not this order's. The
/// next replace was then refused for naming another contract, and a partly
/// filled order came back as untouched.
#[test]
fn a_replayed_order_is_recorded_as_the_venue_states_it() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.orders.push_order_info(4242, RichOrderInfo {
        contract: ApiContract { con_id: 756733, symbol: "SPY".into(), ..Default::default() },
        order: ApiOrder {
            order_id: 4242, action: "BUY".into(), total_quantity: 100.0,
            order_type: "LMT".into(), lmt_price: 100.0, filled_quantity: 30.0,
            ..Default::default()
        },
        order_state: ApiOrderState { status: "Submitted".into(), ..Default::default() },
        last_exec: Default::default(),
    });

    let revision = ApiOrder {
        order_id: 4242, action: "BUY".into(), total_quantity: 100.0,
        order_type: "LMT".into(), lmt_price: 101.0, ..Default::default()
    };
    core.restate_order(Some(&shared), 4242, ApiContract::default(), revision, 7);

    let tracked = core.open_orders.lock().unwrap().get(&4242).cloned().expect("recorded");
    assert_eq!(tracked.instrument, 7, "on the slot the call named");
    assert_eq!(tracked.filled, 30.0, "with what the venue says has filled");
    assert_eq!(tracked.remaining, 70.0, "and what is left of it");
    assert_eq!(tracked.status, "Submitted", "under the venue's own status");
    assert_eq!(
        tracked.before_the_replace.as_ref().map(|o| o.lmt_price), Some(100.0),
        "and the terms the venue holds, for a refusal to put back",
    );
}

/// A refusal that never reached the venue still puts the terms back.
///
/// The record takes a replacement ahead of the answer, and the answer can come
/// from this side of the wire: an order nothing here has a record of, an
/// offset that cannot be restated, a revision that cannot be named. None of
/// those states where the order stands, so a refusal carrying no status left
/// the terms of an attempt that never left this process standing in the
/// record, and every later cancel and replace restated from them.
#[test]
fn a_refusal_that_states_no_status_still_puts_the_terms_back() {
    let (core, shared, revision) = working_at(100.0);
    core.restate_order(Some(&shared), 42, ApiContract::default(), revision, 0);
    core.retire_rejected(&crate::types::CancelReject {
        order_id: 42, instrument: 0, reject_type: 2, reason_code: -1,
        answers_a_live_change: true, still_working: None, timestamp_ns: 0,
    });

    assert_eq!(price_on_record(&core), 100.0, "the record states what the venue holds");
}

/// A refusal this client made states no reason of the venue's.
///
/// The reason code is the venue's, and a refusal composed on this side of the
/// wire has none — it carries the sentinel that says so. Formatted as a
/// number, the caller was handed "reason: -1" as though the venue had stated
/// it, and a program branching on the reason had a code to match that means
/// nothing.
#[test]
fn a_refusal_from_this_side_states_no_reason_code() {
    let (core, _shared, _revision) = working_at(100.0);
    let (_, ours) = core.retire_rejected(&crate::types::CancelReject {
        order_id: 42, instrument: 0, reject_type: 2, reason_code: -1,
        answers_a_live_change: true, still_working: None, timestamp_ns: 0,
    });
    assert_eq!(ours, "Order 42 modify rejected", "no reason, and none invented");

    let (_, theirs) = core.retire_rejected(&crate::types::CancelReject {
        order_id: 42, instrument: 0, reject_type: 1, reason_code: 0,
        answers_a_live_change: true, still_working: None, timestamp_ns: 0,
    });
    assert_eq!(
        theirs, "Order 42 cancel rejected by the venue (reason: 0)",
        "and the venue's own reason still reaches the caller",
    );
}

/// Every reader of the slot cache is answered from a cache the engine has not
/// already emptied under it.
///
/// The cache answers "which slot does this contract hold" without a round trip
/// to the engine, and the engine hands a freed slot to the next contract that
/// needs one. Every reader has to drop what has been given back before it
/// reads, and one of them will always forget: a chain of margin previews frees
/// its slot after each one, so a later market-data request read a slot the
/// first strike now holds, decided it was already watched, and delivered that
/// strike's prices under every other strike's request id.
#[test]
fn the_slot_cache_cannot_be_read_before_it_is_emptied() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.cache_instrument(756733, 4);
    shared.market.note_released_slot(4, u64::MAX);

    assert_eq!(
        core.cached_instrument(&shared, 756733), None,
        "the reader is answered from a cache the release has already reached",
    );
}

/// A refusal of a change the venue has already answered puts nothing back.
///
/// The venue takes a second change before it has answered the first, and the
/// engine settles a refusal against the revision it names. That verdict now
/// reaches the surfaces: without it, a late or duplicate refusal of a change
/// already answered rolled back the change still outstanding, so the record
/// stated terms the venue was working away from.
#[test]
fn a_refusal_of_a_change_already_answered_puts_nothing_back() {
    let (core, shared, revision) = working_at(100.0);
    core.restate_order(Some(&shared), 42, ApiContract::default(), revision, 0);
    core.retire_rejected(&crate::types::CancelReject {
        order_id: 42, instrument: 0, reject_type: 2, reason_code: 0,
        still_working: None, answers_a_live_change: false, timestamp_ns: 0,
    });

    assert_eq!(
        price_on_record(&core), 101.0,
        "the change the venue is still working is not rolled back by a stale refusal",
    );
}

/// A replace names the order, so the contract it states has to be the one the
/// venue says the order is on.
///
/// A combination is not one contract. Two different combinations on one
/// underlying carry the same id and register the same slot, so comparing ids
/// could never tell them apart — and after a description is named, the id on a
/// combination is one leg's underlying anyway. What identifies it is the legs.
#[test]
fn a_replace_names_the_contract_the_venue_says_the_order_is_on() {
    let leg = |con_id: i64, ratio: i32, action: &str| crate::types::model::ComboLeg {
        con_id, ratio, action: action.into(), ..Default::default()
    };
    let combo = |legs: Vec<crate::types::model::ComboLeg>| ApiContract {
        symbol: "SPY".into(), sec_type: "BAG".into(), combo_legs: legs, ..Default::default()
    };
    let spread = combo(vec![leg(1, 1, "BUY"), leg(2, 1, "SELL")]);

    assert!(
        ClientCore::names_the_same_contract(&spread, &combo(vec![
            leg(2, 1, "SELL"), leg(1, 1, "BUY"),
        ])),
        "the same legs, however they are written down",
    );
    assert!(
        !ClientCore::names_the_same_contract(&spread, &combo(vec![
            leg(1, 1, "BUY"), leg(3, 1, "SELL"),
        ])),
        "a different combination is a different contract",
    );
    assert!(
        !ClientCore::names_the_same_contract(&spread, &combo(vec![
            leg(1, 2, "BUY"), leg(2, 1, "SELL"),
        ])),
        "and so is the same pair in other proportions",
    );

    let by_id = |con_id: i64| ApiContract { con_id, ..Default::default() };
    assert!(ClientCore::names_the_same_contract(&by_id(7), &by_id(7)));
    assert!(!ClientCore::names_the_same_contract(&by_id(7), &by_id(8)));
    assert!(
        ClientCore::names_the_same_contract(&by_id(0), &by_id(8)),
        "a side that states no contract states nothing to disagree with",
    );
    assert!(
        ClientCore::names_the_same_contract(&spread, &by_id(8)),
        "nor does one that states legs against one that states none",
    );
}

/// A bound on time reaches an execution the venue timed, and no other.
///
/// The bound and the stored time are compared on their digits, so an execution
/// carrying something that is not a venue timestamp sorts wherever those
/// digits fall — a count of nanoseconds sorts before every bound a caller can
/// write, and every such fill vanished from an ordinary request for today's.
/// One the venue never timed cannot be placed either side of a bound at all,
/// and is kept rather than hidden.
#[test]
fn a_time_bound_reads_only_what_the_venue_timed() {
    let at = |time: &str| StoredExecution {
        contract: ApiContract::default(),
        execution: crate::types::model::Execution { time: time.into(), ..Default::default() },
        commission_and_fees: Default::default(),
    };
    let after = |bound: &str| crate::types::model::ExecutionFilter {
        time: bound.into(), ..Default::default()
    };

    assert!(execution_matches(&at("20260905-10:00:00"), &after("20260101-00:00:00")));
    assert!(!execution_matches(&at("20260101-10:00:00"), &after("20990101-00:00:00")));
    assert!(
        execution_matches(&at(""), &after("20990101-00:00:00")),
        "one the venue never timed is not hidden by a bound on time",
    );
    assert!(
        execution_matches(&at("20260905-10:00:00"), &after("")),
        "and a request that states no bound reads them all",
    );
}

/// Each refusal the catalogue names carries its own number, not the general
/// one for a malformed request.
///
/// Every shared validator answered in prose, and prose is stamped with the
/// general validation number on the way out — so a caller branching on the
/// number for an unset stop price, an unpermitted security type or a
/// combination with no legs took the same branch it takes for a typo in a
/// field name, and could not tell them apart. One row per number, so the
/// numbers cannot drift back one validator at a time.
#[test]
fn a_refusal_the_catalogue_names_carries_its_own_number() {
    let priced = |order_type: &str| ApiOrder {
        action: "BUY".into(), total_quantity: 1.0, order_type: order_type.into(),
        lmt_price: 100.0, tif: "DAY".into(), ..Default::default()
    };

    // A stop with nothing to trigger on.
    for order_type in ["STP", "STP LMT", "TRAIL", "TRAIL LIMIT", "MIT", "LIT"] {
        let why = ClientCore::validate_order(&priced(order_type), &crate::client_core::OrderSession::single(""))
            .expect_err("a stop with no trigger price is refused");
        assert_eq!(why.code, 403, "{order_type}: {why}");
    }

    // A combination that names no legs, and a leg this client cannot state.
    assert_eq!(
        ClientCore::validate_combo_legs("BAG", 0).expect_err("no legs").code, 314,
    );
    let leg = crate::types::model::ComboLeg {
        action: "SIDEWAYS".into(), ratio: 1, ..Default::default()
    };
    assert_eq!(ClientCore::validate_leg(0, &leg).expect_err("no such side").code, 313);

    // A security type the account was not permitted at logon.
    let permitted = std::collections::HashMap::from([
        ("STK".to_string(), vec!["LMT".to_string()]),
    ]);
    assert_eq!(
        ClientCore::refuse_unpermitted_sec_type(&permitted, "OPT")
            .expect_err("not permitted").code,
        203,
    );

    // A trigger method the venue does not carry, and a date it cannot read.
    let mut triggered = priced("LMT");
    triggered.trigger_method = 9;
    assert_eq!(
        ClientCore::validate_order(&triggered, &crate::client_core::OrderSession::single("")).expect_err("no such trigger").code, 146,
    );
    let mut dated = priced("LMT");
    dated.tif = "GTD".into();
    dated.good_till_date = "the day after tomorrow".into();
    assert_eq!(
        ClientCore::validate_order(&dated, &crate::client_core::OrderSession::single("")).expect_err("unreadable date").code, 334,
    );

    // And a request that is malformed with no number of its own keeps the
    // general one, which is what makes the rest branchable.
    let mut unpriced = priced("LMT");
    unpriced.lmt_price = f64::NAN;
    assert_eq!(
        ClientCore::validate_order(&unpriced, &crate::client_core::OrderSession::single("")).expect_err("not a price").code,
        Refusal::VALIDATION,
    );
}

/// The account reads wait for the download to finish, not for the first thing
/// heard, and each download ends where the caller can see it.
///
/// A drop zeroes every holding's marks deliberately and keeps the quantities.
/// Gated on anything having been heard, the first figure of the rebuilt
/// connection let the whole pre-drop book out at a price of nothing, a value
/// of nothing and no profit -- including any holding closed while the
/// connection was down. A summary asked for in the same window was answered
/// with the few tags parsed so far, and the request is one-shot, so the tags
/// it asked for never came. And the end that says the book is whole was said
/// once for the life of the client, so the second download completed in
/// silence.
#[test]
fn the_account_reads_wait_for_the_download_and_every_download_ends() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_account_updates(true);
    core.subscribe_account_summary(3, "All", vec![String::new()]).unwrap();

    // A figure has arrived, and the download has not finished.
    shared.portfolio.holdings_restated_under("AR.1");
    shared.portfolio.note_account_value("NetLiquidation", "75425.51", "USD");
    shared.portfolio.set_position_info(crate::types::PositionInfo {
        con_id: 756733, position: 100.0, avg_cost: 150 * crate::types::PRICE_SCALE,
        ..Default::default()
    });

    assert!(
        core.prepare_portfolio_updates(&shared).is_empty(),
        "no holding goes out priced at nothing while the download is still running",
    );
    assert!(
        core.prepare_account_summary(&shared, "DU1").is_none(),
        "and no summary is answered from the figures parsed so far",
    );

    // The download ends.
    shared.portfolio.set_account_download_complete("AR.1");
    shared.portfolio.account_download_is_settled();

    assert!(!core.prepare_portfolio_updates(&shared).is_empty(), "now the holdings go out");
    assert!(core.prepare_account_summary(&shared, "DU1").is_some(), "and the summary is answered");
    let first = core.prepare_account_updates(&shared).expect("a batch");
    assert!(first.finished, "the end that says the book is whole");

    // A second download, as a rebuilt connection makes.
    shared.portfolio.account_download_is_pending();
    assert!(
        !core.prepare_account_updates(&shared).is_some_and(|b| b.finished),
        "which is not said again while the second download is running",
    );
    shared.portfolio.holdings_restated_under("AR.2");
    shared.portfolio.note_account_value("NetLiquidation", "75000.00", "USD");
    shared.portfolio.set_account_download_complete("AR.2");
    shared.portfolio.account_download_is_settled();

    let second = core.prepare_account_updates(&shared).expect("a second batch");
    assert!(
        second.finished,
        "and the second download ends where the caller can see it, not in silence",
    );
}

/// Figures parsed before the download has ended are not the account. After a
/// drop the struct still holds the pre-drop figures and the first frame of the
/// new connection restates one of them; the fallback read the rest as the
/// account's current profit.
#[test]
fn the_account_fallback_waits_for_the_download() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.set_account(&crate::types::AccountState::default());
    core.subscribe_pnl(7, "").unwrap();
    assert!(
        core.poll_pnl(&shared).into_iter().next().is_none(),
        "nothing is answered from figures the download has not finished stating",
    );
    shared.portfolio.account_download_is_settled();
    assert!(core.poll_pnl(&shared).into_iter().next().is_some(), "answered once the account has stated itself whole");
}

/// Asking for the account again restates it. The reference client answers a
/// second request with every figure and the end again; here the second ask
/// was answered with nothing at all, and a caller blocking on the end waited
/// for ever.
#[test]
fn asking_for_the_account_again_restates_it() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_account_updates(true);
    shared.portfolio.holdings_restated_under("AR.1");
    shared.portfolio.note_account_value("NetLiquidation", "75425.51", "USD");
    shared.portfolio.set_account_download_complete("AR.1");
    shared.portfolio.account_download_is_settled();
    let first = core.prepare_account_updates(&shared).expect("a batch");
    assert!(first.finished, "the end that says the book is whole");
    let quiet = core.prepare_account_updates(&shared).expect("a batch");
    assert!(quiet.fields.is_empty() && !quiet.finished, "nothing new, nothing said");

    core.subscribe_account_updates(true);
    let again = core.prepare_account_updates(&shared).expect("a batch");
    assert!(!again.fields.is_empty(), "asked again, every figure is restated");
    assert!(again.finished, "and the end is said again");
}

/// An execution the venue states with no id is known by its content, as the
/// engine already knows it: the day's executions are replayed at every logon,
/// and an absent id is the shape a replay takes. Stored again on every replay,
/// a caller summing the day's volume doubled it on every rebuilt connection.
#[test]
fn an_execution_with_no_id_is_stored_once_by_its_content() {
    let core = ClientCore::new();
    let stated = |cum_qty: f64| crate::types::model::Execution {
        order_id: 84, time: "20260905  10:00:00".into(), shares: 100.0, price: 150.25, cum_qty,
        ..Default::default()
    };
    for exec in [stated(100.0), stated(100.0), stated(200.0)] {
        core.push_execution(Default::default(), exec, Default::default());
    }
    let stored = core.snapshot_executions(&Default::default());
    let cum: Vec<f64> = stored.iter().map(|s| s.execution.cum_qty).collect();
    assert_eq!(cum, [100.0, 200.0], "the same print once, the next print once");
}

/// A charge naming no execution stamps none. Matched on the empty name, it
/// was written onto every execution stored without one.
#[test]
fn a_charge_naming_no_execution_stamps_none() {
    let core = ClientCore::new();
    core.push_execution(Default::default(), Default::default(), Default::default());
    core.record_charge(&crate::types::model::CommissionAndFeesReport::charged("", 1.25, "USD"));
    let stored = core.snapshot_executions(&Default::default());
    assert_eq!(
        stored[0].commission_and_fees.commission_and_fees, 0.0,
        "nothing named, nothing stamped",
    );
}


/// No profit is worked out from a book the download has not restated. A
/// trading-connection drop leaves the quotes flowing while the book is
/// stale, and the client-side sum multiplied the pre-drop quantities by live
/// prices on every tick: a holding the account closed during the outage went
/// on being valued, and its profit reported, until the download arrived.
#[test]
fn no_profit_is_worked_out_from_a_book_the_download_has_not_restated() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.set_account(&crate::types::AccountState::default());
    shared.portfolio.account_download_is_settled();
    shared.portfolio.set_position_info(crate::types::PositionInfo {
        con_id: 756733, position: 100.0, avg_cost: 150 * crate::types::PRICE_SCALE,
        ..Default::default()
    });
    core.cache_instrument(756733, 4);
    shared.market.push_quote(4, &crate::types::Quote { last: 151 * crate::types::PRICE_SCALE, ..Default::default() });
    core.subscribe_pnl(7, "").unwrap();
    core.subscribe_pnl_single(8, 756733, "").unwrap();
    assert!(core.poll_pnl(&shared).into_iter().next().is_some(), "priced from the live quote while the book is whole");
    assert!(!core.poll_pnl_single(&shared).is_empty());

    shared.portfolio.account_download_is_pending();
    shared.market.push_quote(4, &crate::types::Quote { last: 152 * crate::types::PRICE_SCALE, ..Default::default() });
    assert!(core.poll_pnl(&shared).into_iter().next().is_none(), "nothing while the download is pending, however the price moves");
    assert!(core.poll_pnl_single(&shared).is_empty(), "for the position either");

    shared.portfolio.account_download_is_settled();
    shared.market.push_quote(4, &crate::types::Quote { last: 153 * crate::types::PRICE_SCALE, ..Default::default() });
    assert!(core.poll_pnl(&shared).into_iter().next().is_some(), "and again once the book is whole");
}

/// A summary asked for before the download finished is answered
/// when the session ends rather than held for ever: parked behind the
/// download gate, the caller could neither receive its end nor withdraw it
/// on the ended session.
#[test]
fn a_summary_parked_behind_the_download_is_answered_when_the_session_ends() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    core.subscribe_account_summary(7, "NetLiquidation", vec![String::new()]).unwrap();
    shared.portfolio.account_download_is_pending();
    assert!(core.prepare_account_summary(&shared, "DU1").is_none(), "parked while the download runs");
    shared.reference.set_session_over("the trading connection");
    assert!(
        core.prepare_account_summary(&shared, "DU1").is_some(),
        "answered with what there is once the session is over",
    );
}


/// A condition's trigger method and margin percent are the TWS API's `int`s,
/// and every value goes to the venue as stated: no gateway refusal of either
/// has been read, so none is made. What the venue answers to a value outside
/// the methods it names, or to a negative percent, is its own to say.
#[test]
fn a_condition_carries_its_trigger_method_and_percent_as_stated() {
    let priced = || ApiOrder {
        action: "BUY".into(), total_quantity: 1.0, order_type: "LMT".into(),
        lmt_price: 100.0, tif: "DAY".into(), ..Default::default()
    };
    for tm in [0, 4, 5, 6, 7, 8, 9, 256, -1, i32::MAX] {
        let mut order = priced();
        order.conditions.push(crate::types::OrderCondition::Price {
            con_id: 756733, exchange: "SMART".into(), price: 100,
            is_more: true, trigger_method: tm, is_conjunction_connection: false,
        });
        ClientCore::validate_order(&order, &crate::client_core::OrderSession::single(""))
            .unwrap_or_else(|e| panic!("condition trigger {tm} refused: {e:?}"));
    }
    for percent in [-5, 0, 10, i32::MAX] {
        let mut order = priced();
        order.conditions.push(crate::types::OrderCondition::Margin {
            percent, is_more: false, is_conjunction_connection: false,
        });
        ClientCore::validate_order(&order, &crate::client_core::OrderSession::single(""))
            .unwrap_or_else(|e| panic!("margin percent {percent} refused: {e:?}"));
    }
}

/// A request joining a live subscription is owed the quote as it stands.
///
/// The ticks are worked out once per contract, against what was last sent for
/// that contract, and fanned to everyone watching it. A request that joined a
/// contract whose baseline already matched the quote was therefore sent nothing
/// — and on a contract that is not moving it stayed that way, indefinitely.
/// Forgetting the baseline is what makes the next pass state everything the
/// venue has said, which is what a subscription is answered with.
#[test]
fn a_forgotten_baseline_states_the_quote_as_it_stands() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.market.set_instrument_count(4);

    // Request 1 already holds the contract, as a first subscription leaves it.
    let iid: InstrumentId = 0;
    core.con_id_to_instrument.lock().unwrap().insert(756733, iid);
    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    core.req_to_instrument.lock().unwrap().insert(1, iid);
    shared.market.push_quote(iid, &crate::types::Quote {
        bid: 100 * PRICE_SCALE,
        ask: 101 * PRICE_SCALE,
        ..Default::default()
    });
    assert!(core.poll_instrument_ticks(&shared, iid, 1).delivered, "the quote is stated once");
    assert!(
        !core.poll_instrument_ticks(&shared, iid, 1).delivered,
        "and not again while it stands still",
    );

    // The second joins it — through the engine's record of taking it, which
    // is where the baseline is forgotten. Doing that here instead would prove
    // only that a cleared baseline restates, which the two lines above
    // already say.
    core.note_mkt_data_taken(&shared, &crate::bridge::MarketDataTaken {
        asked_at: std::time::Instant::now(),
        req_id: 2, slot: iid, generation: 2, con_id: 756733, series: Vec::new(),
        snapshot: false, one_shot: false, data_type: data_type_for_mode(0), marked: false,
    });
    assert_eq!(core.followers_of(iid), [2], "the same contract, so it followed rather than took one");
    assert!(
        core.poll_instrument_ticks(&shared, iid, 2).delivered,
        "a request that joins is owed the quote as it stands, not the next move",
    );
}

/// A request joining a contract the venue already refused is refused too.
///
/// The refusal is drained once and told to whoever held the contract then. A
/// request that joins afterwards follows the existing subscription, so nothing
/// is sent for it and nothing comes back — and it was told nothing either. The
/// reason is kept for it, and let go with the slot, so the next contract on that
/// slot does not inherit the last one's refusal.
#[test]
fn registering_a_joiner_leaves_its_refusal_to_the_engine() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.market.set_instrument_count(4);

    // Request 1 already holds the contract, as a first subscription leaves it.
    let iid: InstrumentId = 0;
    core.con_id_to_instrument.lock().unwrap().insert(756733, iid);
    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    core.req_to_instrument.lock().unwrap().insert(1, iid);

    // The venue refuses it, and whoever held it is told once.
    shared.market.push_subscription_failure(iid, "no entitlement".to_string());
    assert_eq!(shared.market.drain_subscription_failures().len(), 1);

    // A second request joins the same contract — through the engine's record
    // of taking it. The engine carries its kept reason in a separate record.
    core.note_mkt_data_taken(&shared, &crate::bridge::MarketDataTaken {
        asked_at: std::time::Instant::now(),
        req_id: 2, slot: iid, generation: 2, con_id: 756733, series: Vec::new(),
        snapshot: false, one_shot: false, data_type: data_type_for_mode(0), marked: false,
    });
    assert!(shared.market.drain_subscription_failures_direct().is_empty());
    assert_eq!(shared.market.failure_for_follower(iid).as_deref(), Some("no entitlement"));

    shared.market.note_released_slot(iid, u64::MAX);
    assert!(
        shared.market.failure_for_follower(iid).is_none(),
        "and the slot's next contract does not inherit it",
    );
}

/// And a refusal the venue has since taken back is not what a joiner is owed.
///
/// Let go only with the slot, the reason a contract could not be subscribed
/// before a reconnect was still there afterwards: the feed came back, the
/// subscription was replayed and accepted, and every request joining the live
/// subscription was handed a refusal off the outage it had already recovered
/// from. What is queued is not touched — those are deliveries owed to the
/// request that was refused, and a later success does not unsay them.
#[test]
fn a_subscription_the_venue_has_taken_is_no_longer_refused_for_a_joiner() {
    let shared = SharedState::new();
    let iid: InstrumentId = 0;

    shared.market.push_subscription_failure(iid, "no entitlement".to_string());
    assert_eq!(
        shared.market.failure_for_follower(iid).as_deref(),
        Some("no entitlement"),
    );

    // What the acknowledgement of a replayed subscription says.
    shared.market.note_subscription_accepted(iid);
    assert!(
        shared.market.failure_for_follower(iid).is_none(),
        "the contract is live, so a joiner is owed the quote and not a refusal",
    );
    assert_eq!(
        shared.market.drain_subscription_failures().len(), 1,
        "and the request that was refused is still owed the reason it was",
    );
}

/// A slot given back takes what is queued under it, not only what is cached.
///
/// What is queued names a slot rather than a contract, so the next contract to
/// take the slot is who it reaches: an increment acknowledged for the contract
/// that left arrives as the new one's. A reader stalled in a callback is all it
/// takes for the release to land in between.
#[test]
fn a_released_slot_leaves_nothing_queued_under_it() {
    let shared = SharedState::new();
    let slot: InstrumentId = 3;

    shared.market.push_tick_req_params(slot, crate::bridge::TickReqParams { min_tick: 0.01, ..Default::default() });
    shared.market.push_tick_news(crate::types::TickNews {
        instrument: slot,
        provider_code: "BRFG".into(),
        article_id: "BRFG$1".into(),
        headline: "about the contract that left".into(),
        timestamp: 0,
    });
    shared.market.push_option_computation(crate::types::OptionComputation {
        instrument: slot,
        ..Default::default()
    });
    shared.market.note_released_slot(slot, u64::MAX);

    assert!(
        shared.market.drain_tick_req_params().iter().all(|(at, _)| *at != slot),
        "no increment is delivered for the contract that left",
    );
    assert!(
        shared.market.drain_tick_news().iter().all(|n| n.instrument != slot),
        "nor a headline about the contract that left, under the one that took its slot",
    );
    assert!(
        shared.market.drain_option_computations().iter().all(|c| c.instrument != slot),
        "nor a model solved against the previous contract's volatility and price",
    );
}


/// A quote feed the engine has given up on takes no more subscriptions.
///
/// The caller was told it had one either way. A new contract took a slot and
/// its request was recorded for a replay that is not coming, with nothing sent
/// — there is no connection to write it to. A request joining a contract
/// already watched never reached the engine at all: it is answered from this
/// side, off a subscription that had stopped. Both read as live and waited out
/// the session for a first tick.
#[test]
fn no_subscription_is_taken_on_a_feed_that_is_over_for_the_session() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    let (tx, _rx) = std::sync::mpsc::channel();
    shared.market.set_instrument_count(4);

    // A contract already watched, which is what a joining request finds.
    let iid: InstrumentId = 0;
    core.con_id_to_instrument.lock().unwrap().insert(756733, iid);
    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    core.req_to_instrument.lock().unwrap().insert(1, iid);

    shared.market.set_market_data_over("the venue would not take the connection back");

    let joining = core.register_mkt_data(
        &shared, &tx, 2, 756733, "SPY", "SMART", "STK", "USD", &Default::default(),
        false, false, "", 0, None, None, false,
    );
    assert!(joining.is_err(), "the joiner is refused: {joining:?}");

    let fresh = core.register_mkt_data(
        &shared, &tx, 3, 272093, "MSFT", "SMART", "STK", "USD", &Default::default(),
        false, false, "", 0, None, None, false,
    );
    assert!(fresh.is_err(), "and so is a contract nobody is watching: {fresh:?}");
}

/// A quote and a cost both come off the wire, so their difference need not
/// fit the width either is held in.
///
/// Each is parsed into that width and held at its edge where it will not fit,
/// so a position marked at one end against a cost at the other asks for a
/// difference no figure can carry. Subtracted plain, the unrealized figure
/// came back wrapped, and a caller was told a position had made money it had
/// lost — by the width of the whole range.
#[test]
fn an_unrealized_figure_holds_at_the_edge_rather_than_wrapping_past_it() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.account_download_is_settled();
    core.subscribe_pnl_single(11, 8005, "").unwrap();

    let iid: InstrumentId = 0;
    core.con_id_to_instrument.lock().unwrap().insert(8005, iid);
    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    shared.portfolio.set_position_info(PositionInfo {
        con_id: 8005,
        position: 1.0,
        avg_cost: Price::MIN,
        symbol: "SYM8005".into(),
        sec_type: "STK".into(),
        currency: "USD".into(),
        multiplier: String::new(),
        ..Default::default()
    });
    shared.market.push_quote(iid, &Quote { last: Price::MAX, ..Default::default() });

    // What matters is that the answer arrives at all: the subtraction runs on
    // the caller's own thread, and a panic there is the caller's process.
    let updates = core.poll_pnl_single(&shared);
    assert!(!updates.is_empty(), "the position is still reported");
}

/// A P&L number pointed at another contract does not inherit the last one.
///
/// A figure is only sent where it differs from the one sent before, so the
/// cache left under the number either reported the previous contract's day as
/// this one's, or — where the two happened to agree — reported nothing at all
/// until something moved. The withdrawal clears it for this reason; taking the
/// number without withdrawing it did not.
#[test]
fn a_pnl_number_taken_for_another_contract_starts_clean() {
    let core = ClientCore::new();
    core.subscribe_pnl_single(11, 8001, "").unwrap();
    core.last_pnl_single.lock().unwrap().insert(11, [1, 2, 3, 4, 5]);

    assert_eq!(core.subscribe_pnl_single(11, 8002, "").unwrap_err().code, 102);
    core.unsubscribe_pnl_single(11);
    core.subscribe_pnl_single(11, 8002, "").unwrap();
    assert!(
        !core.last_pnl_single.lock().unwrap().contains_key(&11),
        "the last contract's figures are not this one's",
    );
}

/// A joiner named by the engine starts with a fresh quote baseline.
/// Its acknowledgement and refusal are engine records, never reader writes.
#[test]
fn registering_a_named_joiner_resets_its_baseline_without_pushing_records() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.market.set_instrument_count(4);

    let iid: InstrumentId = 0;
    // Somebody already holds the contract: acknowledged, then refused, with
    // the quote as it stands already matching this side's baseline.
    core.instrument_to_req.lock().unwrap().insert(iid, 1);
    core.req_to_instrument.lock().unwrap().insert(1, iid);
    shared.market.push_tick_req_params(iid, crate::bridge::TickReqParams { min_tick: 0.01, ..Default::default() });
    let _ = shared.market.drain_tick_req_params();
    shared.market.push_subscription_failure(iid, "no entitlement".to_string());
    let _ = shared.market.drain_subscription_failures();
    core.last_quotes.lock().unwrap().insert(iid, [7i64; 16]);

    // Named by symbol, so this side holds no identity for it: the engine's
    // record of taking it names the slot.
    core.note_mkt_data_taken(&shared, &crate::bridge::MarketDataTaken {
        asked_at: std::time::Instant::now(),
        req_id: 2, slot: iid, generation: 2, con_id: 0, series: Vec::new(),
        snapshot: false, one_shot: false, data_type: data_type_for_mode(0), marked: false,
    });

    assert!(shared.market.drain_tick_req_params_direct().is_empty());
    assert!(shared.market.drain_subscription_failures_direct().is_empty());
    assert!(
        !core.last_quotes.lock().unwrap().contains_key(&iid),
        "and the baseline still matched the quote, so nothing was ever stated to it",
    );
}



/// An answer worked out here survives a slot going back to the table.
///
/// A model the venue publishes names the contract it is about. One solved on
/// this side belongs to the question that asked it and names no contract at
/// all, so it is filed under slot zero — a real slot, held by whatever
/// contract happens to have it. Dropping that slot took every answer waiting
/// on it, and the callbacks their callers were owed never arrived.
#[test]
fn an_answer_this_side_worked_out_is_not_dropped_with_a_slot() {
    let shared = SharedState::new();
    let slot: InstrumentId = 0;

    // The venue's own model for the contract on that slot.
    shared.market.push_option_computation(crate::types::OptionComputation {
        instrument: slot,
        ..Default::default()
    });
    // And an answer to a question asked here, which names no contract.
    shared.market.push_option_computation(crate::types::OptionComputation {
        opt_price: 1.25,
        ..crate::types::OptionComputation::solved(77)
    });

    shared.market.note_released_slot(slot, u64::MAX);

    let left = shared.market.drain_option_computations();
    assert!(
        left.iter().any(|c| c.answers == Some(77)),
        "the answer to a question asked here went with somebody else's slot: {left:?}",
    );
    assert!(
        !left.iter().any(|c| c.answers.is_none()),
        "and the model the venue published for the slot did go with it: {left:?}",
    );
}

/// A follower receives the feed already subscribed, including after promotion.
#[test]
fn followers_keep_the_subscriptions_market_data_type() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    let subscribe = |req_id| core.note_mkt_data_taken(&shared, &crate::bridge::MarketDataTaken {
        asked_at: std::time::Instant::now(),
        req_id, slot: 0, generation: req_id as u64, con_id: 756733, series: Vec::new(),
        snapshot: false, one_shot: false, data_type: data_type_for_mode(core.subscription_mode()), marked: false,
    });

    core.set_market_data_type(MDT_DELAYED);
    subscribe(1);
    core.set_market_data_type(MDT_REALTIME);
    subscribe(2);
    assert_eq!(core.check_mdt_needed(1, true), Some(MDT_DELAYED));
    assert_eq!(core.check_mdt_needed(2, true), Some(MDT_DELAYED), "follower");
    for holder in [1, 2] {
        shared.market.push_quote(0, &Quote {
            bid: (100 + holder) * crate::types::PRICE_SCALE,
            ..Default::default()
        });
        assert_eq!(core.req_id_for_instrument(0), holder);
        let polled = core.poll_instrument_ticks(&shared, 0, holder);
        assert!(polled.delayed);
        assert_eq!(polled.ticks[0].tick_type, 66);
        core.unregister_mkt_data(holder);
        assert_eq!(core.watchers_of(0).is_empty(), holder == 2, "the last one takes the slot's feed with it");
    }
    subscribe(3);
    assert_eq!(core.check_mdt_needed(3, true), Some(MDT_REALTIME), "a new subscription has its own mode");
}



/// A request lets go of a slot the engine has taken back.
///
/// The slot goes to the next contract that needs one, and only the contract
/// cache was forgetting it. The request that had been watching it still named
/// it, so the next contract found that request holding its slot: it arrived as
/// a follower, and its quotes went to a caller whose own subscription was
/// already over.
#[test]
fn a_refused_subscription_does_not_go_on_holding_the_slot_it_was_given() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    assert!(!core.take_or_follow(7, 100, &[], 5, 0), "the first request held the slot");

    // The engine gives the slot back, naming the occupancy that is ending.
    shared.market.note_released_slot(7, 5);
    core.forget_released_slots(&shared);

    assert_eq!(core.watching(100), None, "it is not watching anything now");
    assert!(
        !core.take_or_follow(7, 200, &[], 6, 0),
        "the contract that took the slot next holds it outright, rather than following a \
         request the venue already refused",
    );
    assert!(core.followers_of(7).is_empty(), "and nothing is following it");
}

/// A price no carry reproduces leaves nothing to answer against.
///
/// Everything here is anchored to the model the venue published: the carry is
/// the one at which this tree makes the venue's own price of the contract, at
/// the volatility the venue used. Where no carry in a plausible range does
/// that, the model is not the venue's — and a figure worked out on it is this
/// client's own with the venue's name on it. Taken as no carry at all, which
/// is what it was, that is exactly what was published.
#[test]
fn a_model_that_cannot_be_anchored_is_refused_rather_than_answered() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    let option = ApiContract {
        con_id: 101, sec_type: "OPT".into(), strike: 100.0, right: "C".into(),
        ..Default::default()
    };
    core.cache_instrument(option.con_id, 3);
    // Twice what a month of this volatility can make of this strike, which no
    // dividend and no rate reaches.
    shared.market.push_option_computation(crate::types::OptionComputation {
        instrument: 3, implied_vol: 0.2, opt_price: 5.0, und_price: 100.0,
        cal_days: 30.0, ..Default::default()
    });
    let solve = |terms, model, schedule: &[(f64, f64)]| {
        crate::control::option_model::option_price(terms, model, schedule, 0.2, 100.0)
    };
    assert!(
        core.solve_option(&shared, &option, None, solve).is_err(),
        "a model nothing anchors was answered",
    );
}

/// A model on a reused slot belongs to its new contract.
#[test]
fn an_option_solve_forgets_a_released_contracts_slot() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    let option = ApiContract {
        con_id: 101, sec_type: "OPT".into(), strike: 100.0, right: "C".into(),
        ..Default::default()
    };
    // A statement the model can be anchored to: the price a month of this
    // volatility actually makes of this strike. A figure picked out of the air
    // is one no carry reproduces, which is refused rather than answered.
    let publish = || shared.market.push_option_computation(crate::types::OptionComputation {
        instrument: 3, implied_vol: 0.2, opt_price: 2.28, und_price: 100.0,
        cal_days: 30.0, ..Default::default()
    });
    let solve = |terms, model, schedule: &[(f64, f64)]| {
        crate::control::option_model::option_price(terms, model, schedule, 0.2, 100.0)
    };
    core.cache_instrument(option.con_id, 3);
    publish();
    assert!(core.solve_option(&shared, &option, None, solve).unwrap().is_finite());

    shared.market.note_released_slot(3, u64::MAX);
    core.cache_instrument(202, 3);
    publish();
    let why = core.solve_option(&shared, &option, None, solve)
        .expect_err("another contract's model cannot answer this option");
    assert_eq!(why.message, OPTION_MODEL_UNSTATED);
}

/// What a gateway refuses in an order, under its numbers and in its words,
/// and what it takes.
mod as_a_gateway_checks_it {
    use super::*;

    fn order() -> ApiOrder {
        ApiOrder {
            action: "BUY".into(), total_quantity: 1.0, order_type: "LMT".into(),
            lmt_price: 100.0, tif: "DAY".into(), ..Default::default()
        }
    }

    fn session(features: &[&str]) -> OrderSession {
        OrderSession {
            features: features.iter().map(|f| f.to_string()).collect(),
            ..OrderSession::single("DU1")
        }
    }

    fn refused(order: &ApiOrder, session: &OrderSession) -> (i32, String) {
        let why = ClientCore::validate_order(order, session).expect_err("refused");
        (why.code, why.message)
    }

    /// A trail is an amount or a percentage, not both, refused as a gateway
    /// refuses it while reading the order; the reference client's unset
    /// percentage is no percentage.
    #[test]
    fn a_trail_names_an_amount_or_a_percentage() {
        for order_type in ["TRAIL", "TRAIL LIMIT", "trailing stop"] {
            let both = ApiOrder {
                order_type: order_type.into(), aux_price: 0.25, trailing_percent: 1.5, ..order()
            };
            assert_eq!(
                refused(&both, &session(&[])),
                (320, "Error reading request: Cannot specify Trailing Amount and Trailing Percent at the same time".to_string()),
                "{order_type}",
            );
        }
        // A percentage outside what one can be, in a gateway's words.
        for percent in [150.0, -1.0] {
            let wide = ApiOrder { order_type: "TRAIL".into(), trailing_percent: percent, ..order() };
            assert_eq!(
                refused(&wide, &session(&[])),
                (321, "Invalid Trailing Percent value. Valid values are greater than 0 and less than 100.".to_string()),
                "{percent}",
            );
        }
        let whole = ApiOrder { order_type: "TRAIL".into(), trailing_percent: 100.0, ..order() };
        ClientCore::validate_order(&whole, &session(&[])).expect("a hundred per cent is a percentage");
        // A trailing stop limit by percentage is refused rather than guessed at.
        let by_percent = ApiOrder {
            order_type: "TRAIL LIMIT".into(), trailing_percent: 1.5, lmt_price_offset: 0.1, ..order()
        };
        let (code, why) = refused(&by_percent, &session(&[]));
        assert_eq!(code, 321);
        assert!(why.starts_with("trailing_percent on a TRAIL LIMIT order is not carried by this client"), "{why}");
        let amount = ApiOrder {
            order_type: "TRAIL".into(), aux_price: 0.25, trailing_percent: f64::MAX, ..order()
        };
        ClientCore::validate_order(&amount, &session(&[])).expect("an amount alone is a trail");
        match ClientCore::build_order_request(&amount, 1, 0, None).unwrap() {
            ControlCommand::Order(OrderRequest::SubmitEx { kind: OrderKind::TrailingStop { trail_amt, .. }, .. }) => {
                assert_eq!(trail_amt, PRICE_SCALE / 4, "the amount, not a percentage of the unset value");
            }
            other => panic!("a trailing stop by amount: {other:?}"),
        }
    }

    /// The one order option a gateway knows, and what it answers for others.
    #[test]
    fn an_order_option_is_checked_as_a_gateway_checks_it() {
        let with = |tag: &str, value: &str| ApiOrder {
            order_misc_options: vec![crate::types::model::TagValue { tag: tag.into(), value: value.into() }],
            ..order()
        };
        ClientCore::validate_order(&with("manual", "1"), &session(&[])).expect("the one it knows");
        assert_eq!(
            refused(&with("foo", "1"), &session(&[])),
            (10337, "Misc options key=foo is invalid in PlaceOrder(3) request. Valid keys are: manual".to_string()),
        );
        assert_eq!(
            refused(&with("manual", "2"), &session(&[])),
            (10338, "Misc options value=2 is invalid for key=manual in PlaceOrder(3) request. Valid values are: 0, 1".to_string()),
        );
        // Lifted at logon: any key is taken, and the one it knows still has
        // to read as one of its values.
        ClientCore::validate_order(&with("foo", "1"), &session(&["NOAPIMISCVLD"])).expect("lifted");
        assert_eq!(
            refused(&with("manual", "2"), &session(&["NOAPIMISCVLD"])),
            (321, "Order: 'manual' has wrong value=2, expected [1 or 0]".to_string()),
        );
        // Not a key and a value: refused while the order is read, lifted or not.
        for features in [&[][..], &["NOAPIMISCVLD"][..]] {
            assert_eq!(
                refused(&with("manual", ""), &session(features)),
                (320, "Error reading request:Please use 'Key=Value' format for Misc Options".to_string()),
            );
        }
        // Lifted, the value is read as a number; checked, it is read as written.
        for value in [" 1", "01", "+1", "0 "] {
            ClientCore::validate_order(&with("manual", value), &session(&["NOAPIMISCVLD"]))
                .unwrap_or_else(|e| panic!("{value:?} reads as a number: {e:?}"));
            assert_eq!(refused(&with("manual", value), &session(&[])).0, 10338, "{value:?}");
        }
    }

    /// Every request's option list is read and checked as a gateway reads and
    /// checks it: one rule, the request's own name and keys, and the words its
    /// later check of `manual` uses.
    #[test]
    fn every_option_list_is_checked_as_a_gateway_checks_it() {
        use crate::client_core::{
            CHART_OPTIONS, HISTORICAL_NEWS_OPTIONS, HISTORICAL_TICKS_OPTIONS, IMPL_VOL_OPTIONS,
            MKT_DATA_OPTIONS, MKT_DEPTH_OPTIONS, NEWS_ARTICLE_OPTIONS, OPT_PRC_OPTIONS, ORDER_OPTIONS,
            OptionList, REAL_TIME_BARS_OPTIONS, SCANNER_OPTIONS,
        };
        let list = |pairs: &[(&str, &str)]| -> Vec<crate::types::model::TagValue> {
            pairs.iter().map(|(t, v)| crate::types::model::TagValue { tag: (*t).into(), value: (*v).into() }).collect()
        };
        let checked = |l: &OptionList, pairs: &[(&str, &str)]| {
            ClientCore::check_option_list(l, &ClientCore::written_options(&list(pairs)), &[])
                .map_err(|r| (r.code, r.message))
        };
        let lifted = |l: &OptionList, pairs: &[(&str, &str)]| {
            ClientCore::check_option_list(
                l, &ClientCore::written_options(&list(pairs)), &["NOAPIMISCVLD".to_string()],
            )
            .map_err(|r| (r.code, r.message))
        };
        let unreadable = Err((320, "Error reading request:Please use 'Key=Value' format for Misc Options".to_string()));
        for (l, name, later) in [
            (&ORDER_OPTIONS, "PlaceOrder(3)", "Order"),
            (&MKT_DATA_OPTIONS, "ReqMktData(1)", "Market data"),
            (&MKT_DEPTH_OPTIONS, "ReqMktDepth(10)", "Market data"),
            (&CHART_OPTIONS, "ReqHistoricalData(20)", "Historical data"),
            (&SCANNER_OPTIONS, "ReqScannerSubscription(22)", "Historical data"),
            (&REAL_TIME_BARS_OPTIONS, "ReqRealTimeBars(50)", "Historical data"),
            (&NEWS_ARTICLE_OPTIONS, "ReqNewsArticle(84)", "Historical data"),
            (&HISTORICAL_NEWS_OPTIONS, "ReqHistoricalNews(86)", "Historical data"),
            (&HISTORICAL_TICKS_OPTIONS, "ReqHistoricalTicks(96)", "Market data"),
        ] {
            assert_eq!(checked(l, &[]), Ok(()), "{name}");
            assert_eq!(checked(l, &[("manual", "1")]), Ok(()), "{name}");
            assert_eq!(checked(l, &[("manual", "0")]), Ok(()), "{name}");
            assert_eq!(
                checked(l, &[("foo", "1")]),
                Err((10337, format!("Misc options key=foo is invalid in {name} request. Valid keys are: manual"))),
            );
            assert_eq!(
                checked(l, &[("manual", "2")]),
                Err((10338, format!("Misc options value=2 is invalid for key=manual in {name} request. Valid values are: 0, 1"))),
            );
            // An entry with no key or no value cannot be read, lifted or not.
            for broken in [&[("manual", "")][..], &[("", "1")], &[("manual", "=")]] {
                assert_eq!(checked(l, broken), unreadable, "{name} {broken:?}");
                assert_eq!(lifted(l, broken), unreadable, "{name} {broken:?}");
            }
            // Lifted: any key is taken, and `manual` still has to read as nought
            // or one, trimmed and read as a number.
            assert_eq!(lifted(l, &[("foo", "1")]), Ok(()), "{name}");
            assert_eq!(
                lifted(l, &[("manual", "2")]),
                Err((321, format!("{later}: 'manual' has wrong value=2, expected [1 or 0]"))),
            );
            for reads in [" 1", "01", "+1", "-0", "0 "] {
                assert_eq!(lifted(l, &[("manual", reads)]), Ok(()), "{name} {reads:?}");
                assert_eq!(checked(l, &[("manual", reads)]).unwrap_err().0, 10338, "{name} {reads:?}");
            }
            assert_eq!(lifted(l, &[("manual", "\u{a0}1")]).unwrap_err().0, 321, "{name}");
            // A gateway reads any script's digits; one this client cannot read
            // is let through rather than refused on a reading it may not share.
            assert_eq!(lifted(l, &[("manual", "\u{661}")]), Ok(()), "{name}");
        }
        // Checked in the order a gateway's table holds the keys, not the
        // caller's: `foo` is named before `manual`, and `bar` before `foo`.
        assert_eq!(checked(&MKT_DATA_OPTIONS, &[("manual", "2"), ("foo", "1")]).unwrap_err().0, 10337);
        assert!(checked(&MKT_DATA_OPTIONS, &[("foo", "1"), ("bar", "1")]).unwrap_err().1.contains("key=bar"));
        // A key named twice keeps its last value.
        assert_eq!(checked(&CHART_OPTIONS, &[("manual", "2"), ("manual", "1")]), Ok(()));
        // What separates entries inside a value separates them there too.
        assert!(checked(&HISTORICAL_TICKS_OPTIONS, &[("manual", "1;foo=2")]).unwrap_err().1.contains("key=foo"));
        // A key is matched as written.
        assert_eq!(
            checked(&MKT_DATA_OPTIONS, &[("Manual", "1")]),
            Err((10337, "Misc options key=Manual is invalid in ReqMktData(1) request. Valid keys are: manual".into())),
        );
        // Lifted, a value is trimmed of what sorts at or below a space.
        assert_eq!(lifted(&MKT_DATA_OPTIONS, &[("manual", "\u{1}1")]), Ok(()));
        // The two option calculations take no key: an empty list states
        // nothing, any key is refused with none named, and lifted, anything
        // that reads as a key and a value is taken, with no later check.
        for (l, name) in [(&IMPL_VOL_OPTIONS, "ReqCalcImpliedVolatility(54)"), (&OPT_PRC_OPTIONS, "ReqCalcOptionPrice(55)")] {
            assert_eq!(checked(l, &[]), Ok(()), "{name}");
            assert_eq!(
                checked(l, &[("manual", "1")]),
                Err((10337, format!("Misc options key=manual is invalid in {name} request. Valid keys are: "))),
            );
            assert_eq!(lifted(l, &[("foo", "1")]), Ok(()), "{name}");
            assert_eq!(lifted(l, &[("manual", "2")]), Ok(()), "{name}");
            assert_eq!(lifted(l, &[("manual", "")]), unreadable, "{name}");
        }
        // And the order path is the same rule.
        let order = ApiOrder {
            order_misc_options: list(&[("manual", "")]),
            ..order()
        };
        assert_eq!(refused(&order, &session(&["NOAPIMISCVLD"])).0, 320);
    }

    #[test]
    fn retired_order_instructions_follow_the_session_and_check_order() {
        let mut retired = ApiOrder {
            e_trade_only: true, firm_quote_only: true, nbbo_price_cap: 0.0,
            opt_out_smart_routing: true, ..order()
        };
        let enabled = session(&["DEPRETFQNC", "DEPRPREFBEST"]);
        assert_eq!(refused(&retired, &enabled), (10268, "The 'EtradeOnly' order attribute is not supported.".into()));
        retired.e_trade_only = false;
        assert_eq!(refused(&retired, &enabled), (10269, "The 'FirmQuoteOnly' order attribute is not supported.".into()));
        retired.firm_quote_only = false;
        assert_eq!(refused(&retired, &enabled), (10270, "The 'NbboPriceCap' order attribute is not supported.".into()));
        retired.nbbo_price_cap = f64::MAX;
        assert_eq!(refused(&retired, &enabled).0, 10348);

        for cap in [0.0, -1.0, 100.0] {
            retired.nbbo_price_cap = cap;
            assert_eq!(refused(&retired, &enabled).0, 10270, "{cap} states a cap");
        }
        retired.opt_out_smart_routing = false;
        for cap in [f64::MAX, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            retired.nbbo_price_cap = cap;
            ClientCore::validate_order(&retired, &enabled).expect("no finite cap stated");
        }

        retired.e_trade_only = true;
        retired.order_misc_options = vec![crate::types::model::TagValue { tag: "other".into(), value: "1".into() }];
        assert_eq!(refused(&retired, &enabled).0, 10337, "the list is read first");
        retired.action.clear();
        assert_eq!(refused(&retired, &enabled).0, 10337, "before ordinary order checks too");
        retired.order_misc_options.clear();
        assert_ne!(refused(&retired, &enabled).0, 10268, "ordinary fields precede retired instructions");
        retired.action = "BUY".into();
        retired.what_if = true;
        retired.transmit = false;
        assert_eq!(refused(&retired, &enabled).0, 321, "preview checks precede retired instructions");
    }

    /// Field errors precede retired instructions, including a missing trigger.
    #[test]
    fn retired_order_instructions_follow_the_trigger_price_check() {
        let mut stopped = ApiOrder {
            order_type: "STP".into(), aux_price: 0.0, e_trade_only: true, ..order()
        };
        let enabled = session(&["DEPRETFQNC"]);
        assert_eq!(refused(&stopped, &enabled).0, 403);
        stopped.aux_price = 10.0;
        assert_eq!(refused(&stopped, &enabled).0, 10268);
    }

    /// A preview is not held to the trigger price the order it previews
    /// needs, and an algorithm's order may leave its type unnamed.
    #[test]
    fn previews_and_algorithms_skip_the_trigger_and_type_checks() {
        let preview = ApiOrder { what_if: true, order_type: "STP".into(), aux_price: 0.0, ..order() };
        ClientCore::validate_order(&preview, &session(&[])).expect("a preview of a stop without its trigger");
        let algorithm = ApiOrder { algo_strategy: "Adaptive".into(), order_type: String::new(), ..order() };
        ClientCore::validate_order(&algorithm, &session(&[])).expect("an algorithm's order with its type unnamed");
    }

    #[test]
    fn retired_order_instructions_are_checked_for_previews_and_algorithms() {
        for (what_if, strategy) in [(true, ""), (false, "Adaptive")] {
            let stated = ApiOrder {
                what_if, algo_strategy: strategy.into(), e_trade_only: true, ..order()
            };
            assert_eq!(refused(&stated, &session(&["DEPRETFQNC"])).0, 10268);
        }
    }

    #[test]
    fn an_orders_manual_value_is_checked_after_its_preview_and_trail() {
        let mut stated = ApiOrder {
            order_misc_options: vec![crate::types::model::TagValue {
                tag: "manual".into(), value: "2".into(),
            }], what_if: true, transmit: false, e_trade_only: true, ..order()
        };
        let lifted = session(&["NOAPIMISCVLD", "DEPRETFQNC"]);
        assert_eq!(refused(&stated, &lifted), (321,
            "What-If order should have transmit flag set to TRUE ".into()));
        stated.transmit = true;
        stated.order_type = "TRAIL".into();
        stated.trailing_percent = 150.0;
        assert_eq!(refused(&stated, &lifted), (321,
            "Invalid Trailing Percent value. Valid values are greater than 0 and less than 100.".into()));
        stated.trailing_percent = 1.0;
        assert_eq!(refused(&stated, &lifted), (321,
            "Order: 'manual' has wrong value=2, expected [1 or 0]".into()));
        assert_eq!(refused(&stated, &session(&["DEPRETFQNC"])).0, 10338,
            "without the feature, the value is refused while reading");
    }

    #[test]
    fn an_orders_option_list_is_read_before_its_fields() {
        let order = ApiOrder {
            order_misc_options: vec![crate::types::model::TagValue {
                tag: "unknown".into(), value: "1".into(),
            }], ..Default::default()
        };
        assert_eq!(refused(&order, &session(&[])).0, 10337);
    }

    #[test]
    fn retired_order_instructions_are_warned_in_order_and_removed() {
        let retired = ApiOrder {
            e_trade_only: true, firm_quote_only: true, nbbo_price_cap: 0.0,
            opt_out_smart_routing: true, account: "DU1".into(), ..order()
        };
        let held = session(&[]);
        ClientCore::validate_order(&retired, &held).expect("accepted with warnings");
        let (warnings, refused_by) = ClientCore::retired_instructions(&retired, &held);
        assert!(refused_by.is_none());
        assert_eq!(warnings.iter().map(|w| w.code).collect::<Vec<_>>(), [2168, 2169, 2170, 2181]);
        for (warning, name) in warnings.iter().zip(["EtradeOnly", "FirmQuoteOnly", "NbboPriceCap", "OptOutFromSmartRouting"]) {
            assert_eq!(warning.message, format!("The '{name}' order attribute is not supported."));
        }
        let sent = ClientCore::as_sent(&retired, &held);
        assert!(!sent.e_trade_only && !sent.firm_quote_only);
        assert_eq!(sent.nbbo_price_cap, f64::MAX);
        assert!(sent.account.is_empty());
        assert!(retired.e_trade_only && retired.firm_quote_only, "the caller's order stands");
        assert_eq!(retired.nbbo_price_cap, 0.0);
        assert_eq!(ClientCore::retired_instructions(&order(), &held), (vec![], None));

        // A cap that is not finite states none: nothing is warned and
        // nothing refused, with or without the feature.
        for cap in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let capped = ApiOrder { nbbo_price_cap: cap, ..order() };
            for features in [&[][..], &["DEPRETFQNC"][..]] {
                assert_eq!(ClientCore::retired_instructions(&capped, &session(features)), (vec![], None), "{cap}");
            }
        }
        // A cap stated alone still leaves the order without it.
        let capped = ApiOrder { nbbo_price_cap: 5.0, ..order() };
        assert_eq!(ClientCore::as_sent(&capped, &held).nbbo_price_cap, f64::MAX);
    }

    /// Declining smart routing: refused where the venue withdrew it, and
    /// otherwise placed without it and warned about.
    #[test]
    fn declining_smart_routing_is_answered_as_a_gateway_answers_it() {
        let declining = ApiOrder { opt_out_smart_routing: true, ..order() };
        let text = "The 'OptOutFromSmartRouting' order attribute is not supported.".to_string();
        assert_eq!(refused(&declining, &session(&["DEPRPREFBEST"])), (10348, text.clone()));
        ClientCore::validate_order(&declining, &session(&[])).expect("placed without it");
        let warned = ClientCore::retired_instructions(&declining, &session(&[])).0.pop().expect("and warned");
        assert_eq!((warned.code, warned.message), (2181, text));
        assert!(ClientCore::retired_instructions(&order(), &session(&[])).0.is_empty(), "an ordinary order is not warned");
    }

    /// The kinds of preview a released gateway takes, and its words for the
    /// rest.
    #[test]
    fn a_preview_kind_is_checked_as_a_gateway_checks_it() {
        let asked = |what_if: bool, what_if_type: i32| ApiOrder { what_if, what_if_type, ..order() };
        for kind in [i32::MAX, 0, 1] {
            ClientCore::validate_order(&asked(true, kind), &session(&[])).expect("the ordinary preview");
        }
        assert_eq!(
            refused(&asked(false, 1), &session(&[])),
            (321, "What-If type specified but What-If flag is not set. Orders with whatIfType must have whatIf=true.".to_string()),
        );
        assert_eq!(
            refused(&asked(true, 2), &session(&[])),
            (321, "What-If type 2 is not supported for your account configuration.".to_string()),
        );
        assert_eq!(
            refused(&asked(true, -1), &session(&[])),
            (321, "What-If type -1 is not supported for your account configuration.".to_string()),
        );
        let held = ApiOrder { transmit: false, ..asked(true, i32::MAX) };
        assert_eq!(
            refused(&held, &session(&[])),
            (321, "What-If order should have transmit flag set to TRUE ".to_string()),
        );
    }

    /// The hedging leg's short sale is refused only where the order is itself
    /// a short sale naming a hedging order type.
    #[test]
    fn a_hedging_short_sale_is_refused_only_on_a_short_sale() {
        let hedged = ApiOrder {
            delta_neutral_short_sale: true, delta_neutral_order_type: "MKT".into(), ..order()
        };
        ClientCore::validate_order(&hedged, &session(&[])).expect("on a buy it is taken");
        let short = ApiOrder { action: "SSHORT".into(), ..hedged };
        assert_eq!(refused(&short, &session(&[])).0, 321);
    }

    /// A login whose logon names these accounts as its own.
    fn logon(accounts: &[&str]) -> OrderSession {
        let accounts: Vec<String> = accounts.iter().map(|a| a.to_string()).collect();
        OrderSession {
            account: accounts[0].clone(), accounts: accounts.clone(), logon_accounts: accounts,
            ..OrderSession::default()
        }
    }

    /// The account an order is for, on each kind of login.
    #[test]
    fn an_order_names_the_account_its_login_needs() {
        let several = logon(&["U1", "U2"]);
        assert_eq!(refused(&order(), &several), (321, "You must specify an account.".to_string()));
        let named = ApiOrder { account: "U2".into(), ..order() };
        ClientCore::validate_order(&named, &several).expect("a named account goes");
        assert_eq!(ClientCore::as_sent(&named, &several).account, "U2", "and goes out on it");
        let added = session(&["DYNACCTADD"]);
        assert_eq!(refused(&order(), &added).1, "You must specify an account.", "a login accounts are added to");
        let master = logon(&["I1234"]);
        assert_eq!(refused(&order(), &master).1, "You must specify an account.", "an introducing broker's master");
        let foreign = ApiOrder { account: "U9".into(), ..order() };
        ClientCore::validate_order(&foreign, &session(&[])).expect("one account: any name goes");
        assert_eq!(ClientCore::as_sent(&foreign, &session(&[])).account, "", "on the login's own");
        // What a gateway does not count: a group's code, and an account the
        // login's family links to it.
        ClientCore::validate_order(&order(), &logon(&["U1", "AG123"])).expect("a group is not an account");
        let family = OrderSession { accounts: vec!["U1".into(), "U2".into()], ..logon(&["U1"]) };
        ClientCore::validate_order(&order(), &family).expect("a family's account is not the login's own");
        assert_eq!(ClientCore::as_sent(&foreign, &family).account, "", "and the login states its own");
        // An advisor's order is allocated, and names no account to be taken.
        let advisor = OrderSession { advisor: true, ..logon(&["F1", "U1", "U2"]) };
        let allocated = ApiOrder { fa_group: "Everyone".into(), fa_method: "EqualQuantity".into(), ..order() };
        ClientCore::validate_order(&allocated, &advisor).expect("an advisor's order names no account");
        ClientCore::validate_order(&order(), &advisor).expect("with or without an allocation");
    }

    /// The account an exercise is taken on, on each kind of login.
    #[test]
    fn an_exercise_names_an_account_as_a_gateway_needs_it() {
        let several = logon(&["U1", "U2"]);
        let why = ClientCore::validate_exercise(1, 1, "", &several).unwrap_err();
        assert_eq!((why.code, why.message.as_str()), (321, "The account code is required for this operation."));
        let why = ClientCore::validate_exercise(1, 1, "U9", &several).unwrap_err();
        assert_eq!((why.code, why.message.as_str()), (321, "Invalid account code 'U9'."));
        assert_eq!(ClientCore::validate_exercise(1, 1, "U2", &several).unwrap(), (1, 1, "U2".to_string()));
        let one = session(&[]);
        let why = ClientCore::validate_exercise(1, 1, "U9", &one).unwrap_err();
        assert_eq!(
            (why.code, why.message.as_str()),
            (322, "Error processing request:No unlapsed position exists in this option in account U9."),
        );
        assert_eq!(ClientCore::validate_exercise(2, 1, "DU1", &one).unwrap(), (2, 1, String::new()));
        assert_eq!(ClientCore::validate_exercise(2, 1, "", &one).unwrap(), (2, 1, String::new()));
    }

    /// Every name a gateway takes an order type under builds the order its
    /// own name does; a name that is no type is refused and says so.
    #[test]
    fn an_order_type_answers_to_every_name_a_gateway_gives_it() {
        let kind_of = |name: &str| {
            let named = ApiOrder { order_type: name.into(), aux_price: 1.0, ..order() };
            ClientCore::validate_order(&named, &session(&[]))
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            match ClientCore::build_order_request(&named, 1, 0, None).unwrap() {
                ControlCommand::Order(OrderRequest::SubmitEx { kind, .. }) => std::mem::discriminant(&kind),
                other => panic!("{name}: {other:?}"),
            }
        };
        for (canonical, aliases) in [
            ("MKT", &["MARKET", "market"][..]),
            ("LMT", &["LIMIT"]),
            ("STP", &["STOP"]),
            ("STP LMT", &["STPLMT", "STOP LIMIT", "STOPLIMIT", "STOPLMT", "STOP_LIMIT"]),
            ("STP PRT", &["STPPRT", "STOP PROTECT"]),
            ("TRAIL", &["TRAILING STOP", "TRAILING_STOP"]),
            ("TRAIL LIMIT", &["TRAILLIMIT", "TRAILLMT", "TRAILING_STOP_LIMIT"]),
            ("MOC", &["MKT CLS", "MKTCLS", "MARKETONCLOSE"]),
            ("LOC", &["LMT CLS", "LMTCLS", "LIMITONCLOSE"]),
            ("MTL", &["MKT TO LMT", "MKTTOLMT", "MARKET_TO_LIMIT", "BOX TOP", "BOXTOP", "BOX_TOP"]),
            ("MKT PRT", &["MKTPRT", "MKT_PROTECT"]),
            ("REL", &["RELATIVE", "PEG PRIM"]),
            ("PEG MKT", &["PEGMKT"]),
            ("PEG MID", &["PEGMID"]),
            ("PEG BEST", &["PEGBEST", "PEG BEST "]),
            ("PEG BENCH", &["PEGBENCH", "PEG_TO_BENCH"]),
            ("SNAP MID", &["SNAPMID"]),
            ("SNAP MKT", &["SNAPMKT"]),
            ("SNAP PRIM", &["SNAPPRIM"]),
        ] {
            for alias in aliases {
                assert_eq!(kind_of(alias), kind_of(canonical), "{alias} is {canonical}");
            }
        }
        let unknown = ApiOrder { order_type: "XYZ".into(), ..order() };
        assert_eq!(
            refused(&unknown, &session(&[])),
            (387, "Unsupported order type: 'XYZ' is not an order type this client places".to_string()),
        );
    }

    /// A replace moving an order's group, or the way it cancels, is refused
    /// under a gateway's numbers, unless the venue lifted that at logon.
    #[test]
    fn a_replace_moving_a_group_is_refused_unless_the_venue_lifted_it() {
        let core = ClientCore::new();
        let placed = ApiOrder { oca_group: "A".into(), oca_type: 1, ..order() };
        core.track_order(42, ApiContract::default(), placed.clone(), 0);
        let why = core.modify_refusal(42, &ApiOrder { oca_group: "B".into(), ..placed.clone() }, None).unwrap();
        assert_eq!((why.code, why.message.as_str()), (10326, "OCA group revision is not allowed"));
        let why = core.modify_refusal(42, &ApiOrder { oca_type: 2, ..placed.clone() }, None).unwrap();
        assert_eq!((why.code, why.message.as_str()), (10327, "OCA group type revision is not allowed"));
        assert!(core.modify_refusal(42, &ApiOrder { oca_group: String::new(), ..placed.clone() }, None).is_none(), "naming no group keeps it");
        // Naming no way to cancel names the default, which is not the first.
        let why = core.modify_refusal(42, &ApiOrder { oca_type: 0, ..placed.clone() }, None).unwrap();
        assert_eq!(why.code, 10327, "nought is the default way, not none");
        assert!(core.modify_refusal(42, &ApiOrder { parent_id: 9, ..placed.clone() }, None).is_none(), "a parent is taken");
        // The way is compared whether or not a group is named, and a way
        // outside the four reads as the default.
        core.track_order(43, ApiContract::default(), order(), 0);
        let grouped = ApiOrder { oca_group: "A".into(), oca_type: 1, ..order() };
        assert_eq!(core.modify_refusal(43, &grouped, None).unwrap().code, 10327, "a group added with another way");
        core.track_order(44, ApiContract::default(), ApiOrder { oca_type: 3, ..order() }, 0);
        assert!(core.modify_refusal(44, &ApiOrder { oca_type: 5, ..order() }, None).is_none(), "five reads as three");
        let shared = SharedState::new();
        shared.reference.set_enabled_features(vec!["NOAPIOCASTRICT".into()]);
        assert!(core.modify_refusal(42, &ApiOrder { oca_group: "B".into(), ..placed }, Some(&shared)).is_none(), "lifted");
    }

    /// The account the venue states an order is on is the one read back,
    /// whatever the order named.
    #[test]
    fn the_open_order_view_reads_the_account_the_venue_states() {
        let core = ClientCore::new();
        let shared = SharedState::new();
        let named = ApiOrder { account: "U9".into(), ..order() };
        core.track_order(42, ApiContract::default(), named.clone(), 0);
        shared.orders.push_order_info(42, RichOrderInfo {
            contract: ApiContract::default(),
            order: ApiOrder { account: "DU1".into(), ..named },
            order_state: ApiOrderState { status: "Submitted".into(), ..Default::default() },
            last_exec: Default::default(),
        });
        let read = core.collect_open_orders(&shared).into_iter().find(|(id, _)| *id == 42).unwrap();
        assert_eq!(read.1.order.account, "DU1");
    }
}

/// A window in days or dates is settled as a gateway settles it: one to seven
/// days back counting today, dates within the last seven days, and no window
/// at all where it asks for no more than today's executions.
#[test]
fn an_execution_window_is_settled_as_a_gateway_settles_it() {
    let today = jiff::civil::date(2026, 9, 24);
    let back = |days: i64| today.saturating_sub(jiff::Span::new().days(days));
    let ymd = |d: jiff::civil::Date| d.strftime("%Y%m%d").to_string().parse::<i32>().unwrap();
    for asks_for_no_window in [0, 1, 8, -3, i32::MAX] {
        assert_eq!(execution_days(asks_for_no_window, &[], today), Ok(None), "{asks_for_no_window}");
    }
    assert_eq!(execution_days(2, &[], today), Ok(Some(vec![today, back(1)])));
    assert_eq!(execution_days(7, &[], today).map(|d| d.map(|d| d.len())), Ok(Some(7)));
    assert_eq!(execution_days(0, &[ymd(back(5))], today), Ok(Some(vec![back(5)])));
    assert_eq!(execution_days(0, &[ymd(back(7))], today), Ok(None), "a week back is outside the window");
    assert_eq!(execution_days(0, &[ymd(today)], today), Ok(None), "today alone");
    assert_eq!(execution_days(0, &[ymd(today), ymd(today)], today), Ok(None), "today named twice is today");
    assert_eq!(execution_days(2, &[ymd(back(5))], today), Ok(Some(vec![today, back(1), back(5)])));
    assert_eq!(execution_days(0, &[3, 8], today), Ok(None), "eight or less is dropped");
    assert_eq!(execution_days(0, &[99_991_231, 100_000_101], today), Ok(None), "far off, and dropped");
    // Not a day of the calendar: a gateway refuses the request as it reads it.
    for not_a_day in [20260231, 20261301, 20260900, 20260932, 9, 20_261_332] {
        let refused = execution_days(0, &[not_a_day], today).unwrap_err();
        assert_eq!(refused.code, 320, "{not_a_day}");
    }
    assert_eq!(execution_days(0, &[20240229], today), Ok(None), "a leap day is a day");
}

/// The executions answered are those on the days asked for, each day counted
/// on the session's clock.
#[test]
fn executions_are_answered_for_the_days_asked() {
    let core = ClientCore::new();
    for (id, time) in [
        ("today", "20260924-10:00:00"),
        ("two back", "20260922-10:00:00"),
        ("five back", "20260919-10:00:00"),
        ("late two back", "20260922-20:00:00"),
    ] {
        core.push_execution(
            ApiContract::default(),
            crate::types::model::Execution { exec_id: id.into(), time: time.into(), ..Default::default() },
            Default::default(),
        );
    }
    let now: jiff::Timestamp = "2026-09-24T12:00:00Z".parse().unwrap();
    let on = |zone: &str| {
        let shared = SharedState::new();
        shared.set_settings(std::sync::Arc::new(crate::settings::GatewaySettings {
            timezone: Some(zone.into()), ..Default::default()
        }.resolve()));
        shared
    };
    let answered = |last_n_days: i32, specific_dates: &[i32], zone: &str| {
        let filter = ExecutionFilter {
            last_n_days, specific_dates: specific_dates.to_vec(), ..Default::default()
        };
        let (rows, _) = core.executions_for_request(&on(zone), &[], &filter, now).unwrap();
        let mut ids: Vec<String> = rows.into_iter().map(|se| se.execution.exec_id).collect();
        ids.sort();
        ids
    };
    assert_eq!(answered(2, &[], "UTC"), ["today"]);
    assert_eq!(answered(3, &[], "UTC"), ["late two back", "today", "two back"]);
    assert_eq!(answered(8, &[], "UTC").len(), 4, "no window");
    assert_eq!(answered(1, &[], "UTC").len(), 4, "today alone is the executions held");
    assert_eq!(answered(0, &[20260919], "UTC"), ["five back"]);
    assert_eq!(answered(0, &[20260904], "UTC").len(), 4, "a date outside the week is dropped");
    assert_eq!(answered(2, &[20260919], "UTC"), ["five back", "today"]);
    // Nine hours east, the evening of the twenty-second is the morning of the
    // twenty-third, which is inside two days back.
    assert_eq!(answered(2, &[], "Asia/Tokyo"), ["late two back", "today"]);

    // A day that starts before what the session holds is answered with what
    // it holds, and named so the caller can be told. Held from midnight six
    // days back in UTC, a week counted in Tokyo reaches nine hours further.
    let tokyo = on("Asia/Tokyo");
    let held_from: jiff::Timestamp = "2026-09-18T00:00:00Z".parse().unwrap();
    tokyo.reference.set_executions_held_from(Some(held_from.as_second()));
    let filter = ExecutionFilter { last_n_days: 7, ..Default::default() };
    let (_, unheld) = core.executions_for_request(&tokyo, &[], &filter, now).unwrap();
    assert_eq!(unheld, [jiff::civil::date(2026, 9, 18)]);
    let utc = on("UTC");
    utc.reference.set_executions_held_from(tokyo.reference.executions_held_from());
    let (_, unheld) = core.executions_for_request(&utc, &[], &filter, now).unwrap();
    assert!(unheld.is_empty(), "{unheld:?}");
    let (_, unheld) = core.executions_for_request(&tokyo, &[], &ExecutionFilter::default(), now).unwrap();
    assert!(unheld.is_empty(), "no window, nothing to say");

    // And a date that is not a day refuses the request as a gateway reads it.
    let filter = ExecutionFilter { specific_dates: vec![20260231], ..Default::default() };
    let Err(refused) = core.executions_for_request(&utc, &[], &filter, now) else { panic!("answered") };
    assert_eq!(refused.code, 320);
}

#[test]
fn account_summary_keeps_account_and_ledger_figures_apart() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.portfolio.note_account_value("AccruedCash", "-2580.38", "CHF");
    shared.portfolio.note_ledger_value("AccruedCash", "-238.28", "CHF");
    shared.portfolio.account_download_is_settled();
    core.subscribe_account_summary(1, "All", vec![String::new()]).unwrap();
    let initial = core.prepare_account_summary(&shared, "DU1").unwrap();
    assert_eq!(initial.entries.iter().map(|entry| entry.value.as_str()).collect::<Vec<_>>(), ["-2580.38", "-238.28"]);
    core.last_account_summary.lock().unwrap().get_mut(&1).unwrap().0 -= std::time::Duration::from_secs(180);
    assert!(core.prepare_account_summary(&shared, "DU1").is_none(), "unchanged account and ledger rows do not repeat");
    shared.portfolio.note_account_value("AccruedCash", "-2579.70", "CHF");
    core.last_account_summary.lock().unwrap().get_mut(&1).unwrap().0 -= std::time::Duration::from_secs(180);
    let moved = core.prepare_account_summary(&shared, "DU1").unwrap();
    assert_eq!(moved.entries.len(), 1);
    assert_eq!(moved.entries[0].value, "-2579.70");
}

#[test]
fn named_accounts_keep_their_profit_and_summary_figures_separate() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.set_session_account("DU1");
    for (account, daily, net) in [("DU1", 10, "100"), ("DU2", 20, "200")] {
        let portfolio = shared.portfolio_for(account);
        portfolio.set_account(&crate::types::AccountState {
            daily_pnl: daily * PRICE_SCALE, ..Default::default()
        });
        portfolio.note_account_value("NetLiquidation", net, "USD");
        portfolio.account_download_is_settled();
    }
    core.subscribe_pnl(1, "DU1").unwrap();
    core.subscribe_pnl(2, "DU2").unwrap();
    core.subscribe_pnl(3, "DU1").unwrap();
    assert_eq!(core.poll_pnl(&shared).iter().map(|u| (u.req_id, u.daily_pnl)).collect::<Vec<_>>(),
        [(1, 10.0), (2, 20.0), (3, 10.0)]);
    assert!(core.poll_pnl(&shared).is_empty());
    core.subscribe_account_summary(4, "NetLiquidation", vec!["DU1".into(), "DU2".into()]).unwrap();
    let summary = core.prepare_account_summary_for(&shared, 4).unwrap();
    assert_eq!(summary.entries.iter().map(|e| (e.account.as_str(), e.value.as_str())).collect::<Vec<_>>(),
        [("DU1", "100"), ("DU2", "200")]);
    shared.portfolio_for("DU2").set_account(&crate::types::AccountState { daily_pnl: 25 * PRICE_SCALE, ..Default::default() });
    assert_eq!(core.poll_pnl(&shared).iter().map(|u| (u.req_id, u.daily_pnl)).collect::<Vec<_>>(), [(2, 25.0)]);
}

#[test]
fn a_positions_profit_stays_with_its_named_account() {
    let core = ClientCore::new();
    let shared = SharedState::new();
    shared.set_session_account("DU1");
    for (account, position) in [("DU1", 3.0), ("DU2", 7.0)] {
        let portfolio = shared.portfolio_for(account);
        portfolio.set_position_info(crate::types::PositionInfo {
            con_id: 756733, position, avg_cost: 30 * PRICE_SCALE,
            market_price: 40 * PRICE_SCALE, ..Default::default()
        });
        portfolio.account_download_is_settled();
    }
    core.subscribe_pnl_single(1, 756733, "DU1").unwrap();
    core.subscribe_pnl_single(2, 756733, "DU2").unwrap();
    assert_eq!(core.subscribe_pnl_single(1, 756733, "DU2").unwrap_err().code, 102);
    let mut updates = core.poll_pnl_single(&shared);
    updates.sort_by_key(|u| u.req_id);
    assert_eq!(updates.iter().map(|u| (u.req_id, u.pos, u.daily_pnl)).collect::<Vec<_>>(),
        [(1, 3.0, 30.0), (2, 7.0, 70.0)]);
    core.unsubscribe_pnl_single(1);
    core.subscribe_pnl_single(1, 756733, "DU2").unwrap();
    let updates = core.poll_pnl_single(&shared);
    assert_eq!(updates.iter().map(|u| (u.req_id, u.pos)).collect::<Vec<_>>(), [(1, 7.0)]);
}

#[test]
fn a_snapshot_keeps_its_deadline_while_its_registration_waits_to_be_read() {
    let shared = SharedState::new();
    let core = ClientCore::new();
    core.note_mkt_data_taken(&shared, &crate::bridge::MarketDataTaken {
        req_id: 5, slot: 0, generation: 1, con_id: 756733, series: Vec::new(),
        snapshot: true, one_shot: false, data_type: data_type_for_mode(0), marked: false,
        asked_at: std::time::Instant::now() - std::time::Duration::from_secs(12),
    });
    assert!(core.check_snapshot_done(5), "a late read does not restart the wait");
}

#[test]
fn contract_expiry_accepts_the_months_dates_and_empty_values_a_gateway_takes() {
    for expiry in [
        "", "NOEXP", "noexp", "NoExP", "197801", "300012", "19780101", "30001231",
        "202602", "20260228", "20240229", "20000229", "20260430", "20260731",
        "noExp", "202607", "20260724",
    ] {
        assert!(ClientCore::validate_contract_expiry(expiry).is_ok(), "{expiry:?}");
    }
    for expiry in [
        " ", " NOEXP", "NOEXP ", "EXP", "00000000", "197712", "300101", "202600",
        "202613", "20260900", "20260931", "20260229", "21000229", "20260431",
        "202601011", "2026", "2026-09", "2026/09/01", "20260901 12:00:00",
        "20260901-12:00:00", "20260901 UTC", "202609 ", "+02609", "2026+9",
        "２０２６０９", "2026é", "202609\0", "202609\n",
        "2026-07", "20260732", "20250229", "19771231", "30010101", "2026072",
        "202607241", " 202607", "+20607", "２０２６０７",
    ] {
        let why = ClientCore::validate_contract_expiry(expiry).unwrap_err();
        assert_eq!(why.code, 10372, "{expiry:?}");
        assert_eq!(why.message, "lastTradeDateOrContractMonth: The date entered is invalid. The correct format is yyyyMM for a contract month or yyyyMMdd for a date. E.g.: 202607 or 20260724.");

    }
}
