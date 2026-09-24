//! Preset and contract answers an attached family waits for in the engine.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, mpsc};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

use crate::bridge::SharedState;
use crate::client_core::OrderSession;
use crate::client_core::attached_loading::{self, Request};
use crate::client_core::attached_orders::contract_definition;
use crate::control::attached_presets::AttachedPreset;
use crate::error_codes::Refusal;
use crate::types::model::Contract;

use super::HotLoop;

type Loaded = Result<(AttachedPreset, bool), Refusal>;

type Naming = (Contract, Option<u32>, mpsc::Sender<Result<(), Refusal>>);

pub(super) struct Loading {
    future: Pin<Box<dyn Future<Output = Loaded> + Send>>,
    requests: mpsc::Receiver<Request>,
    names: Vec<Naming>,
}

impl std::fmt::Debug for Loading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loading").field("names", &self.names).finish_non_exhaustive()
    }
}

impl Loading {
    pub(super) fn new(
        shared: Arc<SharedState>,
        contract: Contract,
        session: OrderSession,
        deadline: Instant,
    ) -> Self {
        let (send, requests) = mpsc::channel();
        let future = Box::pin(async move {
            if contract_definition(&shared, &contract).is_none()
                && !(contract.exchange == "SMART"
                    && matches!(contract.sec_type.as_str(), "BAG" | "COMB" | "COMBO"))
            {
                attached_loading::name(&send, &contract, deadline).await?;
            }
            let regular = crate::client_core::attached_combos::resolve(
                &shared,
                &contract,
                &session,
                &send,
                deadline,
                |contract| {
                    let contract = contract.clone();
                    let send = send.clone();
                    Box::pin(
                        async move { attached_loading::name(&send, &contract, deadline).await },
                    )
                },
            )
            .await?;
            let definition = contract_definition(&shared, &contract).ok_or_else(|| {
                Refusal::no_answer(
                    "The contract definition needed for attached orders is unavailable",
                )
            })?;
            let preset = attached_loading::load_attached_preset(
                &shared,
                &send,
                &contract,
                &definition.under_sec_type,
                deadline,
            )
            .await?;
            Ok((preset, regular))
        });
        Self { future, requests, names: Vec::new() }
    }

    pub(super) fn poll(&mut self, engine: &mut HotLoop) -> Poll<Loaded> {
        for (mut contract, mut lookup, answer) in std::mem::take(&mut self.names) {
            match engine.name_order_contract(&mut contract, &mut lookup) {
                Ok(false) => self.names.push((contract, lookup, answer)),
                result => {
                    let _ = answer.send(result.map(|_| ()));
                }
            }
        }
        let result = self.future.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        while let Ok(request) = self.requests.try_recv() {
            self.send_attachment_request(engine, request);
        }
        result
    }

    fn send_attachment_request(&mut self, engine: &mut HotLoop, request: Request) {
        match request {
            Request::ConfirmAttachedCombo { request_key, fields } => {
                if let Err(error) =
                    engine.ccp.send_user_message(fields, &mut engine.ccp_conn, &mut engine.hb)
                {
                    engine.shared.reference.stop_waiting_for_combo_confirmation(&request_key);
                    log::warn!("combination confirmation {request_key} was not sent: {error}");
                }
            }
            Request::FetchAttachedComboRules { exchange } => {
                let fields = crate::control::attached_combos::rules_request(&exchange);
                if let Err(error) =
                    engine.ccp.send_user_message(fields, &mut engine.ccp_conn, &mut engine.hb)
                {
                    engine.shared.reference.forget_attached_combo_rule_request(&exchange);
                    log::warn!("combination rules for {exchange} were not requested: {error}");
                }
            }
            Request::FetchOrderPresetValues { request_key, key } => {
                let fields = crate::control::order_presets::values_request(&request_key, &key);
                if let Err(error) =
                    engine.ccp.send_user_message(fields, &mut engine.ccp_conn, &mut engine.hb)
                {
                    engine.shared.reference.stop_waiting_for_preset_values(&request_key);
                    log::warn!("order preset request {request_key} was not sent: {error}");
                }
            }
            Request::Name(mut contract, answer) => {
                let mut lookup = None;
                match engine.name_order_contract(&mut contract, &mut lookup) {
                    Ok(false) => self.names.push((contract, lookup, answer)),
                    result => {
                        let _ = answer.send(result.map(|_| ()));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::contracts::ContractDefinition;
    use crate::control::order_presets::PresetValues;
    use crate::protocol::connection::Connection;
    use crate::types::model::{ErrorOrigin, Order, OrderOp};
    use crate::types::{ControlCommand, OrderRequest, Placement};
    use std::io::Read;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    fn engine() -> (HotLoop, Arc<SharedState>, mpsc::Sender<ControlCommand>, std::net::TcpStream) {
        let shared = Arc::new(SharedState::new());
        shared.orders.set_replay_done();
        shared.reference.cache_contract_definition(ContractDefinition {
            con_id: 756733,
            exchange: "SMART".into(),
            order_type_key: "STK".into(),
            order_type_rules: vec![("STP".into(), 1), ("LMT".into(), 1), ("OCA".into(), 1)],
            order_types: vec!["STP".into(), "LMT".into(), "OCA".into()],
            ..Default::default()
        });
        shared.reference.set_order_presets(vec![("s=STK".into(), "a=1".into(), "1".into())]);
        let mut engine = HotLoop::new(shared.clone(), None, None);
        let (connection, peer) = Connection::for_test();
        peer.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        engine.ccp_conn = Some(connection);
        engine.set_account_id("DU1".into());
        let (send, receive) = mpsc::channel();
        engine.set_control_rx(receive);
        (engine, shared, send, peer)
    }

    fn placement(transmit: bool, quote: bool) -> ControlCommand {
        ControlCommand::Place(Box::new(Placement {
            order_id: 10,
            allocator: Arc::new(AtomicU64::new(13)),
            contract: Contract {
                con_id: 756733,
                symbol: "SPY".into(),
                sec_type: "STK".into(),
                exchange: "SMART".into(),
                currency: "USD".into(),
                ..Default::default()
            },
            order: Order {
                order_id: 10,
                transmit,
                override_percentage_constraints: !quote,
                sl_order_id: 11,
                sl_order_type: "PRESET".into(),
                pt_order_id: 12,
                pt_order_type: "PRESET".into(),
                ..Order::limit("BUY", 100.0, 100.0)
            },
            warnings: Vec::new(),
        }))
    }

    fn answer(shared: &SharedState, key: &str) {
        shared.reference.set_order_preset_values(PresetValues {
            request_key: key.into(),
            key: "s=STK".into(),
            attributes: "a=1".into(),
            error: None,
            fields: vec![
                (4074, "1".into()),
                (4075, "1".into()),
                (4076, "7".into()),
                (4083, "2".into()),
            ],
        });
    }

    fn wire(peer: &mut std::net::TcpStream) -> String {
        let mut text = String::new();
        let mut bytes = [0; 16384];
        while let Ok(n) = peer.read(&mut bytes) {
            if n == 0 {
                break;
            }
            text.push_str(&String::from_utf8_lossy(&bytes[..n]));
        }
        text.replace('\x01', "|")
    }

    fn submissions(wire: &str) -> Vec<String> {
        wire.split("35=D|")
            .skip(1)
            .map(|frame| {
                frame.split('|').find_map(|field| field.strip_prefix("11=")).unwrap().to_string()
            })
            .collect()
    }

    #[test]
    fn an_attached_preset_wait_keeps_one_admission_and_unrelated_orders_move() {
        let (mut engine, shared, send, mut peer) = engine();
        shared.admit(&send, placement(true, false)).unwrap();
        engine.poll_once();
        let request = wire(&mut peer);
        assert!(request.contains("8166=G|"), "{request}");
        assert_eq!(shared.backlog(), 1);
        let ControlCommand::Place(mut other) = placement(true, false) else { unreachable!() };
        other.order_id = 20;
        other.order = Order::limit("BUY", 1.0, 90.0);
        shared.admit(&send, ControlCommand::Place(other)).unwrap();
        engine.poll_once();
        engine.poll_once();
        assert_eq!(submissions(&wire(&mut peer)), ["20.0"]);
        assert_eq!(shared.backlog(), 1);
        answer(&shared, "OPR.3");
        engine.poll_once();
        engine.poll_once();
        assert_eq!(submissions(&wire(&mut peer)), ["10.0", "11.0", "12.0"]);
        assert_eq!(shared.backlog(), 0);
        assert!(shared.drain_refused().is_empty());
    }

    #[test]
    fn cancelling_an_attached_preset_wait_drops_its_late_answer() {
        let (mut engine, shared, send, mut peer) = engine();
        shared.admit(&send, placement(true, false)).unwrap();
        engine.poll_once();
        shared
            .admit(&send, ControlCommand::CancelOrder { order_id: 10, stated: Default::default() })
            .unwrap();
        engine.poll_once();
        answer(&shared, "OPR.3");
        engine.poll_once();
        engine.poll_once();
        assert!(submissions(&wire(&mut peer)).is_empty());
        assert_eq!(shared.backlog(), 0);
        assert!(shared.drain_refused().is_empty());
    }

    #[test]
    fn a_nontransmitting_attached_family_stays_unsent_at_shutdown() {
        let (mut engine, shared, send, mut peer) = engine();
        shared.admit(&send, placement(false, false)).unwrap();
        shared.admit(&send, ControlCommand::Logout).unwrap();
        shared.admit(&send, ControlCommand::Shutdown).unwrap();
        engine.poll_once();
        assert!(engine.is_running());
        answer(&shared, "OPR.3");
        engine.poll_once();
        assert!(!engine.is_running());
        let sent = wire(&mut peer);
        assert!(submissions(&sent).is_empty(), "{sent}");
        assert!(sent.contains("35=5|"), "{sent}");
        assert_eq!(shared.backlog(), 0);
        assert!(shared.drain_refused().is_empty());
    }

    #[test]
    fn a_transmitting_attached_family_finishes_its_quote_wait_before_logout() {
        let (mut engine, shared, send, mut peer) = engine();
        shared.admit(&send, placement(true, true)).unwrap();
        shared.admit(&send, ControlCommand::Logout).unwrap();
        shared.admit(&send, ControlCommand::Shutdown).unwrap();
        engine.poll_once();
        answer(&shared, "OPR.3");
        engine.poll_once();
        assert!(engine.is_running());
        assert_eq!(engine.attached_quote_orders.len(), 1);
        assert_eq!(shared.backlog(), 1);
        assert!(!wire(&mut peer).contains("35=5|"));
        engine.poll_attached_quotes(Instant::now() + Duration::from_secs(11));
        assert_eq!(engine.built_order_commands_held(), 1);
        engine.poll_once();
        let sent = wire(&mut peer);
        assert_eq!(submissions(&sent), ["10.0", "11.0", "12.0"]);
        assert!(sent.rfind("35=D|").unwrap() < sent.find("35=5|").unwrap(), "{sent}");
        assert!(!engine.is_running());
        assert_eq!(shared.backlog(), 0);
    }

    #[test]
    fn a_child_change_behind_a_preset_wait_keeps_its_place_in_the_family() {
        let (mut engine, shared, send, mut peer) = engine();
        shared.admit(&send, placement(false, false)).unwrap();
        let ControlCommand::Place(mut child) = placement(true, false) else { unreachable!() };
        child.order_id = 11;
        child.order = Order { parent_id: 10, ..Order::stop("SELL", 100.0, 95.0) };
        shared.admit(&send, ControlCommand::Place(child)).unwrap();
        engine.poll_once();
        assert_eq!(shared.backlog(), 2);
        answer(&shared, "OPR.3");
        engine.poll_control_commands();
        engine.work_through_orders(&mut 100);
        let orders: Vec<_> = engine.context.pending_orders.iter().collect();
        assert_eq!(orders.iter().map(|o| o.order_id()).collect::<Vec<_>>(), [10, 11, 12]);
        assert!(
            matches!(orders[1], OrderRequest::SubmitEx { kind: crate::types::OrderKind::Stop { stop_price }, .. } if *stop_price == crate::types::price_from_f64(95.0))
        );
        engine.poll_once();
        assert_eq!(submissions(&wire(&mut peer)), ["10.0", "11.0", "12.0"]);
    }

    #[test]
    fn an_unstated_child_id_is_reported_as_zero_and_only_its_first_order_is_kept() {
        let (mut engine, shared, send, mut peer) = engine();
        let ControlCommand::Place(mut parent) = placement(true, false) else { unreachable!() };
        parent.order.sl_order_id = i32::MAX;
        parent.order.pt_order_id = i32::MAX;
        parent.order.sl_order_type = "selected".into();
        parent.order.pt_order_type = "selected".into();
        shared.admit(&send, ControlCommand::Place(parent)).unwrap();
        engine.poll_once();
        answer(&shared, "OPR.3");
        engine.poll_once();
        engine.poll_once();
        let sent = submissions(&wire(&mut peer));
        assert_eq!(sent, ["10.0", "13.0"]);
        assert_eq!(shared.orders.wire_order_id(0), Some(13));
        let records = shared
            .take_records(shared.next_seq(), crate::bridge::Take::Dispatch { bulletins: false });
        assert!(records.iter().any(|(_, r)| matches!(
            r,
            crate::bridge::Record::OrderBook(crate::bridge::OrderBook::Forgotten(14))
        )));
        let core = crate::client_core::ClientCore::new();
        for (_, record) in records {
            if let crate::bridge::Record::OrderBook(entry) = record {
                core.keep_the_book(&shared, entry);
            }
        }
        assert_eq!(core.wire_order_id(0), Some(13));
        assert!(core.tracked_order(14).is_none());
        assert_eq!(engine.built_order_commands_held(), 0);
    }

    #[test]
    fn a_cancel_behind_preset_loading_withdraws_the_child_before_it_is_sent() {
        let (mut engine, shared, send, mut peer) = engine();
        shared.admit(&send, placement(true, true)).unwrap();
        shared
            .admit(&send, ControlCommand::CancelOrder { order_id: 11, stated: Default::default() })
            .unwrap();
        engine.poll_once();
        answer(&shared, "OPR.3");
        engine.poll_once();
        engine.poll_attached_quotes(Instant::now() + Duration::from_secs(11));
        engine.poll_once();
        let sent = wire(&mut peer);
        assert_eq!(submissions(&sent), ["10.0", "12.0"]);
        assert!(!sent.contains("35=F|"), "an unsent child needs no venue cancel: {sent}");
        assert!(shared.drain_refused().is_empty());
    }

    #[test]
    fn a_refused_cancel_of_a_recovered_order_keeps_its_api_identity() {
        let (mut engine, shared, send, _) = engine();
        shared.orders.note_attached_order_metadata(
            900,
            crate::bridge::AttachedOrderMetadata {
                api_order_id: Some(0),
                api_client_id: Some(0),
                ..Default::default()
            },
        );
        shared.orders.push_order_update(crate::types::OrderUpdate {
            order_id: 900,
            instrument: 0,
            status: crate::types::OrderStatus::Cancelled,
            filled_qty: 0.0,
            remaining_qty: 0.0,
            avg_price: 0,
            perm_id: 0,
            parent_id: 0,
            timestamp_ns: 0,
        });
        shared
            .admit(&send, ControlCommand::CancelOrder { order_id: 0, stated: Default::default() })
            .unwrap();
        engine.poll_once();
        let records = shared
            .take_records(shared.next_seq(), crate::bridge::Take::Dispatch { bulletins: false });
        assert!(records.iter().any(|(_, r)| matches!(
            r,
            crate::bridge::Record::Refused((
                ErrorOrigin::Order { id: 0, op: OrderOp::Cancel },
                161,
                _
            ))
        )));
    }

    #[test]
    fn an_attached_preset_refusal_keeps_its_place_origin() {
        let (mut engine, shared, send, _peer) = engine();
        shared.reference.set_order_presets(Vec::new());
        shared.admit(&send, placement(true, false)).unwrap();
        engine.poll_once();
        let records = shared
            .take_records(shared.next_seq(), crate::bridge::Take::Dispatch { bulletins: false });
        assert!(records.iter().any(|(_, record)| matches!(
            record,
            crate::bridge::Record::Refused((
                ErrorOrigin::Order { id: 10, op: OrderOp::Place },
                10355,
                _
            ))
        )));
        assert_eq!(shared.backlog(), 0);
    }
}
