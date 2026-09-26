//! Quote observers held while attached families await transmission.

use super::{Connection, FarmState, HeartbeatState, InstrumentId, SharedState};
use crate::types::model::Contract;

/// A slot's quotes or mark held for families waiting on it.
pub(super) struct Watch {
    count: usize,
    contract: Contract,
    /// Whether the watch asked for what it holds, rather than riding on a
    /// caller's subscription.
    pub(super) owned: bool,
    /// A caller's withdrawal of the slot, deferred until the watch ends.
    pub(super) withdrawal: Option<(i64, u64, Vec<u32>, u64)>,
}

impl Watch {
    fn new(contract: &Contract, owned: bool) -> Self {
        Self { count: 1, contract: contract.clone(), owned, withdrawal: None }
    }
}

/// Count another family onto a slot already watched, and say whether it was.
fn watched_again(
    watches: &mut std::collections::HashMap<InstrumentId, Watch>,
    instrument: InstrumentId,
) -> bool {
    watches.get_mut(&instrument).map(|watch| watch.count += 1).is_some()
}

/// Count a family off a slot, and hand back the watch once none is left.
fn unwatched(
    watches: &mut std::collections::HashMap<InstrumentId, Watch>,
    instrument: InstrumentId,
) -> Option<Watch> {
    let watch = watches.get_mut(&instrument)?;
    watch.count -= 1;
    if watch.count != 0 {
        return None;
    }
    watches.remove(&instrument)
}

#[derive(Clone)]
pub(super) struct ComboQuote {
    con_id: i64,
    last_con_id: Option<i32>,
    smart: bool,
    legs: Vec<crate::types::model::ComboLeg>,
    frame: crate::types::AttachedComboFrame,
}

/// Send `tags`, with a combination's terms where the slot carries one.
pub(super) fn send_decorated(
    conn: &mut Connection,
    tags: &[(u32, &str)],
    combo: Option<&ComboQuote>,
) {
    let Some(combo) = combo else {
        let _ = conn.send_fixcomp(tags);
        return;
    };
    let mut owned: Vec<_> = tags.iter().map(|(tag, value)| (*tag, (*value).to_string())).collect();
    combo.decorate(&mut owned);
    let fields: Vec<_> = owned.iter().map(|(tag, value)| (*tag, value.as_str())).collect();
    let _ = conn.send_fixcomp(&fields);
}

impl ComboQuote {
    fn new(shared: &SharedState, contract: &Contract) -> Option<Self> {
        if !matches!(contract.sec_type.as_str(), "BAG" | "COMB") {
            return None;
        }
        let key = crate::client_core::attached_combos::combo_key(contract);
        let frame = shared.reference.attached_combo(&key).and_then(|combo| combo.frame)?;
        let definition =
            crate::client_core::attached_orders::contract_definition(shared, contract)?;
        let last_con_id = (!frame.market_data_generic
            && frame.delta_neutral_contract.is_none()
            && (frame.price_mode != 6 || shared.reference.enables("ICSLAST")))
        .then(|| shared.reference.smart_combo_conid(&contract.currency))
        .flatten();
        Some(Self {
            con_id: i64::from(definition.con_id),
            last_con_id,
            smart: contract.exchange == "SMART",
            legs: shared
                .reference
                .attached_combo(&key)
                .and_then(|combo| combo.legs)
                .map(|(legs, _)| legs)?,
            frame,
        })
    }

    pub(super) fn decorate(&self, tags: &mut Vec<(u32, String)>) {
        let mut out = Vec::with_capacity(tags.len() + self.legs.len() * 8);
        let mut at = 0;
        while at < tags.len() {
            if tags[at].0 != 262 {
                out.push(tags[at].clone());
                at += 1;
                continue;
            }
            let end = tags[at + 1..]
                .iter()
                .position(|(tag, _)| *tag == 262)
                .map_or(tags.len(), |next| at + 1 + next);
            let redirected = self
                .last_con_id
                .filter(|_| tags[at..end].iter().any(|(tag, value)| *tag == 264 && value == "443"));
            let generic_con_id = redirected.filter(|con_id| *con_id > 0);
            for (tag, value) in &tags[at..end] {
                out.push((
                    *tag,
                    match tag {
                        6008 => generic_con_id.map_or(self.con_id, i64::from).to_string(),
                        207 if redirected.is_some() => "BEST".into(),
                        _ => value.clone(),
                    },
                ));
            }
            let multiplier = self.frame.multiplier.or_else(|| {
                (generic_con_id.is_some()
                    && !matches!(self.frame.combo_type, 2 | 3 | 4 | 5 | 7 | 8 | 10))
                .then_some(1.0)
            });
            if let Some(multiplier) = multiplier {
                out.push((231, multiplier.to_string()));
            }
            out.push((6079, self.legs.len().to_string()));
            for leg in &self.legs {
                out.extend([
                    (6080, leg.con_id.to_string()),
                    (
                        6081,
                        if self.frame.price_mode == 5 { "1".into() } else { leg.ratio.to_string() },
                    ),
                    (6082, if leg.action == "BUY" { "1" } else { "0" }.into()),
                ]);
                if generic_con_id.is_some() || self.smart && self.frame.include_leg_exchanges {
                    out.push((
                        616,
                        if matches!(leg.exchange.as_str(), "SMART" | "ZERO") {
                            String::new()
                        } else {
                            crate::control::contracts::exchange_to_fix(&leg.exchange).into()
                        },
                    ));
                }
            }
            if self.frame.separate_delta_neutral {
                out.push((6147, "1".into()));
                if let Some(neutral) = &self.frame.delta_neutral_contract {
                    out.extend([
                        (6148, neutral.delta.to_string()),
                        (6149, neutral.price.to_string()),
                        (6150, neutral.con_id.to_string()),
                    ]);
                }
            }
            at = end;
        }
        *tags = out;
    }
}

impl FarmState {
    pub(crate) fn acquire_attached_mark(
        &mut self,
        instrument: InstrumentId,
        contract: &Contract,
        connection: &mut Option<Connection>,
        heartbeat: &mut HeartbeatState,
    ) {
        if watched_again(&mut self.attached_mark_watches, instrument) {
            return;
        }
        let owned =
            !self.asked_generic_ticks.get(&instrument).is_some_and(|ticks| ticks.contains(&232));
        self.attached_mark_watches.insert(instrument, Watch::new(contract, owned));
        if !owned {
            return;
        }
        self.send_attached_mark(instrument, contract, connection, heartbeat);
    }

    fn send_attached_mark(
        &mut self,
        instrument: InstrumentId,
        contract: &Contract,
        connection: &mut Option<Connection>,
        heartbeat: &mut HeartbeatState,
    ) {
        let ticks = self.asked_generic_ticks.entry(instrument).or_default();
        if !ticks.contains(&232) {
            ticks.push(232);
        }

        let req_id = self.next_md_req_id;
        self.next_md_req_id += 1;
        self.md_req_to_instrument.push((req_id, instrument));
        self.generic_tick_reqs.push((req_id, 232));
        let (venue, sec_type) =
            super::stated_venue_and_type(&contract.sec_type, &contract.exchange);
        let entry = super::MdReqEntry { req_id, request_type: 232, venue: venue.to_string(), precision: "1" };
        if let Some((_, record)) =
            self.instrument_md_reqs.iter_mut().find(|(id, _)| *id == instrument)
        {
            record.entries.push(entry);
        } else {
            self.instrument_md_reqs.push((
                instrument,
                super::MdReqRecord {
                    con_id: contract.con_id,
                    sec_type: sec_type.to_string(),
                    mode_9887: 0,
                    entries: vec![entry],
                },
            ));
        }
        if let Some(connection) = connection.as_mut() {
            let tags = super::build_series_subscribe_tags(
                contract.con_id,
                &contract.exchange,
                &contract.sec_type,
                0,
                &super::chrono_free_timestamp(),
                &[(req_id, 232)],
                None,
            );
            let refs: Vec<_> = tags.iter().map(|(tag, value)| (*tag, value.as_str())).collect();
            let _ = connection.send_fixcomp(&refs);
            heartbeat.last_farm_sent = std::time::Instant::now();
        }
    }

    pub(super) fn replay_attached_marks(
        &mut self,
        connection: &mut Option<Connection>,
        heartbeat: &mut HeartbeatState,
    ) {
        let missing: Vec<_> = self
            .attached_mark_watches
            .iter()
            .filter(|(instrument, _)| {
                !self.generic_tick_reqs.iter().any(|(request, tick)| {
                    *tick == 232 && self.md_req_to_instrument.contains(&(*request, **instrument))
                })
            })
            .map(|(instrument, watch)| (*instrument, watch.contract.clone()))
            .collect();
        for (instrument, contract) in missing {
            self.send_attached_mark(instrument, &contract, connection, heartbeat);
        }
    }

    pub(crate) fn release_attached_mark(
        &mut self,
        instrument: InstrumentId,
        _shared: &SharedState,
        connection: &mut Option<Connection>,
        heartbeat: &mut HeartbeatState,
    ) {
        let Some(watch) = unwatched(&mut self.attached_mark_watches, instrument) else { return };
        if let Some((con_id, took_it, series, issued)) = watch.withdrawal {
            self.send_mktdata_unsubscribe(
                instrument, con_id, took_it, &series, issued, false, connection, heartbeat,
            );
        } else if watch.owned {
            self.stop_asking_for_series(
                instrument,
                watch.contract.con_id,
                0,
                &[232],
                u64::MAX,
                connection,
                heartbeat,
            );
            self.instrument_md_reqs.retain(|(_, record)| !record.entries.is_empty());
        }
    }

    pub(crate) fn acquire_attached_quote(
        &mut self,
        instrument: InstrumentId,
        contract: &Contract,
        shared: &SharedState,
        connection: &mut Option<Connection>,
        heartbeat: &mut HeartbeatState,
    ) {
        if watched_again(&mut self.attached_quote_watches, instrument) {
            return;
        }
        let owned = !self.holds_a_stream(instrument);
        self.attached_quote_watches.insert(instrument, Watch::new(contract, owned));
        if owned {
            if let Some(combo) = ComboQuote::new(shared, contract) {
                self.attached_combo_quotes.insert(instrument, combo);
            }
            shared.market.note_pricing_subscription(instrument, 0, false);
            self.send_mktdata_subscribe(
                contract.con_id,
                &contract.symbol,
                &contract.exchange,
                &contract.sec_type,
                &contract.last_trade_date_or_contract_month,
                contract.strike,
                &contract.right,
                &contract.multiplier,
                instrument,
                0,
                false,
                shared,
                connection,
                heartbeat,
            );
        }
    }

    pub(crate) fn release_attached_quote(
        &mut self,
        instrument: InstrumentId,
        _shared: &SharedState,
        connection: &mut Option<Connection>,
        heartbeat: &mut HeartbeatState,
    ) {
        let Some(watch) = unwatched(&mut self.attached_quote_watches, instrument) else { return };
        let withdrawal = watch
            .withdrawal
            .or_else(|| watch.owned.then_some((watch.contract.con_id, 0, Vec::new(), u64::MAX)));
        if let Some((con_id, took_it, series, issued)) = withdrawal {
            self.send_mktdata_unsubscribe(
                instrument, con_id, took_it, &series, issued, false, connection, heartbeat,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> Contract {
        Contract {
            con_id: 218,
            symbol: "ABC".into(),
            sec_type: "STK".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            ..Contract::default()
        }
    }

    fn combo_contract(shared: &SharedState, smart: bool) -> Contract {
        let contract = Contract {
            symbol: "ABC".into(),
            sec_type: "BAG".into(),
            currency: "USD".into(),
            exchange: if smart { "SMART" } else { "CME" }.into(),
            combo_legs: vec![
                crate::types::model::ComboLeg {
                    con_id: 17,
                    ratio: 2,
                    action: "BUY".into(),
                    exchange: "SMART".into(),
                    ..Default::default()
                },
                crate::types::model::ComboLeg {
                    con_id: 18,
                    ratio: 1,
                    action: "SELL".into(),
                    exchange: "CME".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let key = format!("{contract:?}");
        shared.reference.update_attached_combo(&key, |combo| {
            combo.definition = Some(crate::control::contracts::ContractDefinition {
                con_id: 700,
                exchange: contract.exchange.clone(),
                ..Default::default()
            })
        });
        shared.reference.update_attached_combo(&key, |combo| {
            combo.legs = Some((contract.combo_legs.clone(), 1.0))
        });
        shared.reference.update_attached_combo(&key, |combo| {
            combo.frame = Some(crate::types::AttachedComboFrame {
                market_data_generic: smart,
                multiplier: smart.then_some(50.0),
                price_mode: 2,
                combo_type: 9,
                include_leg_exchanges: true,
                separate_delta_neutral: smart,
                delta_neutral_contract: smart.then_some(crate::types::DeltaNeutralContractSpec {
                    con_id: 19,
                    delta: 0.5,
                    price: 33.25,
                }),
            })
        });
        shared.reference.set_smart_combo_contracts(900, None);
        contract
    }

    fn entries(message: &[u8]) -> Vec<Vec<(u32, String)>> {
        let mut entries = Vec::<Vec<_>>::new();
        for field in crate::control::contracts::tag_sequence(message) {
            if field.0 == 10 {
                continue;
            }
            if field.0 == 262 {
                entries.push(Vec::new());
            }
            if let Some(entry) = entries.last_mut() {
                entry.push(field);
            }
        }
        entries
    }

    #[test]
    fn a_smart_combination_quote_carries_its_resolved_legs_and_withdraws_them() {
        let shared = SharedState::new();
        let contract = combo_contract(&shared, true);
        let mut farm = FarmState::new();
        let mut heartbeat = HeartbeatState::new();
        let (connection, peer) = Connection::for_test();
        let mut connection = Some(connection);
        let mut peer = Connection::new_raw(peer).unwrap();
        farm.acquire_attached_quote(0, &contract, &shared, &mut connection, &mut heartbeat);
        let sent = super::super::tests::drain_inner(&mut peer);
        let asked: Vec<_> = sent.iter().flat_map(|message| entries(message)).collect();
        assert_eq!(asked.len(), 4);
        for entry in &asked {
            assert!(entry.contains(&(6008, "700".into())));
            assert!(entry.contains(&(231, "50".into())));
            assert!(entry.contains(&(6079, "2".into())));
            assert_eq!(
                entry
                    .iter()
                    .filter(|(tag, _)| *tag == 6080)
                    .map(|(_, value)| value.as_str())
                    .collect::<Vec<_>>(),
                ["17", "18"]
            );
            assert_eq!(
                entry
                    .iter()
                    .filter(|(tag, _)| *tag == 616)
                    .map(|(_, value)| value.as_str())
                    .collect::<Vec<_>>(),
                ["", "CME"]
            );
            for field in [(6147, "1"), (6148, "0.5"), (6149, "33.25"), (6150, "19")] {
                assert!(entry.contains(&(field.0, field.1.into())));
            }
            assert!(!entry.iter().any(|(tag, _)| matches!(tag, 55 | 6175 | 6134)));
        }
        farm.release_attached_quote(0, &shared, &mut connection, &mut heartbeat);
        let sent = super::super::tests::drain_inner(&mut peer);
        let withdrawn: Vec<_> = sent.iter().flat_map(|message| entries(message)).collect();
        assert_eq!(withdrawn, asked);
        assert!(farm.attached_combo_quotes.is_empty());
    }

    #[test]
    fn a_direct_combination_requests_its_last_on_the_stated_generic_contract() {
        let shared = SharedState::new();
        let contract = combo_contract(&shared, false);
        let quote = ComboQuote::new(&shared, &contract).unwrap();
        let mut tags = super::super::build_conid_subscribe_tags(
            true,
            false,
             "1",
            1,
            2,
            0,
            "CME",
            "BAG",
            0,
            "now",
            &[],
        );
        quote.decorate(&mut tags);
        let split = tags.iter().position(|(tag, value)| *tag == 262 && value == "2").unwrap();
        let (bid_ask, last) = tags.split_at(split);
        assert!(bid_ask.contains(&(6008, "700".into())));
        assert!(bid_ask.contains(&(207, "CME".into())));
        assert!(!bid_ask.iter().any(|(tag, _)| matches!(tag, 231 | 616)));
        assert!(last.contains(&(6008, "900".into())));
        assert!(last.contains(&(207, "BEST".into())));
        assert!(last.contains(&(231, "1".into())));
        assert_eq!(last.iter().filter(|(tag, _)| *tag == 616).count(), 2);
        shared.reference.set_smart_combo_contracts(0, Some("EUR:901"));
        let mut absent = contract.clone();
        absent.currency = "GBP".into();
        let key = format!("{absent:?}");
        shared.reference.update_attached_combo(&key, |combo| {
            combo.definition = Some(crate::control::contracts::ContractDefinition {
                con_id: 700,
                ..Default::default()
            })
        });
        shared
            .reference
            .update_attached_combo(&key, |combo| combo.frame = Some(quote.frame.clone()));
        shared.reference.update_attached_combo(&key, |combo| {
            combo.legs = Some((absent.combo_legs.clone(), 1.0))
        });
        assert!(ComboQuote::new(&shared, &absent).unwrap().last_con_id.is_none());
    }

    #[test]
    fn a_combination_uses_only_a_positive_generic_id_and_the_stated_last_feature() {
        let shared = SharedState::new();
        let contract = combo_contract(&shared, false);
        let key = format!("{contract:?}");
        let mut frame =
            shared.reference.attached_combo(&key).and_then(|combo| combo.frame).unwrap();
        frame.price_mode = 6;
        shared.reference.update_attached_combo(&key, |combo| combo.frame = Some(frame));
        assert!(ComboQuote::new(&shared, &contract).unwrap().last_con_id.is_none());
        shared.reference.add_enabled_features(vec!["ICSLAST".into()]);
        assert_eq!(ComboQuote::new(&shared, &contract).unwrap().last_con_id, Some(900));
        shared.reference.set_smart_combo_contracts(0, Some(""));
        let mut tags =
            vec![(262, "1".into()), (6008, "0".into()), (207, "CME".into()), (264, "443".into())];
        ComboQuote::new(&shared, &contract).unwrap().decorate(&mut tags);
        assert!(tags.contains(&(6008, "700".into())));
        assert!(tags.contains(&(207, "BEST".into())));
        assert!(!tags.iter().any(|(tag, _)| matches!(tag, 231 | 616)));
    }

    #[test]
    fn a_combination_with_a_later_price_mode_keeps_the_initial_quote_ratios() {
        let shared = SharedState::new();
        let contract = combo_contract(&shared, true);
        let key = format!("{contract:?}");
        let mut frame =
            shared.reference.attached_combo(&key).and_then(|combo| combo.frame).unwrap();
        frame.price_mode = 5;
        shared.reference.update_attached_combo(&key, |combo| combo.frame = Some(frame));
        let mut tags = vec![(262, "1".into()), (6008, "0".into()), (264, "442".into())];
        ComboQuote::new(&shared, &contract).unwrap().decorate(&mut tags);
        assert_eq!(
            tags.iter()
                .filter(|(tag, _)| *tag == 6081)
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>(),
            ["1", "1"]
        );
    }

    #[test]
    fn a_combination_quote_keeps_its_terms_when_replayed() {
        let shared = SharedState::new();
        let contract = combo_contract(&shared, true);
        let mut farm = FarmState::new();
        let mut context = crate::engine::context::Context::new();
        context.market.register(0);
        let mut heartbeat = HeartbeatState::new();
        farm.acquire_attached_quote(0, &contract, &shared, &mut None, &mut heartbeat);
        farm.handle_disconnect(&mut None, &mut context, &None, &shared);
        let (connection, peer) = Connection::for_test();
        let mut connection_holder = None;
        let mut peer = Connection::new_raw(peer).unwrap();
        farm.reconnect(
            connection,
            &mut connection_holder,
            &mut context,
            &mut heartbeat,
            crate::engine::hot_loop::ReplayPacing::default(),
            &shared,
        );
        let sent = super::super::tests::drain_inner(&mut peer);
        assert!(!sent.is_empty());
        for entry in sent.iter().flat_map(|message| entries(message)) {
            assert!(entry.contains(&(6008, "700".into())));
            assert!(entry.contains(&(6079, "2".into())));
        }
    }

    #[test]
    fn families_share_one_stream_and_the_last_observer_releases_it() {
        let mut farm = FarmState::new();
        let shared = SharedState::new();
        let mut heartbeat = HeartbeatState::new();
        farm.acquire_attached_quote(0, &contract(), &shared, &mut None, &mut heartbeat);
        let requests = farm.next_md_req_id;
        farm.acquire_attached_quote(0, &contract(), &shared, &mut None, &mut heartbeat);
        assert_eq!(farm.next_md_req_id, requests);
        farm.release_attached_quote(0, &shared, &mut None, &mut heartbeat);
        assert!(farm.holds_a_stream(0));
        farm.release_attached_quote(0, &shared, &mut None, &mut heartbeat);
        assert!(!farm.holds_market_data(0));
    }

    #[test]
    fn a_caller_joining_the_quote_keeps_its_stream() {
        let mut farm = FarmState::new();
        let shared = SharedState::new();
        let mut heartbeat = HeartbeatState::new();
        farm.acquire_attached_quote(0, &contract(), &shared, &mut None, &mut heartbeat);
        farm.note_subscription_asked_on(0, 1);
        farm.release_attached_quote(0, &shared, &mut None, &mut heartbeat);
        assert!(farm.holds_a_stream(0));
    }

    #[test]
    fn a_callers_withdrawal_waits_for_the_order_observer() {
        let mut farm = FarmState::new();
        let shared = SharedState::new();
        let mut heartbeat = HeartbeatState::new();
        farm.acquire_attached_quote(0, &contract(), &shared, &mut None, &mut heartbeat);
        farm.note_subscription_asked_on(0, 1);
        farm.send_mktdata_unsubscribe(0, 218, 0, &[], 2, false, &mut None, &mut heartbeat);
        assert!(farm.holds_a_stream(0));
        farm.release_attached_quote(0, &shared, &mut None, &mut heartbeat);
        assert!(!farm.holds_market_data(0));
    }

    #[test]
    fn a_mark_observer_sends_and_withdraws_series_232() {
        let mut farm = FarmState::new();
        let shared = SharedState::new();
        let mut heartbeat = HeartbeatState::new();
        let (connection, peer) = Connection::for_test();
        let mut connection = Some(connection);
        let mut peer = Connection::new_raw(peer).unwrap();
        farm.acquire_attached_mark(0, &contract(), &mut connection, &mut heartbeat);
        let sent = super::super::tests::drain_inner(&mut peer);
        assert_eq!(sent.len(), 1);
        let tags = crate::protocol::fix::fix_parse(&sent[0]);
        for (tag, value) in
            [(263, "1"), (146, "1"), (6008, "218"), (264, "232"), (207, "BEST"), (167, "CS")]
        {
            assert_eq!(tags.get(&tag).map(String::as_str), Some(value));
        }
        let request = tags[&262].clone();
        farm.release_attached_mark(0, &shared, &mut connection, &mut heartbeat);
        let sent = super::super::tests::drain_inner(&mut peer);
        assert_eq!(sent.len(), 1);
        let tags = crate::protocol::fix::fix_parse(&sent[0]);
        assert_eq!(tags[&263], "2");
        assert_eq!(tags[&264], "232");
        assert_eq!(tags[&262], request);
    }

    #[test]
    fn the_mark_cache_follows_the_series_flags() {
        let shared = SharedState::new();
        for (price, flags, expected) in [
            (100.5_f64, 1_i32, Some(100.5)),
            (102.0, 0, None),
            (103.0, 0x0800_0001, None),
            (-1.0, 1, None),
            (101.0, 1, Some(101.0)),
        ] {
            let mut bytes = price.to_be_bytes().to_vec();
            bytes.extend(flags.to_be_bytes());
            assert!(super::super::deliver_series(232, &bytes, 0, &shared));
            assert_eq!(shared.market.pricing_quote_views(0).mark_price, expected);
        }
    }

    #[test]
    fn a_standalone_mark_is_requested_again_after_the_connection_changes() {
        let mut farm = FarmState::new();
        let mut heartbeat = HeartbeatState::new();
        farm.acquire_attached_mark(0, &contract(), &mut None, &mut heartbeat);
        let previous = farm.next_md_req_id;
        farm.instrument_md_reqs.clear();
        farm.md_req_to_instrument.clear();
        farm.generic_tick_reqs.clear();
        farm.replay_attached_marks(&mut None, &mut heartbeat);
        assert_eq!(farm.next_md_req_id, previous + 1);
        assert_eq!(farm.generic_tick_reqs, [(previous, 232)]);
        farm.replay_attached_marks(&mut None, &mut heartbeat);
        assert_eq!(farm.next_md_req_id, previous + 1);
    }

    #[test]
    fn a_mark_observer_requests_only_its_series_and_shares_it() {
        let mut farm = FarmState::new();
        let shared = SharedState::new();
        let mut heartbeat = HeartbeatState::new();
        farm.acquire_attached_mark(0, &contract(), &mut None, &mut heartbeat);
        farm.acquire_attached_mark(0, &contract(), &mut None, &mut heartbeat);
        assert!(!farm.holds_a_stream(0));
        let entries = &farm.instrument_md_reqs[0].1.entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].request_type, 232);
        farm.release_attached_mark(0, &shared, &mut None, &mut heartbeat);
        assert!(farm.holds_market_data(0));
        farm.release_attached_mark(0, &shared, &mut None, &mut heartbeat);
        assert!(!farm.holds_market_data(0));
        assert!(farm.generic_tick_reqs.is_empty());
        assert!(farm.asked_generic_ticks.is_empty());
    }

    #[test]
    fn a_callers_mark_withdrawal_waits_for_the_order_observer() {
        let mut farm = FarmState::new();
        let shared = SharedState::new();
        let mut heartbeat = HeartbeatState::new();
        farm.asked_generic_ticks.insert(0, vec![232]);
        farm.send_mktdata_subscribe(
            218,
            "ABC",
            "SMART",
            "STK",
            "",
            0.0,
            "",
            "",
            0,
            0,
            false,
             &crate::bridge::SharedState::new(),
            &mut None,
            &mut heartbeat,
        );
        farm.acquire_attached_mark(0, &contract(), &mut None, &mut heartbeat);
        farm.stop_asking_for_series(0, 218, 0, &[232], 2, &mut None, &mut heartbeat);
        assert!(farm.generic_tick_reqs.iter().any(|(_, tick)| *tick == 232));
        farm.release_attached_mark(0, &shared, &mut None, &mut heartbeat);
        assert!(!farm.generic_tick_reqs.iter().any(|(_, tick)| *tick == 232));
        assert!(farm.holds_a_stream(0));
    }

    #[test]
    fn a_caller_joining_a_mark_preserves_one_request() {
        let mut farm = FarmState::new();
        let shared = SharedState::new();
        let mut heartbeat = HeartbeatState::new();
        farm.acquire_attached_mark(0, &contract(), &mut None, &mut heartbeat);
        farm.note_series_asked_on(0, &[232], 1);
        farm.send_mktdata_subscribe(
            218,
            "ABC",
            "SMART",
            "STK",
            "",
            0.0,
            "",
            "",
            0,
            0,
            false,
             &crate::bridge::SharedState::new(),
            &mut None,
            &mut heartbeat,
        );
        assert_eq!(farm.generic_tick_reqs.iter().filter(|(_, tick)| *tick == 232).count(), 1);
        farm.release_attached_mark(0, &shared, &mut None, &mut heartbeat);
        assert!(farm.generic_tick_reqs.iter().any(|(_, tick)| *tick == 232));
    }

    #[test]
    fn a_later_join_survives_a_deferred_withdrawal() {
        let mut farm = FarmState::new();
        let shared = SharedState::new();
        let mut heartbeat = HeartbeatState::new();
        farm.acquire_attached_quote(0, &contract(), &shared, &mut None, &mut heartbeat);
        farm.note_subscription_asked_on(0, 1);
        farm.send_mktdata_unsubscribe(0, 218, 0, &[], 2, false, &mut None, &mut heartbeat);
        farm.note_subscription_asked_on(0, 3);
        farm.release_attached_quote(0, &shared, &mut None, &mut heartbeat);
        assert!(farm.holds_a_stream(0));
    }
}
