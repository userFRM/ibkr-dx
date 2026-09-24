//! Transmission waits for the market data used by an attached parent.

use super::HotLoop;
use crate::client_core::attached_prices::cached_quote_views;
use crate::types::model::{Contract, Order};
use crate::types::{InstrumentId, OrderRequest};
use std::time::{Duration, Instant};

pub(super) struct WaitingFamily {
    pub(super) orders: Vec<OrderRequest>,
    contract: Contract,
    pub(super) instrument: InstrumentId,
    started: Instant,
    next_poll: Instant,
    mark: bool,
}

pub(super) struct QuoteLookup {
    req_id: u32,
    con_id: u32,
    exchange: String,
    parents: Vec<Contract>,
}

impl HotLoop {
    pub(super) fn accept_attached_orders(
        &mut self,
        orders: Vec<OrderRequest>,
        contract: Contract,
        parent: Order,
        instrument: InstrumentId,
    ) {
        let orders = self.shared.orders.waiting_attached_family(&orders);
        if orders.is_empty() {
            return;
        }
        let waiting = orders.first().and_then(|order| {
            self.attached_quote_orders.iter().position(|family| {
                family.orders.iter().any(|held| held.order_id() == order.order_id())
            })
        });
        if let Some(waiting) = waiting {
            for order in orders {
                if matches!(&order, OrderRequest::Modify { .. }) {
                    self.queue_order_during_quote_wait(order);
                } else {
                    let family = &mut self.attached_quote_orders[waiting].orders;
                    if let Some(held) =
                        family.iter_mut().find(|held| held.order_id() == order.order_id())
                    {
                        *held = order;
                    } else {
                        family.push(order);
                    }
                }
            }
            return;
        }
        if orders.iter().all(|order| matches!(order, OrderRequest::Modify { .. })) {
            for order in orders {
                self.queue_order_during_quote_wait(order);
            }
            return;
        }
        let indicative = if parent.override_percentage_constraints {
            None
        } else {
            match crate::client_core::attached_quote_contract::select(&self.shared, &contract) {
                crate::client_core::attached_quote_contract::Selection::Ready(proxy) => {
                    let proxy_instrument = if proxy.con_id == contract.con_id {
                        instrument
                    } else {
                        self.context.market.instrument_by_con_id(proxy.con_id).unwrap_or_else(
                            || {
                                self.register_contract(
                                    proxy.con_id,
                                    proxy.symbol.clone(),
                                    &proxy.sec_type,
                                    &proxy.exchange,
                                    "",
                                    "",
                                )
                            },
                        )
                    };
                    self.shared.market.note_attached_quote_instrument(
                        proxy.con_id,
                        &proxy.exchange,
                        proxy_instrument,
                    );
                    Some((proxy_instrument, proxy))
                }
                crate::client_core::attached_quote_contract::Selection::Resolve {
                    con_id,
                    exchange,
                } => {
                    self.resolve_attached_quote(&contract, con_id, &exchange);
                    None
                }
                crate::client_core::attached_quote_contract::Selection::Unavailable => None,
            }
        };
        let quote =
            indicative.as_ref().is_some_and(|(id, proxy)| !self.attached_quote_ready(*id, proxy));
        let mark = !quote && self.attached_mark_needed(instrument, &contract, &parent);
        if !quote && !mark {
            for order in self.shared.orders.take_waiting_attached_family(&orders) {
                self.context.pending_orders.push(order);
            }
            return;
        }
        let (instrument, contract) =
            if quote { indicative.unwrap() } else { (instrument, contract) };
        let started = Instant::now();
        if mark {
            self.shared.market.note_pricing_mark_rejected(instrument, false);
            self.farm.acquire_attached_mark(
                instrument,
                &contract,
                &mut self.farm_conn,
                &mut self.hb,
            );
        } else {
            self.farm.acquire_attached_quote(
                instrument,
                &contract,
                &self.shared,
                &mut self.farm_conn,
                &mut self.hb,
            );
        }
        self.attached_quote_orders.push(WaitingFamily {
            orders,
            contract,
            instrument,
            started,
            next_poll: started + Duration::from_secs(1),
            mark,
        });
    }

    fn resolve_attached_quote(&mut self, parent: &Contract, con_id: u32, exchange: &str) {
        if let Some(lookup) = self
            .attached_quote_lookups
            .iter_mut()
            .find(|lookup| lookup.con_id == con_id && lookup.exchange == exchange)
        {
            if !lookup.parents.iter().any(|held| {
                crate::client_core::attached_quote_contract::key(held)
                    == crate::client_core::attached_quote_contract::key(parent)
            }) {
                lookup.parents.push(parent.clone());
            }
            return;
        }
        let req_id = self.ccp.next_internal_secdef_id;
        self.ccp.next_internal_secdef_id = self.ccp.next_internal_secdef_id.wrapping_add(1);
        self.attached_quote_lookups.push(QuoteLookup {
            req_id,
            con_id,
            exchange: exchange.into(),
            parents: vec![parent.clone()],
        });
        self.ccp.send_attached_quote_definition(
            req_id,
            con_id,
            exchange,
            &mut self.ccp_conn,
            &mut self.hb,
            &self.shared,
        );
    }

    fn finish_attached_quote_lookups(&mut self) {
        use crate::client_core::attached_quote_contract::{self, CachedSelection, Selection};
        for lookup in std::mem::take(&mut self.attached_quote_lookups) {
            if self.ccp.pending_secdef.iter().any(|(req_id, ..)| *req_id == lookup.req_id) {
                self.attached_quote_lookups.push(lookup);
                continue;
            }
            if let Some(definition) =
                self.shared.reference.contract_definition_exact(lookup.con_id, &lookup.exchange)
            {
                for parent in lookup.parents {
                    if let Selection::Resolve { con_id, exchange } =
                        attached_quote_contract::complete(&self.shared, &parent, definition.clone())
                    {
                        self.resolve_attached_quote(&parent, con_id, &exchange);
                    }
                }
            } else {
                for parent in lookup.parents {
                    self.shared.reference.cache_attached_quote_contract(
                        attached_quote_contract::key(&parent),
                        CachedSelection::Unavailable,
                    );
                }
            }
        }
    }

    fn attached_quote_ready(&self, instrument: InstrumentId, contract: &Contract) -> bool {
        let held = self.shared.market.pricing_quote_views(instrument);
        if self.shared.market.failure_for_follower(instrument).is_some()
            || matches!(held.mode, 1 | 3)
        {
            return true;
        }
        if !held.confirmed {
            return false;
        }
        let definition =
            crate::client_core::attached_orders::contract_definition(&self.shared, contract);
        let negative_allowed = definition
            .as_ref()
            .and_then(|definition| definition.market_rule_id)
            .and_then(|id| self.shared.reference.market_rule(id as i32))
            .is_some_and(|rule| rule.negative_prices);
        let quotes = cached_quote_views(
            &self.shared.market,
            instrument,
            negative_allowed,
            contract.sec_type == "IND",
            matches!(contract.sec_type.as_str(), "BAG" | "COMB"),
            contract.sec_type == "FUT"
                && !self.shared.reference.enables("NO_NEGATIVE_CLOSE_FOR_FUT"),
            false,
        );
        (quotes.regular.last.is_some() || quotes.regular.close.is_some())
            && (quotes.regular.bid_available || quotes.regular.ask_available)
    }

    fn attached_mark_needed(
        &self,
        instrument: InstrumentId,
        contract: &Contract,
        parent: &Order,
    ) -> bool {
        if matches!(contract.sec_type.as_str(), "BAG" | "COMB")
            || crate::client_core::attached_orders::contract_definition(&self.shared, contract)
                .is_none_or(|definition| definition.ev_rule.is_empty())
            || self.shared.market.pricing_quote_views(instrument).mark_price.is_some()
        {
            return false;
        }
        contract.sec_type == "SLB"
            || matches!(
                parent.order_type.as_str(),
                "MKT"
                    | "MKT PRT"
                    | "MTL"
                    | "PEG MKT"
                    | "PEG MID"
                    | "PEG BEST"
                    | "MIDPRICE"
                    | "SNAP MKT"
                    | "SNAP MID"
                    | "SNAP PRIM"
                    | "MOC"
                    | "BOX TOP"
                    | "REL"
                    | "RPI"
                    | "PASSV REL"
                    | "VOL"
                    | "PPV"
                    | "PDV"
                    | "PMV"
                    | "PSV"
                    | "FUNARI"
            )
    }

    fn release_attached_observer(&mut self, family: &WaitingFamily) {
        if family.mark {
            self.farm.release_attached_mark(
                family.instrument,
                &self.shared,
                &mut self.farm_conn,
                &mut self.hb,
            );
        } else {
            self.farm.release_attached_quote(
                family.instrument,
                &self.shared,
                &mut self.farm_conn,
                &mut self.hb,
            );
        }
    }

    pub(super) fn poll_attached_quotes(&mut self, now: Instant) {
        self.finish_attached_quote_lookups();
        let mut pending = std::mem::take(&mut self.attached_quote_orders);
        for mut family in pending.drain(..) {
            family.orders = self.shared.orders.waiting_attached_family(&family.orders);
            if family.orders.is_empty() {
                self.release_attached_observer(&family);
                continue;
            }
            let rejected = self.shared.market.failure_for_follower(family.instrument).is_some();
            let held = self.shared.market.pricing_quote_views(family.instrument);
            let mark_ready = family.mark
                && (held.mark_rejected
                    || held.mark_price.is_some()
                    || now.saturating_duration_since(family.started) >= Duration::from_secs(2));
            if rejected
                || mark_ready
                || (!family.mark
                    && now >= family.next_poll
                    && (now.saturating_duration_since(family.started) > Duration::from_secs(10)
                        || self.attached_quote_ready(family.instrument, &family.contract)))
            {
                self.release_attached_observer(&family);
                for order in self.shared.orders.take_waiting_attached_family(&family.orders) {
                    self.context.pending_orders.push(order);
                }
            } else {
                if now >= family.next_poll {
                    family.next_poll = now + Duration::from_secs(1);
                }
                self.attached_quote_orders.push(family);
            }
        }
    }

    pub(super) fn queue_order_during_quote_wait(&mut self, request: OrderRequest) {
        match &request {
            OrderRequest::Cancel { order_id, .. } => {
                if self.cancel_waiting_attached(Some(*order_id), None) {
                    return;
                }
            }
            OrderRequest::CancelAll { instrument, .. } => {
                self.cancel_waiting_attached(None, Some(*instrument));
            }
            OrderRequest::Modify { .. }
                if self.shared.orders.amend_waiting_attached(&request, &[]) =>
            {
                return;
            }
            _ => {}
        }
        self.context.pending_orders.push(request);
    }

    pub(super) fn cancel_waiting_attached(
        &mut self,
        order_id: Option<u64>,
        instrument: Option<InstrumentId>,
    ) -> bool {
        let mut cancelled = Vec::new();
        let mut families = std::mem::take(&mut self.attached_quote_orders);
        for mut family in families.drain(..) {
            family.orders = self.shared.orders.waiting_attached_family(&family.orders);
            let all = (order_id.is_none() && instrument.is_none())
                || instrument.is_some()
                    && family.orders.first().is_some_and(|order| order.instrument() == instrument)
                || family.orders.first().is_some_and(|order| Some(order.order_id()) == order_id);
            if all {
                cancelled.extend(self.shared.orders.take_waiting_attached_family(&family.orders));
                self.release_attached_observer(&family);
            } else {
                if let Some(order_id) = order_id
                    && family.orders.iter().any(|order| order.order_id() == order_id)
                {
                    if let Some(order) = self.shared.orders.take_waiting_attached_order(order_id) {
                        cancelled.push(order);
                    }
                    family.orders.retain(|order| order.order_id() != order_id);
                }
                self.attached_quote_orders.push(family);
            }
        }
        let found = !cancelled.is_empty();
        for order in cancelled {
            self.forget_waiting_attachment(order.order_id());
            if let Some(update) = self.shared.orders.discard_waiting(&order, self.context.now_ns())
            {
                super::emit(&self.event_tx, crate::bridge::Event::OrderUpdate(update));
            }
        }
        found
    }

    pub(super) fn stop_attached_quotes(&mut self) {
        for family in std::mem::take(&mut self.attached_quote_orders) {
            self.release_attached_observer(&family);
            for order in self.shared.orders.take_waiting_attached_family(&family.orders) {
                self.context.pending_orders.push(order);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::SharedState;
    use crate::bridge::{PRICING_BID, PRICING_BID_SIZE, PRICING_CLOSE};
    use crate::types::{OrderAttrs, OrderKind, Quote, Side};
    use std::sync::Arc;

    fn family() -> Vec<OrderRequest> {
        [10, 11, 12]
            .into_iter()
            .map(|order_id| OrderRequest::SubmitEx {
                order_id,
                instrument: 0,
                side: Side::Buy,
                con_id: 218,
                kind: OrderKind::Limit { price: 100 },
                qty: 1,
                tif: b'0',
                attrs: OrderAttrs {
                    parent_id: if order_id == 10 { 0 } else { 10 },
                    ..Default::default()
                },
            })
            .collect()
    }

    fn contract() -> Contract {
        Contract {
            con_id: 218,
            symbol: "ABC".into(),
            sec_type: "STK".into(),
            exchange: "SMART".into(),
            ..Contract::default()
        }
    }

    fn accept(
        engine: &mut HotLoop,
        orders: Vec<OrderRequest>,
        contract: Contract,
        parent: Order,
        instrument: InstrumentId,
    ) {
        engine.shared.orders.stage_waiting_attached(&orders);
        engine.accept_attached_orders(orders, contract, parent, instrument);
    }

    fn quoted(loop_: &HotLoop) {
        loop_.shared.market.note_pricing_subscription(0, 0, true);
        loop_.shared.market.push_pricing_quote(
            0,
            0,
            &Quote { bid: 100, bid_size: 1, close: 100, ..Quote::default() },
            PRICING_BID | PRICING_BID_SIZE | PRICING_CLOSE,
            0,
        );
    }

    #[test]
    fn a_family_waits_for_a_confirmed_quote_then_keeps_its_order_and_prices() {
        let shared = Arc::new(SharedState::new());
        let mut engine = HotLoop::new(shared, None, None);
        accept(&mut engine, family(), contract(), Order::default(), 0);
        assert_eq!(engine.context.pending_orders.drain().count(), 0);
        let started = engine.attached_quote_orders[0].started;
        quoted(&engine);
        engine.poll_attached_quotes(started + Duration::from_millis(999));
        assert_eq!(engine.context.pending_orders.drain().count(), 0);
        engine.poll_attached_quotes(started + Duration::from_secs(1));
        let orders: Vec<_> = engine.context.pending_orders.drain().collect();
        assert_eq!(orders.len(), 3);
        for (request, expected) in orders.into_iter().zip([10, 11, 12]) {
            assert!(
                matches!(request, OrderRequest::SubmitEx { order_id, kind: OrderKind::Limit { price: 100 }, .. } if order_id == expected)
            );
        }
        assert!(!engine.farm.holds_market_data(0));
    }

    #[test]
    fn rejection_and_timeout_continue_without_a_price() {
        for rejection in [false, true] {
            let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
            accept(&mut engine, family(), contract(), Order::default(), 0);
            let started = engine.attached_quote_orders[0].started;
            if rejection {
                engine.shared.market.push_subscription_failure(0, "Not subscribed".into());
                engine.poll_attached_quotes(started);
            } else {
                engine.poll_attached_quotes(started + Duration::from_secs(10));
                assert_eq!(engine.context.pending_orders.drain().count(), 0);
                engine.poll_attached_quotes(started + Duration::from_secs(11));
            }
            assert_eq!(engine.context.pending_orders.drain().count(), 3);
            assert!(!engine.farm.holds_market_data(0));
        }
    }

    #[test]
    fn the_override_and_delayed_feed_skip_quote_acquisition() {
        for delayed in [false, true] {
            let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
            if delayed {
                engine.shared.market.note_pricing_subscription(0, 1, false);
            }
            accept(
                &mut engine,
                family(),
                contract(),
                Order { override_percentage_constraints: !delayed, ..Order::default() },
                0,
            );
            assert_eq!(engine.context.pending_orders.drain().count(), 3);
            assert!(!engine.farm.holds_market_data(0));
        }
    }

    #[test]
    fn market_orders_with_an_ev_rule_wait_for_a_mark_for_two_seconds() {
        for with_mark in [false, true] {
            let shared = Arc::new(SharedState::new());
            let definition = crate::control::contracts::ContractDefinition {
                con_id: 218,
                exchange: "SMART".into(),
                ev_rule: "aussieBond".into(),
                ..Default::default()
            };
            shared.reference.cache_contract_definition(definition);
            let mut engine = HotLoop::new(shared, None, None);
            accept(
                &mut engine,
                family(),
                contract(),
                Order {
                    order_type: "MKT".into(),
                    override_percentage_constraints: true,
                    ..Order::default()
                },
                0,
            );
            assert!(engine.attached_quote_orders[0].mark);
            assert!(!engine.farm.holds_a_stream(0));
            let started = engine.attached_quote_orders[0].started;
            engine.poll_attached_quotes(started + Duration::from_millis(1999));
            assert_eq!(engine.context.pending_orders.drain().count(), 0);
            if with_mark {
                engine.shared.market.note_pricing_mark(0, Some(100.5));
                engine.poll_attached_quotes(started + Duration::from_millis(1999));
            } else {
                engine.poll_attached_quotes(started + Duration::from_secs(2));
            }
            assert_eq!(engine.context.pending_orders.drain().count(), 3);
            assert!(!engine.farm.holds_market_data(0));
        }
    }

    #[test]
    fn limit_and_stop_orders_do_not_wait_for_an_ev_mark() {
        let shared = Arc::new(SharedState::new());
        let definition = crate::control::contracts::ContractDefinition {
            con_id: 218,
            exchange: "SMART".into(),
            ev_rule: "aussieBond".into(),
            ..Default::default()
        };
        shared.reference.cache_contract_definition(definition);
        let mut engine = HotLoop::new(shared, None, None);
        for order_type in ["LMT", "STP", "TRAIL", "TRAIL LMT", "PEG STK"] {
            accept(
                &mut engine,
                family(),
                contract(),
                Order {
                    order_type: order_type.into(),
                    override_percentage_constraints: true,
                    ..Order::default()
                },
                0,
            );
            assert_eq!(engine.context.pending_orders.drain().count(), 3);
            assert!(engine.attached_quote_orders.is_empty());
        }
    }

    #[test]
    fn cancelling_a_waiting_parent_cancels_its_children_without_submission() {
        let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
        accept(&mut engine, family(), contract(), Order::default(), 0);
        engine.queue_order_during_quote_wait(OrderRequest::Cancel {
            order_id: 10,
            stated: Default::default(),
        });
        assert!(engine.attached_quote_orders.is_empty());
        assert!(!engine.farm.holds_market_data(0));
        assert_eq!(engine.context.pending_orders.drain().count(), 0);
        let updates = engine.shared.orders.drain_order_updates();
        assert_eq!(updates.iter().map(|update| update.order_id).collect::<Vec<_>>(), [10, 11, 12]);
        assert!(updates.iter().all(|update| update.status == crate::types::OrderStatus::Cancelled));
        assert_eq!(
            engine.shared.orders.drain_order_notices(),
            vec![
                (10, 202, "Order was discarded".into()),
                (11, 202, "Order was discarded".into()),
                (12, 202, "Order was discarded".into()),
            ]
        );
    }

    #[test]
    fn cancelling_one_waiting_child_leaves_its_parent_and_sibling() {
        let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
        accept(&mut engine, family(), contract(), Order::default(), 0);
        let started = engine.attached_quote_orders[0].started;
        engine.queue_order_during_quote_wait(OrderRequest::Cancel {
            order_id: 11,
            stated: Default::default(),
        });
        quoted(&engine);
        engine.poll_attached_quotes(started + Duration::from_secs(1));
        assert_eq!(
            engine.context.pending_orders.drain().map(|order| order.order_id()).collect::<Vec<_>>(),
            [10, 12]
        );
        assert_eq!(engine.shared.orders.drain_order_updates()[0].order_id, 11);
    }

    #[test]
    fn cancelling_all_includes_waiting_families() {
        let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
        accept(&mut engine, family(), contract(), Order::default(), 0);
        engine.queue_order_during_quote_wait(OrderRequest::CancelAll {
            instrument: 0,
            stated: Default::default(),
        });
        assert!(engine.attached_quote_orders.is_empty());
        assert_eq!(engine.shared.orders.drain_order_updates().len(), 3);
        assert!(matches!(
            engine.context.pending_orders.drain().next(),
            Some(OrderRequest::CancelAll { .. })
        ));
    }

    #[test]
    fn changing_a_waiting_order_changes_its_first_submission() {
        let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
        accept(&mut engine, family(), contract(), Order::default(), 0);
        let started = engine.attached_quote_orders[0].started;
        engine.queue_order_during_quote_wait(OrderRequest::Modify {
            order_id: 10,
            price: 120,
            qty: 2,
            outside_rth: true,
            ord_type: b'2',
            tif: b'1',
            stop_price: 0,
            spec: Some(Box::new(crate::types::OrderSpec {
                kind: OrderKind::Limit { price: 120 },
                attrs: OrderAttrs { outside_rth: true, ..OrderAttrs::default() },
            })),
        });
        assert_eq!(engine.context.pending_orders.drain().count(), 0);
        quoted(&engine);
        engine.poll_attached_quotes(started + Duration::from_secs(1));
        assert!(matches!(
            engine.context.pending_orders.drain().next(),
            Some(OrderRequest::SubmitEx {
                order_id: 10,
                qty: 2,
                tif: b'1',
                kind: OrderKind::Limit { price: 120 },
                attrs: OrderAttrs { outside_rth: true, .. },
                ..
            })
        ));
    }

    #[test]
    fn direct_amendments_and_new_children_reach_the_first_submission_together() {
        let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
        let mut orders = family();
        for request in &mut orders {
            if let OrderRequest::SubmitEx { order_id, attrs, .. } = request {
                attrs.parent_id = if *order_id == 10 { 0 } else { 10 };
                let attached = attrs.attached_mut();
                attached.family_key = "family".into();
                attached.contract_id = Some(218);
                attached.ratio_factor = Some(2.0);
                attached.combo =
                    Some(crate::types::AttachedComboFrame { price_mode: 3, ..Default::default() });
                attrs.oca_group_str = "original".into();
                attrs.oca_type = 2;
                attrs.outside_rth = true;
            }
        }
        accept(&mut engine, orders[..2].to_vec(), contract(), Order::default(), 0);
        let started = engine.attached_quote_orders[0].started;
        let change_with = |price, tif| OrderRequest::Modify {
            order_id: 10,
            price,
            qty: 2,
            outside_rth: false,
            ord_type: b'2',
            tif,
            stop_price: 0,
            spec: Some(Box::new(crate::types::OrderSpec {
                kind: OrderKind::Limit { price },
                attrs: OrderAttrs::default(),
            })),
        };
        let change = |price| change_with(price, b'1');
        assert!(engine.shared.orders.amend_waiting_attached(&change(110), &orders[2..]));
        // A change that states no TIF and no group type keeps the ones held.
        assert!(engine.shared.orders.amend_waiting_attached(&change_with(120, 0), &[]));
        quoted(&engine);
        engine.poll_attached_quotes(started + Duration::from_secs(1));
        let sent: Vec<_> = engine.context.pending_orders.drain().collect();
        assert_eq!(sent.iter().map(OrderRequest::order_id).collect::<Vec<_>>(), [10, 11, 12]);
        let OrderRequest::SubmitEx { kind, attrs, qty, tif, .. } = &sent[0] else { panic!() };
        assert!(matches!(kind, OrderKind::Limit { price: 120 }));
        assert_eq!((*qty, *tif), (2, b'1'));
        assert!(!attrs.outside_rth);
        assert_eq!(attrs.oca_group_str, "original");
        assert_eq!(attrs.oca_type, 2);
        assert_eq!(attrs.attached.as_ref().unwrap().family_key, "family");
        assert_eq!(attrs.attached.as_ref().and_then(|held| held.contract_id), Some(218));
        assert_eq!(attrs.attached.as_ref().and_then(|held| held.ratio_factor), Some(2.0));
        assert_eq!(
            attrs
                .attached
                .as_ref()
                .and_then(|held| held.combo.as_ref())
                .map(|frame| frame.price_mode),
            Some(3)
        );
        assert!(!engine.shared.orders.amend_waiting_attached(&change(130), &[]));
        assert!(!engine.shared.orders.is_waiting_attached(12));
    }

    #[test]
    fn cancelling_a_waiting_family_includes_a_child_added_during_the_wait() {
        for cancel_parent in [false, true] {
            let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
            let mut orders = family();
            if let OrderRequest::SubmitEx { attrs, .. } = &mut orders[2] {
                attrs.parent_id = 10;
            }
            accept(&mut engine, orders[..2].to_vec(), contract(), Order::default(), 0);
            let started = engine.attached_quote_orders[0].started;
            let change = OrderRequest::Modify {
                order_id: 10,
                price: 100,
                qty: 1,
                outside_rth: false,
                ord_type: b'2',
                tif: b'0',
                stop_price: 0,
                spec: Some(Box::new(crate::types::OrderSpec {
                    kind: OrderKind::Limit { price: 100 },
                    attrs: OrderAttrs::default(),
                })),
            };
            assert!(engine.shared.orders.amend_waiting_attached(&change, &orders[2..]));
            engine.queue_order_during_quote_wait(OrderRequest::Cancel {
                order_id: if cancel_parent { 10 } else { 12 },
                stated: Default::default(),
            });
            assert_eq!(
                engine.shared.orders.drain_order_updates().len(),
                if cancel_parent { 3 } else { 1 }
            );
            quoted(&engine);
            engine.poll_attached_quotes(started + Duration::from_secs(1));
            let sent: Vec<_> =
                engine.context.pending_orders.drain().map(|order| order.order_id()).collect();
            assert_eq!(sent, if cancel_parent { vec![] } else { vec![10, 11] });
            assert!(!engine.shared.orders.is_waiting_attached(12));
        }
    }

    #[test]
    fn a_new_child_joins_a_family_that_is_waiting() {
        let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
        accept(&mut engine, family()[..2].to_vec(), contract(), Order::default(), 0);
        let started = engine.attached_quote_orders[0].started;
        accept(&mut engine, family(), contract(), Order::default(), 0);
        assert_eq!(engine.attached_quote_orders.len(), 1);
        assert_eq!(engine.attached_quote_orders[0].started, started);
        assert_eq!(
            engine.attached_quote_orders[0]
                .orders
                .iter()
                .map(OrderRequest::order_id)
                .collect::<Vec<_>>(),
            [10, 11, 12]
        );
    }

    #[test]
    fn a_proxy_quote_uses_its_own_slot_and_is_removed_with_its_parent() {
        let shared = Arc::new(SharedState::new());
        shared.reference.set_enabled_features(vec!["USESTKMD".into()]);
        let derivative = Contract {
            con_id: 218,
            sec_type: "CFD".into(),
            exchange: "SMART".into(),
            ..Default::default()
        };
        shared.reference.cache_contract_definition(crate::control::contracts::ContractDefinition {
            con_id: 218,
            sec_type: crate::control::contracts::SecurityType::Cfd,
            exchange: "SMART".into(),
            under_con_id: 219,
            under_sec_type: "STK".into(),
            order_type_key: "X".into(),
            order_type_rules: vec![("USESTKMD".into(), 0)],
            ..Default::default()
        });
        shared.reference.cache_contract_definition(crate::control::contracts::ContractDefinition {
            con_id: 219,
            sec_type: crate::control::contracts::SecurityType::Stock,
            exchange: "SMART".into(),
            ..Default::default()
        });
        let mut engine = HotLoop::new(shared, None, None);
        let trade = engine.context.market.register(218);
        accept(&mut engine, family(), derivative, Order::default(), trade);
        let proxy = engine.attached_quote_orders[0].instrument;
        assert_ne!(proxy, trade);
        assert_eq!(engine.shared.market.attached_quote_instrument(219, "SMART"), Some(proxy));
        engine.queue_order_during_quote_wait(OrderRequest::CancelAll {
            instrument: trade,
            stated: Default::default(),
        });
        assert!(engine.attached_quote_orders.is_empty());
        assert!(!engine.farm.holds_market_data(proxy));
    }

    #[test]
    fn missing_proxy_resolution_does_not_hold_the_order() {
        let shared = Arc::new(SharedState::new());
        shared.reference.set_enabled_features(vec!["USESTKMD".into()]);
        let derivative = Contract {
            con_id: 218,
            sec_type: "CFD".into(),
            exchange: "SMART".into(),
            ..Default::default()
        };
        shared.reference.cache_contract_definition(crate::control::contracts::ContractDefinition {
            con_id: 218,
            sec_type: crate::control::contracts::SecurityType::Cfd,
            exchange: "SMART".into(),
            under_con_id: 219,
            under_sec_type: "STK".into(),
            order_type_key: "X".into(),
            order_type_rules: vec![("USESTKMD".into(), 0)],
            ..Default::default()
        });
        let mut engine = HotLoop::new(shared, None, None);
        accept(&mut engine, family(), derivative.clone(), Order::default(), 0);
        assert!(engine.attached_quote_orders.is_empty());
        assert_eq!(engine.context.pending_orders.drain().count(), 3);
        assert_eq!(engine.attached_quote_lookups.len(), 1);
        engine.shared.reference.cache_contract_definition(
            crate::control::contracts::ContractDefinition {
                con_id: 219,
                sec_type: crate::control::contracts::SecurityType::Stock,
                exchange: "SMART".into(),
                ..Default::default()
            },
        );
        engine.ccp.pending_secdef.clear();
        engine.poll_attached_quotes(Instant::now());
        assert!(engine.attached_quote_lookups.is_empty());
        assert!(
            crate::client_core::attached_quote_contract::cached(&engine.shared, &derivative)
                .is_some_and(|proxy| proxy.con_id == 219)
        );
    }

    #[test]
    fn cached_prices_wait_for_confirmation() {
        let mut engine = HotLoop::new(Arc::new(SharedState::new()), None, None);
        quoted(&engine);
        engine.shared.market.note_pricing_subscription(0, 0, false);
        accept(&mut engine, family(), contract(), Order::default(), 0);
        assert_eq!(engine.attached_quote_orders.len(), 1);
        assert_eq!(engine.context.pending_orders.drain().count(), 0);
        engine.stop_attached_quotes();
        assert_eq!(engine.context.pending_orders.drain().count(), 3);
    }
}
