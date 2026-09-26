//! What a gateway recovers once the trading connection is back: the orders
//! the drop left sent to the venue and not yet answered.
//!
//! They are kept as the connection goes. Once the connection that replaced it
//! has named what is working, the venue is asked what it has finished today,
//! and its answer is read as any report is, so an order it names in either
//! answer takes the state the venue gives it. An order named in neither is
//! held as inactive and not sent again: a gateway sends no order of a program's
//! twice. Where the recovery had sent the order out once already, the program
//! is told under 106 that it could not be sent. The placements the drop left
//! waiting go out once that is over, and a question of what is working is
//! answered then.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::CcpState;
use crate::bridge::SharedState;
use crate::engine::context::Context;
use crate::engine::hot_loop::HeartbeatState;
use crate::protocol::connection::Connection;
use crate::protocol::fix;
use crate::types::OrderStatus;
use crate::types::model as api;

/// How long a gateway gives the recovery, from the logon that begins it.
const RECOVERY_BOUND: Duration = Duration::from_secs(60);

/// The code a gateway tells a program under that an order of its could not
/// be sent.
const CANNOT_TRANSMIT: i32 = 106;

/// Where the recovery of one drop stands.
#[derive(Default)]
pub(crate) struct Recovery {
    /// The orders the drop left sent and unanswered, by the number each went
    /// out under.
    unanswered: Vec<u64>,
    /// When it is given up on: a minute from the logon that began it.
    until: Option<Instant>,
    /// Whether the connection now up began it. One that replaced it after it
    /// began leaves it to its bound, as a gateway does.
    here: bool,
    /// Where the question of what the venue has finished stands.
    question: Question,
    /// Whether what the drop left waiting may go out.
    over: bool,
    /// The placements a recovery has sent out once, which a gateway does not
    /// send out again.
    sent_once: HashSet<u64>,
    /// The latest time a report stated about an order, which the question
    /// asks back to.
    latest_report: i64,
}

#[derive(Default, PartialEq)]
enum Question {
    #[default]
    None,
    /// To be asked on the next pass.
    Due,
    /// Asked; its answer ends at the report stating its contract `*`.
    Out,
}

impl Recovery {
    /// Whether the placements the drop left waiting may go out.
    pub(crate) fn is_over(&self) -> bool {
        self.over
    }

    /// Whether the recovery has sent this placement out once already.
    pub(crate) fn sent_once(&self, order_id: u64) -> bool {
        self.sent_once.contains(&order_id)
    }

    /// The recovery sends these placements out: a gateway sends each out once.
    pub(crate) fn sends_out(&mut self, ids: impl IntoIterator<Item = u64>) {
        self.sent_once.extend(ids);
    }

    /// A report stated this time about an order.
    pub(crate) fn note_report_time(&mut self, parsed: &std::collections::HashMap<u32, String>) {
        if let Some(at) = parsed
            .get(&6699)
            .or_else(|| parsed.get(&60))
            .and_then(|stated| crate::protocol::datetime::ib_datetime_to_unix_millis(stated))
        {
            self.latest_report = self.latest_report.max(at);
        }
    }
}

impl CcpState {
    /// The trading connection went: keep the orders it leaves sent and
    /// unanswered, where no recovery is already keeping some, and hold what is
    /// left waiting until the next one is over.
    pub(crate) fn keep_what_the_drop_leaves(&mut self, context: &Context, shared: &SharedState) {
        let recovery = &mut self.recovery;
        recovery.over = false;
        shared.orders.hold_open_orders(true);
        if recovery.question == Question::Out {
            // The answer the question was waiting on goes with the
            // connection, and the recovery with it: the next connection is not
            // asked again, and the bound ends it.
            recovery.question = Question::None;
            self.completed_orders_open = false;
        } else if recovery.question == Question::Due {
            recovery.question = Question::None;
        }
        if recovery.until.is_some() {
            recovery.here = false;
            return;
        }
        if recovery.unanswered.is_empty() {
            recovery.unanswered = context.unanswered_orders();
        }
    }

    /// A logon: where the drop left orders to recover and none is under way,
    /// this connection begins it.
    pub(crate) fn begin_the_recovery(&mut self) {
        let recovery = &mut self.recovery;
        if !recovery.unanswered.is_empty() && recovery.until.is_none() {
            recovery.until = Some(Instant::now() + RECOVERY_BOUND);
            recovery.here = true;
        }
    }

    /// The venue ended what it names, or an answer to what it has finished:
    /// ask what it has finished where this connection is recovering orders,
    /// judge them where that answer is the one ending, and otherwise let go of
    /// what the drop left waiting.
    pub(crate) fn recover_at_the_end(
        &mut self,
        context: &mut Context,
        shared: &SharedState,
        ours: bool,
    ) {
        if ours {
            self.recovery.question = Question::None;
            self.judge_what_the_drop_left(context, shared);
            self.let_go_of_what_waited(shared);
            return;
        }
        let recovery = &mut self.recovery;
        if recovery.here && !recovery.unanswered.is_empty() && recovery.question == Question::None {
            recovery.question = Question::Due;
            return;
        }
        if recovery.question == Question::None {
            self.let_go_of_what_waited(shared);
        }
    }

    /// Whether the answer being assembled is the recovery's own.
    pub(crate) fn the_answer_is_the_recoverys(&self) -> bool {
        self.completed_orders_open && self.recovery.question == Question::Out
    }

    fn let_go_of_what_waited(&mut self, shared: &SharedState) {
        self.recovery.over = true;
        shared.orders.hold_open_orders(false);
        if self.recovery_sweep_at.is_some() {
            self.recovery_sweep_at = Some(Instant::now());
        }
    }

    /// Ask the venue what it has finished, where the recovery is due to, and
    /// give the recovery up once its bound has passed.
    pub(crate) fn carry_the_recovery(
        &mut self,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
        context: &Context,
    ) {
        if self.recovery.until.is_some_and(|until| Instant::now() >= until) {
            log::warn!(
                "the orders the drop left sent and unanswered were not accounted for within \
                 the recovery's minute: {:?}",
                self.recovery.unanswered,
            );
            // Given up, as a gateway gives it up: a question of what is
            // working is answered, and what waited goes out at the next end.
            self.recovery.unanswered.clear();
            self.recovery.until = None;
            self.recovery.here = false;
            if self.recovery.question == Question::Due {
                self.recovery.question = Question::None;
            }
            shared.orders.hold_open_orders(false);
        }
        if self.recovery.question != Question::Due || self.completed_orders_open {
            return;
        }
        let Some(conn) = ccp_conn.as_mut() else { return };
        self.recovery.question = Question::Out;
        let ts = crate::protocol::datetime::chrono_free_timestamp();
        let mut fields: Vec<(u32, String)> = vec![
            (fix::TAG_MSG_TYPE, "H".into()),
            (fix::TAG_SENDING_TIME, ts.to_string()),
            (11, "*".into()),
            (55, "*".into()),
            (54, "*".into()),
            (6533, "1".into()),
        ];
        // From the orders' own time, where the logon offers a question bounded
        // in time; otherwise the question states none.
        if shared.reference.enables("1DAYSORDER") {
            let since = self.recovery.asked_back_to(context);
            fields.push((
                6536,
                crate::protocol::datetime::unix_to_ib_utc_dash(since.div_euclid(1_000)),
            ));
        }
        let sent = conn.send_fix(
            &fields.iter().map(|(tag, value)| (*tag, value.as_str())).collect::<Vec<_>>(),
        );
        match sent {
            Ok(()) => {
                hb.last_ccp_sent = Instant::now();
                self.orders_in_this_answer.clear();
                self.the_answer_is_full = false;
                self.completed_orders_open = true;
                log::info!(
                    "Asked the venue what it has finished, for the orders the drop left unanswered"
                );
            }
            // Left for the bound to end, as a gateway leaves a question it
            // could not send.
            Err(e) => {
                log::warn!("the question of what the venue has finished could not be sent: {e}")
            }
        }
    }

    /// Each order the drop left sent and unanswered, against what the venue
    /// has named and what it has said it finished: taken as it stated it where
    /// it stated anything, and otherwise held as inactive and not sent again.
    fn judge_what_the_drop_left(&mut self, context: &mut Context, shared: &SharedState) {
        let unanswered = std::mem::take(&mut self.recovery.unanswered);
        self.recovery.until = None;
        self.recovery.here = false;
        for (at, &order_id) in unanswered.iter().enumerate() {
            if !context.order(order_id).is_some_and(|order| order.status == OrderStatus::Uncertain)
            {
                continue;
            }
            context.set_order_status_forced(order_id, OrderStatus::Inactive);
            if let Some(mut info) = shared.orders.get_order_info(order_id) {
                info.order_state.status = "Inactive".into();
                shared.orders.push_order_info(order_id, info);
            }
            // One of a family is judged with the last of it still to be
            // judged, as a gateway transmits a family from its last member.
            let parent =
                |id: u64| context.submitted.get(&id).map_or(0, |spec| spec.attrs.parent_id);
            let family_to_come = unanswered[at + 1..].iter().any(|later| {
                parent(*later) == order_id
                    || (parent(order_id) != 0 && parent(*later) == parent(order_id))
            });
            if family_to_come || !self.recovery.sent_once.contains(&order_id) {
                continue;
            }
            if let Some(description) =
                super::executions::order_description(order_id, context, shared)
            {
                let api_id = shared
                    .orders
                    .attached_order_metadata(order_id)
                    .and_then(|held| held.api_order_id)
                    .unwrap_or(order_id as i64);
                shared.orders.push_order_notice(
                    order_id,
                    api::OrderOp::Place,
                    CANNOT_TRANSMIT,
                    format!("Can't transmit order id:{api_id}, {description}"),
                );
            }
        }
    }
}

impl Recovery {
    /// How far back the question asks, as a gateway reckons it: a second
    /// before the earliest of the orders went out, or the last time a report
    /// stated where one has and that is earlier, and then not before a day
    /// and a second back from now.
    fn asked_back_to(&self, context: &Context) -> i64 {
        let went_out = self
            .unanswered
            .iter()
            .filter_map(|id| context.placed_at.get(id).map(|(at, _)| *at))
            .filter(|at| *at > 0)
            .min()
            .map_or(i64::MAX, |at| at - 1_000);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as i64);
        let floor = now - 86_400_000 + 1_000;
        if self.latest_report == 0 {
            return if went_out == i64::MAX { floor } else { went_out };
        }
        went_out.min(self.latest_report).max(floor)
    }
}
