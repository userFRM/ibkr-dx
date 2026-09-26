//! The session's one order.
//!
//! A gateway writes every message to its client down one socket, so a TWS
//! client reads everything in one total order: a refusal arrives after
//! everything written before it, a fill of an order before the error about a
//! change to it. The engine keeps a queue per kind of record instead, so each
//! record takes a stamp from one counter as it is pushed, inside the lock of
//! the queue it goes into. A read takes the counter's value once, takes from
//! every queue exactly the records stamped below it, and delivers them in
//! stamp order.
//!
//! What is not a record is conflated state: a quote, a holding, an account
//! figure, kept as one value that a read compares with what its caller was
//! last told. That has no single place in the order, and is delivered after
//! the records of the read that polled it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::control::contracts::{ContractDefinition, OptionChainScope, SymbolMatch};
use crate::control::histogram::HistogramEntry;
use crate::control::historical::{HeadTimestampResponse, HistoricalResponse};
use crate::control::news::NewsHeadline;
use crate::control::scanner::ScannerResult;
use crate::types::model as api;
use crate::types::*;

use super::{RecordKind, RichOrderInfo, TickReqParams, VenueDataConnection};

/// The counter every record takes its stamp from, shared by every queue of
/// one session, and the mark conflated state leaves when it is written.
#[derive(Clone, Default)]
pub struct Stamps(Arc<AtomicU64>, Arc<AtomicBool>);

impl Stamps {
    /// Conflated state was written: a quote, a holding, an account figure.
    #[inline]
    pub fn note_written(&self) {
        self.1.store(true, Ordering::Release);
    }

    /// Whether conflated state was written since this was last asked.
    #[inline]
    pub fn take_written(&self) -> bool {
        self.1.swap(false, Ordering::AcqRel)
    }

    /// The next stamp. Taken by a pusher inside the lock of the queue it
    /// pushes into, which is what makes a read's cut exact.
    #[inline]
    fn take(&self) -> u64 {
        self.0.fetch_add(1, Ordering::AcqRel)
    }

    /// Every record stamped below this has been pushed into its queue, or is
    /// being pushed under a lock the reader's next take waits on.
    #[inline]
    pub fn cut(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

/// A queue of records of one kind, each held with its stamp.
///
/// Pushed at the tail, so the stamps rise along it; a record a reader left in
/// place — one a call that answers is waiting on — keeps its own.
pub struct Queue<T> {
    stamps: Stamps,
    items: Mutex<Vec<(u64, T)>>,
}

impl<T> Queue<T> {
    /// An empty queue stamping from `stamps`.
    pub fn new(stamps: &Stamps) -> Self {
        Self { stamps: stamps.clone(), items: Mutex::new(Vec::new()) }
    }

    /// The same, with room for `n`.
    pub fn with_capacity(stamps: &Stamps, n: usize) -> Self {
        Self { stamps: stamps.clone(), items: Mutex::new(Vec::with_capacity(n)) }
    }

    /// The queue itself, stamps and all, for a caller that has to look at
    /// more than one entry under one acquisition.
    pub fn lock(&self) -> MutexGuard<'_, Vec<(u64, T)>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Push one record, stamped under the queue's own lock.
    pub fn push(&self, item: T) {
        let mut held = self.lock();
        held.push((self.stamps.take(), item));
    }

    /// Push through a guard this caller already holds, stamped under it.
    pub fn push_held(&self, held: &mut Vec<(u64, T)>, item: T) {
        held.push((self.stamps.take(), item));
    }

    /// Push onto a stream nobody may be draining, oldest out first.
    ///
    /// A tenth at a time rather than one at a time: dropping a single entry
    /// per push leaves every later push doing a full shift of the vector.
    pub fn push_bounded(&self, item: T, limit: usize, what: &str) {
        let mut held = self.lock();
        if held.len() >= limit {
            let drop_to = limit - limit / 10;
            let shed = held.len() - drop_to;
            held.drain(..shed);
            log::warn!(
                "{what} has gone past {limit} unread, so the oldest of them were dropped — \
                 nothing is draining this stream",
            );
        }
        held.push((self.stamps.take(), item));
    }

    /// Take every record, leaving none.
    pub fn drain(&self) -> Vec<T> {
        self.lock().drain(..).map(|(_, item)| item).collect()
    }

    /// Keep only the records `keep` says to keep.
    pub fn retain(&self, mut keep: impl FnMut(&T) -> bool) {
        self.lock().retain(|(_, item)| keep(item));
    }

    /// Take the records `mine` picks, in order, leaving the rest in theirs.
    ///
    /// Partitioned rather than removed one at a time: each removal shifts the
    /// tail, so a pass over a queue that has grown costs the square of it,
    /// under the lock the engine pushes into.
    pub fn take_if(&self, mut mine: impl FnMut(&T) -> bool) -> Vec<T> {
        let mut held = self.lock();
        let (taken, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut *held).into_iter().partition(|(_, item)| mine(item));
        *held = kept;
        taken.into_iter().map(|(_, item)| item).collect()
    }

    /// Take the first record `mine` picks, leaving the rest.
    pub fn take_first(&self, mut mine: impl FnMut(&T) -> bool) -> Option<T> {
        let mut held = self.lock();
        let at = held.iter().position(|(_, item)| mine(item))?;
        Some(held.remove(at).1)
    }

    /// Whether any record answers `test`.
    pub fn any(&self, mut test: impl FnMut(&T) -> bool) -> bool {
        self.lock().iter().any(|(_, item)| test(item))
    }

    /// How many records are waiting.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Take the records stamped below `cut` as `fate` says, each taken one
    /// made a [`Record`] by `wrap` and held with its stamp. `true` leaves a
    /// record where it is and `false` takes it.
    fn take_below<F: Into<Fate>>(
        &self,
        cut: u64,
        fate: impl Fn(&T) -> F,
        wrap: impl Fn(T) -> Record,
        out: &mut Vec<(u64, Record)>,
    ) {
        let mut held = self.lock();
        let end = held.partition_point(|(seq, _)| *seq < cut);
        let mut left = Vec::new();
        for (seq, item) in held.drain(..end) {
            match fate(&item).into() {
                Fate::Take => out.push((seq, wrap(item))),
                Fate::Leave => left.push((seq, item)),
                Fate::Drop => {}
            }
        }
        held.splice(..0, left);
        drop(held);
        #[cfg(test)]
        hooks::run(&hooks::BETWEEN_DRAINS);
    }
}

/// What a read does with one queued record.
#[derive(Clone, Copy)]
enum Fate {
    /// Delivered by this read.
    Take,
    /// Left where it is, for the reader that holds it.
    Leave,
    /// Taken and delivered to nobody.
    Drop,
}

impl From<bool> for Fate {
    fn from(leave: bool) -> Self {
        if leave { Self::Leave } else { Self::Take }
    }
}

/// Points inside a read where a test acts as another thread would: between
/// one queue's drain and the next, and between the conflated state's poll and
/// the cut; and one inside the engine's lap, between its take and what it
/// publishes. Each runs on the thread that reaches it, with no queue's lock
/// held.
#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;
    use std::thread::LocalKey;

    /// A hook: run at its point on the thread that set it, until cleared.
    pub(crate) type Hook = RefCell<Option<Box<dyn FnMut()>>>;

    thread_local! {
        /// After each queue's drain.
        pub(crate) static BETWEEN_DRAINS: Hook = const { RefCell::new(None) };
        /// Before a call stamps an answer for its session.
        pub(crate) static BEFORE_ANSWER_STAMP: Hook = const { RefCell::new(None) };
        /// After step (a), before step (b).
        pub(crate) static BEFORE_THE_CUT: Hook = const { RefCell::new(None) };
        /// In the engine's loop, after a lap has taken its commands off the
        /// channel and before it publishes what it has finished.
        pub(crate) static AFTER_THE_TAKE: Hook = const { RefCell::new(None) };
        /// In a caller stopping the engine, holding the thread's handle and
        /// before its join.
        pub(crate) static BEFORE_THE_JOIN: Hook = const { RefCell::new(None) };
    }

    /// Set the hook at `point`.
    pub(crate) fn set(point: &'static LocalKey<Hook>, hook: impl FnMut() + 'static) {
        point.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    /// Clear the hook at `point`.
    pub(crate) fn clear(point: &'static LocalKey<Hook>) {
        point.with(|h| *h.borrow_mut() = None);
    }

    /// Run the hook at `point`, if one is set. Taken out while it runs, so a
    /// read it starts does not run it again from inside.
    pub(crate) fn run(point: &'static LocalKey<Hook>) {
        let Some(mut hook) = point.with(|h| h.borrow_mut().take()) else { return };
        hook();
        point.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(hook);
            }
        });
    }
}

/// A fill, and the report it was booked off with the status that report
/// stated: one execution report is one record.
#[derive(Clone, Debug)]
pub struct FillRecord {
    /// The print.
    pub fill: Fill,
    /// The report it was booked off, shared with the cache until it changes.
    pub report: Option<std::sync::Arc<RichOrderInfo>>,
    /// The order's status as the same report stated it.
    pub status: Option<OrderUpdate>,
}

/// An order's status, with the order's state as its report stated it.
#[derive(Clone, Debug)]
pub struct UpdateRecord {
    /// The status.
    pub update: OrderUpdate,
    /// What the venue said about the order beside it: margins, cost and
    /// warning, as they stood when the report was pushed, where it had said
    /// anything.
    pub state: Option<std::sync::Arc<RichOrderInfo>>,
    /// The client the venue says placed the order, as it stood then.
    pub client_id: i32,
}

/// An order the engine took from a caller, as the caller's side records it:
/// written where it stands in the session's order, ahead of anything the venue
/// says about the order.
#[derive(Clone, Debug)]
pub struct TakenOrder {
    /// Its number.
    pub order_id: u64,
    /// The contract as the venue was told it, legs and hedge included.
    pub contract: api::Contract,
    /// The order as it went, or as it is kept.
    pub order: api::Order,
    /// The slot its contract holds.
    pub instrument: InstrumentId,
    /// Whether it restates an order the venue is already working, rather than
    /// placing one.
    pub restated: bool,
}

/// A market-data request the engine has taken, as the reader's record of who
/// is watching what is written from it.
#[derive(Clone, Debug)]
pub struct MarketDataTaken {
    /// The caller's number for the request.
    pub req_id: i64,
    /// The slot it is served on.
    pub slot: InstrumentId,
    /// The occupancy that slot is held under.
    pub generation: u64,
    /// The contract it named, zero where the venue has not numbered it yet.
    pub con_id: i64,
    /// The extra series it named.
    pub series: Vec<u32>,
    /// Whether it asked for a snapshot this client ends.
    pub snapshot: bool,
    /// When the engine took the subscription, before any reader delivered it.
    pub asked_at: std::time::Instant,
    /// Whether it asked for the venue's chargeable one-shot.
    pub one_shot: bool,
    /// The feed serving this subscription when the request joined it.
    pub data_type: i32,
    /// Whether its contract is of a type a gateway marks as an option, whose
    /// snapshot also waits for the option model.
    pub marked: bool,
}

/// A bar request the engine has taken, as its caller's side writes down what
/// the request asked for: ahead of every record answering it. A request the
/// engine refuses leaves what the number's live request asked for as it was.
#[derive(Clone, Debug)]
pub struct HistoricalTaken {
    /// The caller's number for the request.
    pub req_id: u32,
    /// How the caller asked for its bar times to be written.
    pub format_date: i32,
    /// The end the caller named, or empty for the moment of asking.
    pub end_date_time: String,
    /// How far back from that end.
    pub duration: String,
    /// How long its bars are.
    pub bar_size: String,
    /// Whether it keeps its bars up to date once its history is in.
    pub keep_up_to_date: bool,
}

/// What the engine did with an order a caller placed, for the caller's side's
/// record of it.
#[derive(Clone, Debug)]
pub enum OrderBook {
    /// Taken: placed or restated, sent or kept.
    Taken(Box<TakenOrder>),
    /// A placement the engine kept and then withdrew. It never reached the
    /// venue, so nothing is said of it; its record goes.
    Forgotten(u64),
    /// A revision the engine kept and then withdrew. It never reached the
    /// venue, so the record goes back to the terms the venue holds.
    RevisionForgotten(u64),
    /// A revision the venue refused. Restore the order before delivering the
    /// error that carries the venue's reason.
    RevisionRefused(crate::types::CancelReject),
}

/// An answer the dispatcher composes from its own side of the session when it
/// delivers the marker, pushed where the answer stands in the sequence.
///
/// What a call asked the engine is answered from the engine's records; what it
/// asked of this side — the contracts it has named, the orders it placed, the
/// executions it kept — is this side's to state, and it is stated as it stands
/// at the marker's place in the order.
#[derive(Clone, Debug)]
pub enum Answer {
    /// `req_positions`: every holding, then `position_end`.
    Positions,
    /// `req_positions_multi`: every holding under the request, then its end.
    PositionsMulti {
        /// The caller's number.
        req_id: i64,
        /// The account named.
        account: String,
        /// The model named, stated on each holding.
        model_code: String,
    },
    /// `req_account_updates_multi`: the account's figures under the request,
    /// then its end.
    AccountUpdatesMulti {
        /// The caller's number.
        req_id: i64,
        /// The account named.
        account: String,
        /// The model named, stated on each figure.
        model_code: String,
        /// Whether only the ledger and the net liquidation were asked for.
        ledger_and_nlv: bool,
    },
    /// `req_account_updates(true, ..)`: the subscription starts here, with
    /// the first batch and `account_download_end` where the account is stated.
    AccountUpdates {
        /// The account named.
        account: String,
    },
    /// `req_open_orders` and `req_all_open_orders`: every working order, then
    /// `open_order_end`.
    OpenOrders,
    /// `req_completed_orders`: every finished order, then its end.
    CompletedOrders {
        /// Whether only the orders an API placed were asked for.
        api_only: bool,
    },
    /// `req_ids`: `next_valid_id`.
    NextValidId,
    /// `req_executions`: the executions kept, then `exec_details_end`.
    Executions {
        /// The caller's number.
        req_id: i64,
        /// What was asked for.
        filter: api::ExecutionFilter,
    },
}

/// An answer the engine gives at the call, without the venue.
#[derive(Clone, Debug)]
pub enum Reply {
    /// `news_providers`.
    NewsProviders(Vec<NewsProvider>),
    /// `current_time`, in seconds.
    CurrentTime(i64),
    /// `current_time_in_millis`.
    CurrentTimeInMillis(i64),
    /// `soft_dollar_tiers`, under the request.
    SoftDollarTiers(i64, Vec<SoftDollarTier>),
    /// `family_codes`.
    FamilyCodes(Vec<FamilyCode>),
    /// `user_info`, under the request.
    UserInfo(i64, String),
    /// `managed_accounts`.
    ManagedAccounts(String),
    /// `market_rule`, under the rule's number.
    MarketRule(i32, Vec<api::PriceIncrement>),
    /// `smart_components`, under the request.
    SmartComponents(i64, Vec<SmartComponent>),
    /// `display_group_list`, under the request.
    DisplayGroupList(i64, String),
    /// `display_group_updated`, under the request.
    DisplayGroupUpdated(i64, String),
}

/// One record, as a read hands it to a dispatcher.
#[derive(Clone, Debug)]
pub enum Record {
    // ── The session ──
    /// The connection went, and whether that was asked for. Pushed once, as
    /// the connected flag flips.
    ConnectionLost {
        /// A stop asked for, for which nothing is said: its end is `Closed`.
        by_design: bool,
    },
    /// The connection came back after an announced loss.
    ConnectionRestored,
    /// One of the venue's data connections went away or came back.
    VenueData((VenueDataConnection, bool)),
    /// The session's last record.
    Closed,
    // ── Slots ──
    /// A slot is held from here under this generation.
    SlotTaken {
        /// The slot.
        slot: InstrumentId,
        /// The occupancy number it is held under.
        generation: u64,
    },
    /// The occupancy a slot was held under has ended.
    SlotReleased {
        /// The slot.
        slot: InstrumentId,
        /// The occupancy number that ended.
        generation: u64,
    },
    // ── Orders ──
    /// Something said about an order that goes anyway, and the operation on
    /// it that it answers.
    OrderNotice((u64, i32, String, api::OrderOp)),
    /// A fill, with its report and the status stated beside it.
    Fill(FillRecord),
    /// A status change with no fill on the same report.
    OrderUpdate(UpdateRecord),
    /// What a fill cost.
    Charge(api::CommissionAndFeesReport),
    /// An execution the venue restated rather than announced. Boxed, as the
    /// larger records are: every record is moved as often as the largest.
    RestatedExecution(Box<(api::Contract, api::Execution)>),
    /// The venue took an order's outstanding replacement.
    ReplacementTaken(u64),
    /// A refused cancel or modify.
    CancelReject(CancelReject),
    /// Why an order stopped working, or why it was refused, and the
    /// operation on it that it answers.
    OrderInactive((u64, i32, String, api::OrderOp)),
    /// What an order would cost.
    WhatIf(WhatIfResponse),
    // ── Market data ──
    /// A market-data request the engine has taken, and the slot it is served
    /// on: ahead of every record under that slot for it.
    MarketDataTaken(Box<MarketDataTaken>),
    /// A market-data request withdrawn by its cancel: nothing after this is
    /// delivered under its number.
    MarketDataWithdrawn(i64),
    /// What a joined subscription was acknowledged with, for one request.
    TickReqParamsFor((i64, TickReqParams)),
    /// A refusal owed to a request that joined a refused contract.
    SubscriptionFailureFor((i64, String)),
    /// What a subscription was acknowledged with: slot, generation, params.
    TickReqParams((InstrumentId, u64, TickReqParams)),
    /// A trade on a tick-by-tick stream.
    TbtTrade(TbtTrade),
    /// A quote on a tick-by-tick stream.
    TbtQuote(TbtQuote),
    /// A midpoint on a tick-by-tick stream.
    TbtMid(TbtMid),
    /// A book given up on, under its request.
    DepthDrop((u32, String)),
    /// A change to a book.
    DepthUpdate(DepthUpdate),
    /// A headline on a contract: generation, headline.
    TickNews((u64, TickNews)),
    /// An answer to a calculation asked of this client.
    OptionComputation(OptionComputation),
    /// A model tick on an option: the slot's generation, the tick.
    OptionTick((u64, super::OptionTick)),
    /// What the venue said went wrong, belonging to no request.
    VenueError(String),
    /// A subscription the venue could not be asked for: slot, generation,
    /// reason.
    SubscriptionFailure((InstrumentId, u64, String)),
    /// A notice that leaves the subscription running.
    SubscriptionNotice((InstrumentId, u64, crate::error_codes::Refusal)),
    /// The feed the venue accepted for a subscription.
    MarketDataType((InstrumentId, u64, i32)),
    /// A companion request refused: slot, generation, series, reason.
    CompanionRefusal((InstrumentId, u64, u32, String)),
    /// A broadcast notice.
    NewsBulletin(NewsBulletin),
    /// A real-time bar, under its request.
    RealTimeBar((u32, crate::types::RealTimeBar)),
    /// A bar still forming, under the request that keeps its bars up to date.
    HistoricalUpdate(super::SessionBar),
    // ── Reference ──
    /// A bar request the engine has taken.
    HistoricalTaken(Box<HistoricalTaken>),
    /// A refusal or notice stated under a request, with what it is about.
    HistoricalError((api::ErrorOrigin, i32, String)),
    /// Bars answering a request.
    HistoricalData((u32, HistoricalResponse)),
    /// An order this session did not place, paired with the number it is
    /// reached under.
    OrderBound((i64, i64, i64)),
    /// A head timestamp.
    HeadTimestamp((u32, HeadTimestampResponse)),
    /// A contract's details. Boxed, as the larger records are.
    ContractDetails((u32, Box<ContractDefinition>)),
    /// The end of a contract-details answer.
    ContractDetailsEnd(u32),
    /// The exchanges that serve a book, answering an ask.
    DepthExchanges(Vec<DepthMktDataDescription>),
    /// The calendar's event types.
    CalendarMeta((u32, String)),
    /// The calendar's events.
    CalendarEvents((u32, String)),
    /// Matching symbols.
    MatchingSymbols((u32, Vec<SymbolMatch>)),
    /// An option chain: request, underlying, scopes.
    OptionParams((u32, i64, Vec<OptionChainScope>)),
    /// The scanner's parameters.
    ScannerParams(String),
    /// A partition of the advisor's configuration.
    AdvisorConfig((i32, String)),
    /// The end of a replacement of one.
    AdvisorReplaced((i64, String)),
    /// An advisor request refused: a replacement, or the question of a
    /// partition.
    AdvisorRefused((api::ErrorOrigin, i32, String)),
    /// A scan's rows.
    ScannerData((u32, ScannerResult)),
    /// Headlines: request, headlines, whether more are held.
    HistoricalNews((u32, Vec<NewsHeadline>, bool)),
    /// An article.
    NewsArticle((u32, i32, String)),
    /// A fundamentals report.
    FundamentalData((u32, String)),
    /// A histogram.
    HistogramData((u32, Vec<HistogramEntry>)),
    /// Historical ticks: request, ticks, what was shown, whether done.
    HistoricalTicks((u32, HistoricalTickData, String, bool)),
    /// A trading schedule.
    HistoricalSchedule((u32, HistoricalScheduleResponse)),
    // ── At the call ──
    /// A refusal made at a call, or of a value the engine cannot carry.
    Refused((api::ErrorOrigin, i64, String)),
    /// An answer this side composes when it delivers it.
    Answer(Answer),
    /// An answer given at the call.
    Reply(Reply),
    /// A question or a numbered request withdrawn by its cancel, confirmed
    /// where it stands: after every record of the exchange it ends.
    Retired(Retirement),
    /// What the engine did with an order a caller placed.
    OrderBook(OrderBook),
}

/// Whose a record is, for a call that answers and takes only its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Records about an order, by its number.
    Order(i64),
    /// Records answering a request, by its number.
    Request(i64),
}

/// Which records a read takes.
#[derive(Clone, Copy, Debug)]
pub enum Take<'a> {
    /// `process_msgs`: every record but those a call that answers, or a
    /// stream reading by number, is holding under its own number; and the
    /// broadcast notices only where they were asked for.
    Dispatch {
        /// Whether broadcast notices were asked for.
        bulletins: bool,
    },
    /// A call that answers, pumping with a record kept: a whole read, its own
    /// answer in its place, leaving only what a stream holds.
    Whole {
        /// Whether broadcast notices were asked for.
        bulletins: bool,
    },
    /// A call that answers with no record kept: the records under the numbers
    /// it holds, and nothing else.
    Own(&'a [Owner]),
}

impl super::SharedState {
    /// The stamp the next record will take. A read's cut.
    pub fn next_seq(&self) -> u64 {
        self.stamps.cut()
    }

    /// Take a stamp for a record kept outside the session's queues: an answer
    /// the Python surface builds at its call, which holds objects the engine's
    /// thread must not own. Taken under the lock of the queue that keeps it,
    /// so a read's cut is as exact for it as for any record.
    #[doc(hidden)]
    pub fn take_stamp(&self) -> u64 {
        self.stamps.take()
    }

    /// Take the records stamped below `cut` that `take` picks, from every
    /// queue, in stamp order.
    ///
    /// Every record under the cut is in its queue by now: its stamp was taken
    /// inside that queue's lock, which this takes after the cut was read. A
    /// record stamped at or past the cut stays for the next read.
    pub fn take_records(&self, cut: u64, take: Take<'_>) -> Vec<(u64, Record)> {
        let held = |kind: RecordKind, id: i64| self.reference.is_ours(kind, id);
        // Whether a record a reader holds under `kind` and `id` stays where it
        // is. A stream reads its records by number and holds no turn; a call
        // that answers is either this read (with a record kept) or reads by
        // number too.
        let leave = |kind: Option<RecordKind>, owner: Option<Owner>| -> bool {
            let id = match owner {
                Some(Owner::Order(id) | Owner::Request(id)) => id,
                None => return matches!(take, Take::Own(_)),
            };
            match take {
                Take::Dispatch { .. } => match kind {
                    Some(kind) => held(kind, id),
                    None => false,
                },
                Take::Whole { .. } => match kind {
                    Some(kind) => {
                        held(kind, id)
                            && !u32::try_from(id).is_ok_and(super::ReferenceState::is_ask_id)
                    }
                    None => false,
                },
                Take::Own(mine) => !mine.contains(owner.as_ref().unwrap()),
            }
        };
        // What the engine and the venue answer under a request number in the
        // band the calls that answer take, once no call holds it any longer,
        // answers a question already given up on — the end of a lookup whose
        // call returned on its refusal, an answer that came after the wait ran
        // out — and is nobody's.
        let abandoned = |id: i64| {
            u32::try_from(id).is_ok_and(super::ReferenceState::is_ask_id)
                && !self.reference.held_under_any_kind(id)
        };
        let kept_back = |kind: Option<RecordKind>, owner: Option<Owner>| -> Fate {
            match owner {
                Some(Owner::Request(id)) if abandoned(id) => Fate::Drop,
                _ => leave(kind, owner).into(),
            }
        };
        // The refusals queue belongs to every kind of request at once.
        let error_left = |id: u32| -> bool {
            match take {
                Take::Dispatch { .. } => self.reference.held_under_any_kind(i64::from(id)),
                Take::Whole { .. } => self.reference.left_for_its_reader(id),
                Take::Own(mine) => {
                    !mine.contains(&Owner::Request(super::ReferenceState::request_id_reported(id)))
                }
            }
        };
        let error_kept_back = |id: u32| -> Fate {
            if abandoned(i64::from(id)) {
                Fate::Drop
            } else {
                error_left(id).into()
            }
        };
        let bulletins = match take {
            Take::Dispatch { bulletins } | Take::Whole { bulletins } => bulletins,
            Take::Own(_) => false,
        };
        let answer = Some(RecordKind::Answer);
        let order = |id: u64| Some(Owner::Order(id as i64));
        let request = |id: u32| Some(Owner::Request(i64::from(id)));
        let mut out: Vec<(u64, Record)> = Vec::new();

        // The session's own.
        self.session_records.take_below(cut, |_| kept_back(None, None), |r| r, &mut out);
        self.calls.take_below(
            cut,
            |r| match r {
                // A refusal under a number a reader holds is left for that
                // reader, as the venue's refusals are.
                //
                // Nothing here is dropped, under whatever number: a request a
                // program numbered in the band is refused under that number,
                // and a stream `watch` opened there is withdrawn, and a second
                // withdrawal refused, after its number is let go.
                Record::Refused((api::ErrorOrigin::Request { id, .. }, ..)) => {
                    match u32::try_from(*id) {
                        Ok(id) => error_left(id),
                        Err(_) => leave(None, call_owner(r)),
                    }
                }
                _ => leave(None, call_owner(r)),
            },
            |r| r,
            &mut out,
        );

        // Orders.
        let o = &self.orders;
        o.order_notices.take_below(
            cut,
            |(id, ..)| kept_back(answer, order(*id)),
            Record::OrderNotice,
            &mut out,
        );
        o.fills.take_below(
            cut,
            |f| kept_back(None, order(f.fill.order_id)),
            Record::Fill,
            &mut out,
        );
        o.order_updates.take_below(
            cut,
            |u| kept_back(None, order(u.update.order_id)),
            Record::OrderUpdate,
            &mut out,
        );
        o.charges.take_below(cut, |_| kept_back(None, None), Record::Charge, &mut out);
        o.restated_executions.take_below(
            cut,
            |_| kept_back(None, None),
            |restated| Record::RestatedExecution(Box::new(restated)),
            &mut out,
        );
        o.replacements_taken.take_below(
            cut,
            |id| kept_back(None, order(*id)),
            Record::ReplacementTaken,
            &mut out,
        );
        o.cancel_rejects.take_below(
            cut,
            |r| kept_back(None, order(r.order_id)),
            Record::CancelReject,
            &mut out,
        );
        o.order_inactive.take_below(
            cut,
            |(id, ..)| kept_back(answer, order(*id)),
            Record::OrderInactive,
            &mut out,
        );
        o.what_if_responses.take_below(
            cut,
            |w| kept_back(answer, order(w.order_id)),
            Record::WhatIf,
            &mut out,
        );

        // Market data.
        let m = &self.market;
        m.tick_req_params_direct.take_below(
            cut,
            |(id, _)| kept_back(None, Some(Owner::Request(*id))),
            Record::TickReqParamsFor,
            &mut out,
        );
        m.subscription_failures_direct.take_below(
            cut,
            |(id, _)| kept_back(None, Some(Owner::Request(*id))),
            Record::SubscriptionFailureFor,
            &mut out,
        );
        m.tick_req_params.take_below(
            cut,
            |_| kept_back(None, None),
            Record::TickReqParams,
            &mut out,
        );
        m.tbt_trades.take_below(
            cut,
            |t| kept_back(None, Some(Owner::Request(t.req_id))),
            Record::TbtTrade,
            &mut out,
        );
        m.tbt_quotes.take_below(
            cut,
            |t| kept_back(None, Some(Owner::Request(t.req_id))),
            Record::TbtQuote,
            &mut out,
        );
        m.tbt_mids.take_below(
            cut,
            |t| kept_back(None, Some(Owner::Request(t.req_id))),
            Record::TbtMid,
            &mut out,
        );
        m.depth_drops_unsaid.take_below(
            cut,
            |(id, _)| kept_back(Some(RecordKind::Depth), request(*id)),
            Record::DepthDrop,
            &mut out,
        );
        m.depth_updates.take_below(
            cut,
            |u| kept_back(Some(RecordKind::Depth), request(u.req_id)),
            Record::DepthUpdate,
            &mut out,
        );
        m.tick_news.take_below(cut, |_| kept_back(None, None), Record::TickNews, &mut out);
        m.option_computations.take_below(
            cut,
            |c| kept_back(None, c.answers.map(Owner::Request)),
            Record::OptionComputation,
            &mut out,
        );
        m.option_ticks.take_below(cut, |_| kept_back(None, None), Record::OptionTick, &mut out);
        m.venue_errors.take_below(cut, |_| kept_back(None, None), Record::VenueError, &mut out);
        m.subscription_notices.take_below(cut, |_| kept_back(None, None), Record::SubscriptionNotice, &mut out);
        m.market_data_types.take_below(cut, |_| kept_back(None, None), Record::MarketDataType, &mut out);
        m.subscription_failures.take_below(
            cut,
            |_| kept_back(None, None),
            Record::SubscriptionFailure,
            &mut out,
        );
        m.companion_refusals.take_below(
            cut,
            |_| kept_back(None, None),
            Record::CompanionRefusal,
            &mut out,
        );
        m.news_bulletins.take_below(cut, |_| !bulletins, Record::NewsBulletin, &mut out);
        m.real_time_bars.take_below(
            cut,
            |(id, _)| kept_back(Some(RecordKind::Bars), request(*id)),
            Record::RealTimeBar,
            &mut out,
        );
        m.bar_updates.take_below(
            cut,
            |(id, ..)| kept_back(Some(RecordKind::Bars), request(*id)),
            Record::HistoricalUpdate,
            &mut out,
        );

        // Reference.
        let r = &self.reference;
        r.historical_errors.take_below(
            cut,
            |(id, ..)| error_kept_back(*id),
            |(_, code, msg, origin)| Record::HistoricalError((origin, code, msg)),
            &mut out,
        );
        r.historical_taken.take_below(
            cut,
            |taken| kept_back(answer, request(taken.req_id)),
            |taken| Record::HistoricalTaken(Box::new(taken)),
            &mut out,
        );
        r.historical_data.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            Record::HistoricalData,
            &mut out,
        );
        r.orders_bound.take_below(cut, |_| kept_back(None, None), Record::OrderBound, &mut out);
        r.head_timestamps.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            Record::HeadTimestamp,
            &mut out,
        );
        r.contract_details.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            |(id, def)| Record::ContractDetails((id, Box::new(def))),
            &mut out,
        );
        r.contract_details_end.take_below(
            cut,
            |id| kept_back(answer, request(*id)),
            Record::ContractDetailsEnd,
            &mut out,
        );
        r.depth_exchanges_answers.take_below(
            cut,
            |_| kept_back(None, None),
            Record::DepthExchanges,
            &mut out,
        );
        r.calendar_meta_data.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            Record::CalendarMeta,
            &mut out,
        );
        r.calendar_events.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            Record::CalendarEvents,
            &mut out,
        );
        r.matching_symbols.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            Record::MatchingSymbols,
            &mut out,
        );
        r.option_params.take_below(
            cut,
            |(id, ..)| kept_back(answer, request(*id)),
            Record::OptionParams,
            &mut out,
        );
        r.scanner_params.take_below(
            cut,
            |_| kept_back(None, None),
            Record::ScannerParams,
            &mut out,
        );
        r.advisor_config.take_below(
            cut,
            |_| kept_back(None, None),
            Record::AdvisorConfig,
            &mut out,
        );
        r.advisor_replaced.take_below(
            cut,
            |(id, _)| kept_back(None, Some(Owner::Request(*id))),
            Record::AdvisorReplaced,
            &mut out,
        );
        r.advisor_refused.take_below(
            cut,
            |(id, ..)| kept_back(None, Some(Owner::Request(*id))),
            |(_, code, text, origin)| Record::AdvisorRefused((origin, code, text)),
            &mut out,
        );
        r.scanner_data.take_below(
            cut,
            |(id, _)| match kept_back(Some(RecordKind::Scanner), request(*id)) {
                Fate::Take => kept_back(answer, request(*id)),
                fate => fate,
            },
            Record::ScannerData,
            &mut out,
        );
        r.historical_news.take_below(
            cut,
            |(id, ..)| kept_back(answer, request(*id)),
            Record::HistoricalNews,
            &mut out,
        );
        r.news_articles.take_below(
            cut,
            |(id, ..)| kept_back(None, request(*id)),
            Record::NewsArticle,
            &mut out,
        );
        r.fundamental_data.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            Record::FundamentalData,
            &mut out,
        );
        r.histogram_data.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            Record::HistogramData,
            &mut out,
        );
        r.historical_ticks.take_below(
            cut,
            |(id, ..)| kept_back(None, request(*id)),
            Record::HistoricalTicks,
            &mut out,
        );
        r.historical_schedules.take_below(
            cut,
            |(id, _)| kept_back(answer, request(*id)),
            Record::HistoricalSchedule,
            &mut out,
        );

        // Stamps are unique, so this is the order they were taken in.
        out.sort_unstable_by_key(|(seq, _)| *seq);
        out
    }

    /// Push a record made at a call: a refusal, an answer, a reply.
    pub fn push_call_record(&self, record: Record) {
        self.calls.push(record);
    }

    /// Push a refusal made at a call.
    pub fn push_refused(&self, origin: api::ErrorOrigin, code: i64, message: impl Into<String>) {
        self.calls.push(Record::Refused((origin, code, message.into())));
    }

    /// Take the refusals made at calls, leaving everything else queued: the
    /// number each is reported under, its code and its words.
    #[doc(hidden)]
    pub fn drain_refused(&self) -> Vec<(i64, i64, String)> {
        self.calls
            .take_if(|r| matches!(r, Record::Refused(_)))
            .into_iter()
            .filter_map(|r| match r {
                Record::Refused((origin, code, message)) => Some((origin.id(), code, message)),
                _ => None,
            })
            .collect()
    }

    /// The session's last record. Pushed once: whatever pushes it again
    /// finds it already said.
    pub fn push_closed(&self) {
        let _admission = self.admission.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.publish_finished(self.admitted.load(Ordering::Acquire));
        self.admission_closed.store(true, Ordering::Release);
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // The connection is down with it, and nothing says so but this.
        self.link_up.store(false, Ordering::Release);
        self.session_records.push(Record::Closed);
        drop(_admission);
        self.notify();
    }

    /// Whether the session's last record has been pushed.
    pub fn closed_pushed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

/// Whose a record made at a call is.
fn call_owner(record: &Record) -> Option<Owner> {
    match record {
        Record::Refused((origin, ..)) => match *origin {
            api::ErrorOrigin::Request { id, .. } => Some(Owner::Request(id)),
            api::ErrorOrigin::Order { id, .. } => Some(Owner::Order(id)),
            _ => None,
        },
        Record::Answer(Answer::Executions { req_id, .. })
        | Record::Answer(Answer::PositionsMulti { req_id, .. })
        | Record::Answer(Answer::AccountUpdatesMulti { req_id, .. }) => {
            Some(Owner::Request(*req_id))
        }
        Record::Reply(
            Reply::SoftDollarTiers(id, _)
            | Reply::UserInfo(id, _)
            | Reply::SmartComponents(id, _)
            | Reply::DisplayGroupList(id, _)
            | Reply::DisplayGroupUpdated(id, _),
        ) => Some(Owner::Request(*id)),
        // The caller's record of an order is taken by whichever read takes
        // what the venue says about that order, and ahead of it: read apart,
        // a preview's answer or a placement's status was delivered against no
        // record.
        // A market-data request's registration and withdrawal are its own,
        // and taken ahead of what arrives under it.
        Record::MarketDataTaken(taken) => Some(Owner::Request(taken.req_id)),
        Record::MarketDataWithdrawn(req_id) => Some(Owner::Request(*req_id)),
        Record::OrderBook(OrderBook::Taken(taken)) => Some(Owner::Order(taken.order_id as i64)),
        Record::OrderBook(OrderBook::RevisionRefused(reject)) => Some(Owner::Order(reject.order_id as i64)),
        Record::OrderBook(OrderBook::Forgotten(id) | OrderBook::RevisionForgotten(id)) => {
            Some(Owner::Order(*id as i64))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::SharedState;

    fn trade(req_id: i64) -> TbtTrade {
        TbtTrade {
            instrument: 0,
            req_id,
            kind: crate::types::TbtType::Last,
            price: 0,
            size: 0,
            timestamp: 0,
            exchange: String::new(),
            conditions: String::new(),
            past_limit: false,
            unreported: false,
        }
    }

    /// Closing finishes unread admissions and refuses new ones.
    #[test]
    fn closing_finishes_unread_admissions() {
        let shared = SharedState::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        shared.admit(&tx, crate::types::ControlCommand::Ping).unwrap();
        assert_eq!(shared.backlog(), 1);
        shared.push_closed();
        assert_eq!(shared.backlog(), 0);
        assert!(shared.admit(&tx, crate::types::ControlCommand::Ping).is_err());
    }

    /// A record pushed into one queue after a record in another is taken
    /// after it, whichever queue a read happens to look at first.
    #[test]
    fn records_come_out_in_the_order_they_went_in() {
        let shared = SharedState::new();
        shared.market.push_tbt_trade(trade(1));
        shared.push_refused(api::ErrorOrigin::Request { id: 2, ends: true }, 321, "b");
        shared.orders.push_order_notice(3, api::OrderOp::Place, 399, "c".into());
        shared.market.push_tbt_trade(trade(4));
        let cut = shared.next_seq();
        let taken = shared.take_records(cut, Take::Dispatch { bulletins: false });
        let order: Vec<i64> = taken
            .iter()
            .map(|(_, r)| match r {
                Record::TbtTrade(t) => t.req_id,
                Record::Refused((origin, ..)) => origin.id(),
                Record::OrderNotice((id, ..)) => *id as i64,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(order, [1, 2, 3, 4]);
    }

    /// A record stamped at or past the cut is left for the next read.
    #[test]
    fn a_record_past_the_cut_stays_queued() {
        let shared = SharedState::new();
        shared.market.push_tbt_trade(trade(1));
        let cut = shared.next_seq();
        shared.market.push_tbt_trade(trade(2));
        let first = shared.take_records(cut, Take::Dispatch { bulletins: false });
        assert_eq!(first.len(), 1);
        let second = shared.take_records(shared.next_seq(), Take::Dispatch { bulletins: false });
        assert!(matches!(&second[..], [(_, Record::TbtTrade(t))] if t.req_id == 2));
    }

    /// A call that answers with no record kept takes its own and nothing else.
    #[test]
    fn an_answering_call_with_no_record_takes_only_its_own() {
        let shared = SharedState::new();
        shared.market.push_tbt_trade(trade(1));
        shared.push_refused(api::ErrorOrigin::Request { id: 9, ends: true }, 321, "mine");
        let cut = shared.next_seq();
        let mine = [Owner::Request(9)];
        let taken = shared.take_records(cut, Take::Own(&mine));
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert_eq!(shared.market.drain_tbt_trades().len(), 1, "the rest stays for process_msgs");
    }

    /// A read that takes an order's reports takes the order's record with
    /// them, ahead of them.
    #[test]
    fn an_orders_record_is_taken_with_its_reports() {
        let shared = SharedState::new();
        shared.push_call_record(Record::OrderBook(OrderBook::Taken(Box::new(TakenOrder {
            order_id: 7,
            contract: Default::default(),
            order: Default::default(),
            instrument: 0,
            restated: false,
        }))));
        shared.orders.push_order_update(crate::types::OrderUpdate {
            order_id: 7,
            instrument: 0,
            status: crate::types::OrderStatus::Submitted,
            filled_qty: 0.0,
            remaining_qty: 1.0,
            avg_price: 0,
            perm_id: 0,
            parent_id: 0,
            timestamp_ns: 0,
        });
        let cut = shared.next_seq();
        let taken = shared.take_records(cut, Take::Own(&[Owner::Order(7)]));
        assert!(
            matches!(
                taken.as_slice(),
                [(_, Record::OrderBook(OrderBook::Taken(_))), (_, Record::OrderUpdate(_))],
            ),
            "{taken:?}",
        );
    }
}
