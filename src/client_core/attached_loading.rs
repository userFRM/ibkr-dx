//! Loading the selected account preset before constructing attached orders.

use super::{ApiContract, Refusal, SharedState};
use crate::control::attached_presets::{
    AttachedPreset, PresetInstrument, is_unreadable, request_selector, select_preset_key,
};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::task::Poll;
use std::time::Instant;

/// Work requested by a family's loading continuation, carried out by the engine.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Request {
    /// Confirm the venue representation of combination legs for attached orders.
    ConfirmAttachedCombo {
        /// The key echoed by the answer.
        request_key: String,
        /// The confirmation fields derived from the combination and its definitions.
        fields: Vec<(u32, String)>,
    },
    /// Read whether an exchange accepts a combination without delta neutrality.
    FetchAttachedComboRules {
        /// The exchange whose rules are needed.
        exchange: String,
    },
    /// Request the values in one account order preset.
    FetchOrderPresetValues {
        /// The unique request key, in the `OPR.` namespace.
        request_key: String,
        /// The preset's key, rebuilt as a request names it.
        key: String,
    },
    Name(ApiContract, Sender<Result<(), Refusal>>),
}

/// Poll an answer once per engine lap, under the placement's deadline.
pub(crate) async fn answer<T>(
    receive: Receiver<T>,
    deadline: Instant,
    what: &str,
) -> Result<T, Refusal> {
    std::future::poll_fn(move |_| match receive.try_recv() {
        Ok(answer) => Poll::Ready(Ok(answer)),
        Err(TryRecvError::Disconnected) => {
            Poll::Ready(Err(Refusal::not_connected("Engine stopped before it answered")))
        }
        Err(TryRecvError::Empty) if Instant::now() >= deadline => {
            Poll::Ready(Err(Refusal::no_answer(what)))
        }
        Err(TryRecvError::Empty) => Poll::Pending,
    })
    .await
}

pub(crate) async fn name(
    control: &Sender<Request>,
    contract: &ApiContract,
    deadline: Instant,
) -> Result<(), Refusal> {
    let (send, receive) = std::sync::mpsc::channel();
    control
        .send(Request::Name(contract.clone(), send))
        .map_err(|error| Refusal::not_connected(format!("Engine stopped: {error}")))?;
    answer(receive, deadline, "Contract definition timed out").await?
}

pub(crate) async fn load_attached_preset(
    shared: &SharedState,
    control: &Sender<Request>,
    contract: &ApiContract,
    underlying_security_type: &str,
    deadline: Instant,
) -> Result<AttachedPreset, Refusal> {
    let instrument = PresetInstrument {
        security_type: &contract.sec_type,
        underlying_security_type,
        symbol: &contract.symbol,
        currency: &contract.currency,
    };
    loop {
        let mut entries = std::future::poll_fn(|_| match shared.reference.order_preset_list() {
            Some(list) => Poll::Ready(Ok(list)),
            None if Instant::now() >= deadline => {
                Poll::Ready(Err(Refusal::no_answer("Order preset list timed out")))
            }
            None => Poll::Pending,
        })
        .await?;
        for (key, attributes, _) in &mut entries {
            if let Some(values) =
                shared.reference.current_order_preset_values(&request_selector(key))
            {
                *attributes =
                    if is_unreadable(&values.fields) { String::new() } else { values.attributes };
            }
        }
        let enabled = shared.reference.enables("PRESETS");
        let Some(key) = select_preset_key(&entries, instrument, enabled) else {
            return Ok(AttachedPreset::default());
        };
        let key = request_selector(key);
        if let Some(values) = shared.reference.current_order_preset_values(&key) {
            return AttachedPreset::read(&values)
                .ok_or_else(|| Refusal::no_answer("Order preset answer could not be read"));
        }
        if Instant::now() >= deadline {
            return Err(Refusal::no_answer("Order preset values timed out"));
        }
        let (request_key, receive) = shared.reference.expect_order_preset_values(&key);
        // Removing the waiter also covers cancellation while the future is held.
        struct Waiting<'a>(&'a SharedState, String);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.reference.stop_waiting_for_preset_values(&self.1);
            }
        }
        let waiting = Waiting(shared, request_key.clone());
        control
            .send(Request::FetchOrderPresetValues { request_key, key })
            .map_err(|error| Refusal::not_connected(format!("Engine stopped: {error}")))?;
        answer(receive, deadline, "Order preset values timed out").await?;
        drop(waiting);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::control::order_presets::PresetValues;
    use std::sync::Mutex;
    use std::sync::mpsc::{RecvTimeoutError, channel};
    use std::time::Duration;

    pub(crate) fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let Poll::Ready(answer) = future.as_mut().poll(&mut context) {
                return answer;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn contract() -> ApiContract {
        ApiContract {
            sec_type: "STK".into(),
            symbol: "ABC".into(),
            currency: "USD".into(),
            ..ApiContract::default()
        }
    }

    fn entry(key: &str, attributes: &str, changed: &str) -> (String, String, String) {
        (key.into(), attributes.into(), changed.into())
    }

    fn request(receive: &Mutex<Receiver<Request>>) -> (String, String) {
        match receive.lock().unwrap().recv_timeout(Duration::from_secs(2)).unwrap() {
            Request::FetchOrderPresetValues { request_key, key } => (request_key, key),
            _ => panic!("Expected a preset values request"),
        }
    }

    fn answer(request: (String, String), attributes: &str) -> PresetValues {
        PresetValues {
            request_key: request.0,
            key: request.1,
            attributes: attributes.into(),

            error: None,
            fields: vec![(4075, "1".into()), (4083, "2".into())],
        }
    }

    #[test]
    fn the_list_arrives_before_the_selected_get_is_sent() {
        let shared = SharedState::new();
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        std::thread::scope(|scope| {
            let loader = scope.spawn(|| {
                block_on(load_attached_preset(
                    &shared,
                    &send,
                    &contract(),
                    "",
                    Instant::now() + Duration::from_secs(2),
                ))
            });
            assert!(matches!(
                receive.lock().unwrap().recv_timeout(Duration::from_millis(20)),
                Err(RecvTimeoutError::Timeout)
            ));
            shared.reference.set_order_presets(vec![entry("s=STK", "a=1", "1")]);
            let get = request(&receive);
            assert_eq!(get.1, "s=STK");
            shared.reference.set_order_preset_values(answer(get, "a=1"));
            assert!(loader.join().unwrap().unwrap().auto_attach_profit_taker);
        });
    }

    #[test]
    fn one_selected_get_is_cached_for_the_current_list() {
        let shared = SharedState::new();
        shared
            .reference
            .set_order_presets(vec![entry("s=STK", "a=1", "1"), entry("s=FUT", "a=1", "1")]);
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        std::thread::scope(|scope| {
            let responder = scope.spawn(|| {
                let get = request(&receive);
                assert_eq!(get.1, "s=STK");
                shared.reference.set_order_preset_values(answer(get, "a=1"));
            });
            let first = block_on(load_attached_preset(
                &shared,
                &send,
                &contract(),
                "",
                Instant::now() + Duration::from_secs(2),
            ))
            .unwrap();
            responder.join().unwrap();
            assert_eq!(
                block_on(load_attached_preset(
                    &shared,
                    &send,
                    &contract(),
                    "",
                    Instant::now() + Duration::from_secs(2)
                ))
                .unwrap(),
                first
            );
            assert!(receive.lock().unwrap().try_recv().is_err());
        });
    }

    #[test]
    fn an_answer_must_match_both_request_and_preset_key() {
        let shared = SharedState::new();
        shared.reference.set_order_presets(vec![entry("s=STK", "a=1", "1")]);
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        std::thread::scope(|scope| {
            let responder = scope.spawn(|| {
                let get = request(&receive);
                let mut unrelated = answer(get.clone(), "a=1");
                unrelated.request_key = "OPR.unrelated".into();
                unrelated.fields = vec![(4083, "bad".into())];
                shared.reference.set_order_preset_values(unrelated);
                let mut wrong_key = answer(get.clone(), "a=1");
                wrong_key.key = "s=FUT".into();
                shared.reference.set_order_preset_values(wrong_key);
                shared.reference.set_order_preset_values(answer(get, "a=1"));
            });
            assert_eq!(
                block_on(load_attached_preset(
                    &shared,
                    &send,
                    &contract(),
                    "",
                    Instant::now() + Duration::from_secs(2)
                ))
                .unwrap()
                .profit_order_type,
                "2"
            );
            responder.join().unwrap();
        });
    }

    #[test]
    fn a_list_change_during_get_requires_a_new_answer() {
        let shared = SharedState::new();
        shared.reference.set_order_presets(vec![entry("s=STK", "a=1", "1")]);
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        std::thread::scope(|scope| {
            let responder = scope.spawn(|| {
                let first = request(&receive);
                shared.reference.set_order_presets(vec![entry("s=STK", "a=1", "2")]);
                let mut stale = answer(first.clone(), "a=1");
                stale.fields = vec![(4083, "stale".into())];
                shared.reference.set_order_preset_values(stale);
                let second = request(&receive);
                assert_ne!(first.0, second.0);
                assert_eq!(first.1, second.1);
                shared.reference.set_order_preset_values(answer(second, "a=1"));
            });
            assert_eq!(
                block_on(load_attached_preset(
                    &shared,
                    &send,
                    &contract(),
                    "",
                    Instant::now() + Duration::from_secs(2)
                ))
                .unwrap()
                .profit_order_type,
                "2"
            );
            responder.join().unwrap();
        });
    }

    #[test]
    fn changed_get_attributes_cause_selection_to_run_again() {
        let shared = SharedState::new();
        shared.reference.set_enabled_features(vec!["PRESETS".into()]);
        let entries = vec![
            entry("s=STK", "", "1"),
            entry("s=STK&tc=Alpha", "st=1&a=1", "1"),
            entry("s=STK&tc=Beta", "st=1&a=1", "1"),
        ];
        shared.reference.set_order_presets(entries.clone());
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        std::thread::scope(|scope| {
            let responder = scope.spawn(|| {
                let first = request(&receive);
                assert_eq!(first.1, "s=STK&tc=Alpha");
                shared.reference.set_order_preset_values(answer(first, "st=1&a=0"));
                let second = request(&receive);
                assert_eq!(second.1, "s=STK&tc=Beta");
                shared.reference.set_order_preset_values(answer(second, "st=1&a=1"));
            });
            assert!(
                block_on(load_attached_preset(
                    &shared,
                    &send,
                    &contract(),
                    "",
                    Instant::now() + Duration::from_secs(2)
                ))
                .unwrap()
                .auto_attach_profit_taker
            );
            responder.join().unwrap();
        });
        assert_eq!(shared.reference.order_presets(), entries);
    }

    #[test]
    fn empty_lists_use_defaults_without_a_get() {
        let shared = SharedState::new();
        shared.reference.set_order_presets(Vec::new());
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        assert_eq!(
            block_on(load_attached_preset(
                &shared,
                &send,
                &contract(),
                "",
                Instant::now() + Duration::from_secs(2)
            ))
            .unwrap(),
            AttachedPreset::default()
        );
        assert!(receive.lock().unwrap().try_recv().is_err());
    }

    #[test]
    fn timeouts_remove_waiters_and_late_answers_are_not_current() {
        let shared = SharedState::new();
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        let missing_list =
            block_on(load_attached_preset(&shared, &send, &contract(), "", Instant::now()))
                .unwrap_err();
        assert_eq!(missing_list.code, Refusal::no_answer("").code);
        shared.reference.set_order_presets(vec![entry("s=STK", "a=1", "1")]);
        let missing_values = block_on(load_attached_preset(
            &shared,
            &send,
            &contract(),
            "",
            Instant::now() + Duration::from_millis(100),
        ))
        .unwrap_err();
        assert_eq!(missing_values.code, Refusal::no_answer("").code);
        shared.reference.set_order_preset_values(answer(request(&receive), "a=1"));
        assert!(shared.reference.current_order_preset_values("s=STK").is_none());
    }

    #[test]
    fn stopped_engine_is_distinct_from_an_unanswered_request() {
        let shared = SharedState::new();
        shared.reference.set_order_presets(vec![entry("s=STK", "a=1", "1")]);
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        drop(receive);
        let error = block_on(load_attached_preset(
            &shared,
            &send,
            &contract(),
            "",
            Instant::now() + Duration::from_secs(2),
        ))
        .unwrap_err();
        assert_eq!(error.code, Refusal::not_connected("").code);
    }

    #[test]
    fn completed_error_answers_keep_their_values() {
        let shared = SharedState::new();
        shared.reference.set_order_presets(vec![entry("s=STK", "a=1", "1")]);
        let (send, receive) = channel();
        let receive = Mutex::new(receive);
        std::thread::scope(|scope| {
            let responder = scope.spawn(|| {
                let mut values = answer(request(&receive), "a=1");
                values.error = Some("Not found".into());
                shared.reference.set_order_preset_values(values);
            });
            assert!(
                block_on(load_attached_preset(
                    &shared,
                    &send,
                    &contract(),
                    "",
                    Instant::now() + Duration::from_secs(2)
                ))
                .unwrap()
                .auto_attach_profit_taker
            );
            responder.join().unwrap();
        });
    }

    /// The time a placement has already spent waiting counts: a deadline
    /// already past refuses without asking for values it has no time to hear.
    #[test]
    fn a_spent_deadline_refuses_without_asking() {
        let shared = SharedState::new();
        shared.reference.set_order_presets(vec![entry("s=STK", "a=1", "1")]);
        let (send, receive) = channel();
        let started = Instant::now();
        let error =
            block_on(load_attached_preset(&shared, &send, &contract(), "", started)).unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (Refusal::no_answer("").code, "Order preset values timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(receive.try_recv().is_err(), "nothing was asked");
    }
}
