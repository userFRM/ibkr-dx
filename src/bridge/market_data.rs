//! What is quoted, and what the venue has said about it.

use super::*;
use super::record::{Queue, Stamps};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use crate::types::*;

/// How many broadcast notices are kept for a caller who has not asked for them
/// yet. The venue broadcasts these unasked and only a subscriber drains them,
/// so without a bound a session that never subscribes keeps every notice of the
/// day for the life of the process.
pub const NEWS_BULLETIN_LIMIT: usize = 1000;

/// How much of a stream is kept for a caller who has stopped reading it.
///
/// The same reasoning as the bulletins above, and it applies harder: these
/// arrive at market rate rather than a few times an hour, and this library
/// documents a way of reading them that never pumps the callback loop at all.
/// Unbounded, a book or a tick-by-tick stream grows the process until it dies.
/// Bounded, a caller that stopped reading loses the oldest of what it was not
/// reading, which is the lesser of the two.
pub const STREAM_BACKLOG_LIMIT: usize = 100_000;

/// What a subscription's acknowledgement states for the request, as a
/// gateway hands it on `tickReqParams`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TickReqParams {
    /// The increment prices move in.
    pub min_tick: f64,
    /// The exchange the best bid and offer are taken from, with the contract's
    /// security type appended as four hex digits where the name is four
    /// characters or fewer, as a gateway writes it.
    pub bbo_exchange: String,
    /// What the venue says this request may be given: 0 nothing stated, 1 no
    /// top of book, 2 snapshots, 3 real-time top of book, 4 snapshots not
    /// available through the API.
    pub snapshot_permissions: i64,
}

/// What a gateway's option model publishes for an option on its own tick, 13
/// (83 on a delayed feed).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OptionTick {
    /// The option.
    pub instrument: InstrumentId,
    /// The implied volatility, delta, option price, dividend present value,
    /// gamma, vega, theta and underlying price, in the callback's order, with
    /// `f64::MAX` for a figure not stated.
    pub figures: [f64; 8],
    /// Whether the volatility behind it was worked from prices, which is what
    /// `tickAttrib` says.
    pub price_based: bool,
}

pub(crate) const PRICING_BID: u8 = 1;
pub(crate) const PRICING_ASK: u8 = 2;
pub(crate) const PRICING_LAST: u8 = 4;
pub(crate) const PRICING_CLOSE: u8 = 8;
pub(crate) const PRICING_BID_SIZE: u8 = 16;
pub(crate) const PRICING_ASK_SIZE: u8 = 32;
pub(crate) const PRICING_LAST_SIZE: u8 = 64;
pub(crate) const PRICING_STATE: u8 = 128;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PricingQuote {
    pub bid: Option<Price>,
    pub ask: Option<Price>,
    pub last: Option<Price>,
    pub close: Option<Price>,
    pub bid_size: Option<Qty>,
    pub ask_size: Option<Qty>,
    pub last_size: Option<Qty>,
    pub state_mask: i64,
    pub close_attributes: i32,
    pub close_date: Option<i32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PricingQuoteViews {
    pub mode: i32,
    pub confirmed: bool,
    pub mark_price: Option<f64>,
    pub mark_rejected: bool,
    /// Real-time, delayed, frozen, and delayed-frozen records.
    pub records: [PricingQuote; 4],
    pub vwap: Option<f64>,
    pub auction_price: Option<f64>,
    pub auction_volume: i32,
    pub auction_borrow: Option<f64>,
    pub auction_borrow_volume: i32,
    pub auction_lend: Option<f64>,
    pub auction_lend_volume: i32,
}

/// This machine's clock, in unix milliseconds.
fn local_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// A calculation asked for before the venue had stated a model for its
/// contract, kept until it does.
///
/// The calculation is answered from the venue's model for the contract, and a
/// contract nobody is watching has no model stated for it. Asking opens the
/// watch; the engine answers where it writes the model the watch brings, so
/// the answer stands right behind the model in the session's order.
#[derive(Clone, Debug)]
pub struct KeptCalculation {
    /// The contract it is on.
    pub contract: crate::types::model::Contract,
    /// The slot the watch holds the contract in.
    pub slot: crate::types::InstrumentId,
    /// Whether it inverts a price or prices a volatility.
    pub wants_volatility: bool,
    /// The option price or volatility the caller supplied.
    pub option_price: f64,
    /// The underlying price the caller supplied.
    pub under_price: f64,
    /// Whether it has been answered.
    ///
    /// Answered questions are kept rather than dropped, because the watch
    /// opened to obtain the model is withdrawn where the caller withdraws the
    /// calculation — and a question that is gone cannot be withdrawn.
    pub answered: bool,
}

/// What each contract's numbered-figure tables hold: the slot, the series and
/// which of the two tables, against the figures the venue numbered in it.
type NumberedFiguresHeld =
    std::collections::HashMap<(crate::types::InstrumentId, u32, bool), Vec<(i32, f64)>>;

/// What each contract's stated rows hold: the slot and the series, against
/// the three figures of each row the venue stated.
type StatedRowsHeld =
    std::collections::HashMap<(crate::types::InstrumentId, u32), Vec<(f64, f64, f64)>>;

/// What each contract's paired-figure runs hold: the slot and the series,
/// against the pairs the venue stated in it.
type PairedFiguresHeld =
    std::collections::HashMap<(crate::types::InstrumentId, u32), Vec<(f64, f64)>>;

/// Lock-free quotes, TBT streams, real-time bars, depth updates, and news ticks.
pub struct MarketDataState {
    /// The session's stamps, which a quote's write marks.
    stamps: Stamps,
    quotes: super::slot_table::SlotTable<SeqQuote>,
    /// Which occupancy each slot is held under, as the engine named it when
    /// the slot was taken: written into the slot's quote with every quote, and
    /// onto every record queued under the slot, so a reader can tell the
    /// contract that left a slot from the one that took it.
    generations: super::slot_table::SlotTable<AtomicU64>,
    pricing_quotes: Mutex<std::collections::HashMap<InstrumentId, PricingQuoteViews>>,
    attached_quote_instruments: Mutex<std::collections::HashMap<(i64, String), InstrumentId>>,
    /// InstrumentId counter — set by hot loop on RegisterInstrument.
    instrument_count: AtomicU64,
    /// Slots that have been given back, for the surfaces to forget.
    ///
    /// A slot is handed to the next contract that needs one, and a surface
    /// that had cached the old contract's slot went on naming it: the order it
    /// placed was recorded against whatever now holds that slot, and the fill
    /// moved the wrong position.
    ///
    /// Named by the slot rather than by the contract on it, because a contract
    /// the venue has not named yet holds a slot under no id at all — and a
    /// release that could only say "contract nought" named nothing a surface
    /// could act on, for exactly the contracts a caller states by description.
    /// Each with the point in the order of what the client has asked for at
    /// which it was given back.
    ///
    /// A slot number alone says nothing about which occupancy ended: the slot
    /// goes to the next contract that needs one, and a release drained after
    /// that forgot the records of a contract that had just been given it —
    /// live on the wire, and reachable from nothing here.
    released_slots: Mutex<Vec<(crate::types::InstrumentId, u64)>>,
    pub(super) tbt_trades: Queue<TbtTrade>,
    pub(super) tbt_quotes: Queue<TbtQuote>,
    /// The point between the two, each time it moved.
    pub(super) tbt_mids: Queue<TbtMid>,
    pub(super) real_time_bars: Queue<(u32, RealTimeBar)>,
    pub(super) depth_updates: Queue<DepthUpdate>,
    /// Books that were dropped for running away unread, and have not been
    /// asked for again.
    ///
    /// A book only means anything whole. Once entries are gone, everything
    /// after them describes positions in a book that no longer exists — so
    /// nothing further is kept for one until the caller withdraws it and asks
    /// again. Handing back what arrives next would be handing back a book that
    /// reads correct and is not.
    depth_dropped: Mutex<std::collections::HashSet<u32>>,
    /// The books given up on that the caller has not been told about yet, and
    /// what happened, under the request each was asked for.
    ///
    /// A dropped book is the one failure here a caller cannot see: the
    /// subscription reads as healthy and the entries simply stop arriving,
    /// which is what a quiet market looks like. Said once per drop.
    pub(super) depth_drops_unsaid: Queue<(u32, String)>,
    /// Each headline with the occupancy of its slot when it arrived.
    pub(super) tick_news: Queue<(u64, TickNews)>,
    pub(super) news_bulletins: Queue<NewsBulletin>,
    /// Each answer to a calculation asked of this client.
    pub(super) option_computations: Queue<crate::types::OptionComputation>,
    /// Each model tick with the occupancy of its slot when it was pushed.
    pub(super) option_ticks: Queue<(u64, OptionTick)>,
    /// Calculations asked for before the venue had stated a model for their
    /// contract, by the number they were asked under: solved by the engine
    /// where it writes the model, and answered right after it.
    kept_calculations: Mutex<std::collections::HashMap<i64, KeptCalculation>>,
    calculations_waiting: std::sync::atomic::AtomicUsize,
    /// The last statement the venue made of its own model, per contract, kept
    /// rather than only handed over.
    last_option_model: Mutex<std::collections::HashMap<crate::types::InstrumentId, crate::types::OptionComputation>>,
    /// Subscriptions the venue was never able to be asked for, the occupancy
    /// of the slot, and why.
    pub(super) subscription_notices: Queue<(InstrumentId, u64, crate::error_codes::Refusal)>,
    pub(super) market_data_types: Queue<(InstrumentId, u64, i32)>,
    subscription_data_types: Mutex<std::collections::HashMap<InstrumentId, std::sync::Arc<std::sync::atomic::AtomicI32>>>,
    pub(super) subscription_failures: Queue<(crate::types::InstrumentId, u64, String)>,
    /// Refusals of the requests that ride beside a quote: the contract, which
    /// companion was refused, and the venue's own reason.
    ///
    /// Held apart from the failures above, which mean the quote itself was
    /// refused. A companion is refused on its own and the quote goes on
    /// ticking, so a caller told on that channel withdrew a subscription that
    /// was working.
    pub(super) companion_refusals: Queue<(crate::types::InstrumentId, u64, u32, String)>,
    /// What each subscription was acknowledged with, for whoever watches the
    /// contract, with the occupancy of the slot it was acknowledged under.
    pub(super) tick_req_params: Queue<(crate::types::InstrumentId, u64, TickReqParams)>,
    /// What the last acknowledgement for an instrument stated, so a request
    /// that follows an existing subscription can be told it too: the venue
    /// sends one tickReqParams per reqMktData, and a follower asked for none.
    last_tick_req_params: Mutex<std::collections::HashMap<crate::types::InstrumentId, TickReqParams>>,
    /// Why the venue refused a contract's subscription, kept for whoever asks
    /// for it next. The failure itself is drained once and told to whoever
    /// held it then; a request that joins the same contract afterwards was
    /// told nothing and received nothing, because the subscription it joined
    /// had already been refused.
    last_subscription_failure: Mutex<std::collections::HashMap<crate::types::InstrumentId, String>>,
    /// A refusal owed to one request that joined a contract already refused.
    pub(super) subscription_failures_direct: Queue<(i64, String)>,
    /// Why the quote feed is done for the rest of this session, where it is.
    ///
    /// Set once the engine gives up on the feed, and never cleared: the feed
    /// is not coming back within this session, which is what giving up on it
    /// means. Read where a subscription is asked for, so a request that cannot
    /// be served is refused rather than acknowledged into a table nothing will
    /// replay.
    market_data_over: Mutex<Option<&'static str>>,
    /// tickReqParams owed to a single request that followed a live
    /// subscription, delivered to that request alone rather than fanned.
    pub(super) tick_req_params_direct: Queue<(i64, TickReqParams)>,
    /// What the venue has said went wrong, in its own words.
    pub(super) venue_errors: Queue<String>,
    series_ticks: Mutex<std::collections::HashMap<crate::types::InstrumentId, Vec<SeriesTick>>>,
    quote_attribute_masks: Mutex<std::collections::HashMap<crate::types::InstrumentId, (i64, i64)>>,
    /// What the venue last said about a contract itself, beside its prices.
    contract_figures:
        Mutex<std::collections::HashMap<crate::types::InstrumentId, crate::types::ContractFigures>>,
    /// The figures each series has stated for a contract, in the order the
    /// series states them.
    ///
    /// Kept by slot and by series. By slot because a figure belongs to the
    /// contract that was in it, and by series because the venue runs dozens of
    /// them side by side and one record per contract would have each series
    /// overwrite the last.
    stated_figures:
        Mutex<std::collections::HashMap<(crate::types::InstrumentId, u32), Vec<f64>>>,
    /// The numbered figures each series has stated for a contract: the whole
    /// ones under `false` and the fractional ones under `true`, because the
    /// venue numbers the two tables separately and a figure is its number and
    /// the table it stood in.
    numbered_figures: Mutex<NumberedFiguresHeld>,
    /// The venue's option model for a contract as it closed, kept apart from
    /// the standing one.
    closing_option_model: Mutex<
        std::collections::HashMap<crate::types::InstrumentId, crate::types::OptionComputation>,
    >,
    /// The strategies a spread scan has stated for an underlying.
    scanned_strategies: Mutex<
        std::collections::HashMap<
            crate::types::InstrumentId, Vec<crate::types::ScannedStrategy>,
        >,
    >,
    /// The rows of three figures each series has stated for a contract.
    stated_rows: Mutex<StatedRowsHeld>,
    /// The venue's answer to each contract's chargeable snapshot, not yet read.
    snapshot_answers: Mutex<std::collections::HashMap<crate::types::InstrumentId, Vec<SeriesTick>>>,
    /// What each of the option model's chain series last stated for an
    /// underlying.
    chain_model_parameters: Mutex<
        std::collections::HashMap<
            (crate::types::InstrumentId, u32), Vec<crate::types::ChainModelParameters>,
        >,
    >,
    /// The runs of paired figures each series has stated for a contract.
    paired_figures: Mutex<PairedFiguresHeld>,
    /// Which contracts the venue is restricting short sales in.
    ///
    /// Stated on the same record as the halt, and kept here rather than on the
    /// quote: that record is two cache lines exactly, on every slot in the
    /// table, and one more field costs a third.
    short_sale_restricted: Mutex<std::collections::HashSet<crate::types::InstrumentId>>,
    /// How far the venue's clock runs from this machine's, in milliseconds.
    ///
    /// Nothing here ever asks the venue what time it is — this wire carries no
    /// such request. A caller asking for the venue's clock is answered from
    /// this machine's, shifted by this: what the venue has stated about its
    /// own, on the logon it stamps and in the clock it pushes unasked
    /// afterwards. Zero until it states one, so a session that has heard
    /// nothing answers this machine's clock unshifted — the two are not known
    /// to differ, and an answer is owed either way.
    clock_skew_millis: AtomicI64,
    /// Messages the venue sent that nothing here reads, named once each:
    /// which connection, and what it was. Empty is the claim that this client
    /// reads everything this venue sends it, and the only way to check it.
    /// Connection, what the row is about, and the latest thing said about it.
    unread_wire: Mutex<Vec<(&'static str, String, String)>>,
}

impl MarketDataState {
    /// An empty one, stamping from its own counter.
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::stamping(&Stamps::default())
    }

    /// An empty one, stamping from the session's counter.
    pub(super) fn stamping(stamps: &Stamps) -> Self {
        Self {
            stamps: stamps.clone(),
            quotes: super::slot_table::SlotTable::new(SeqQuote::new),
            generations: super::slot_table::SlotTable::new(|| AtomicU64::new(0)),
            instrument_count: AtomicU64::new(0),
            released_slots: Mutex::new(Vec::new()),
            tbt_trades: Queue::with_capacity(stamps, 256),
            tbt_quotes: Queue::with_capacity(stamps, 256),
            tbt_mids: Queue::with_capacity(stamps, 256),
            real_time_bars: Queue::with_capacity(stamps, 64),
            depth_updates: Queue::with_capacity(stamps, 64),
            depth_dropped: Mutex::new(std::collections::HashSet::new()),
            depth_drops_unsaid: Queue::new(stamps),
            tick_news: Queue::with_capacity(stamps, 32),
            news_bulletins: Queue::with_capacity(stamps, 16),
            option_computations: Queue::with_capacity(stamps, 16),
            option_ticks: Queue::with_capacity(stamps, 16),
            last_option_model: Mutex::new(std::collections::HashMap::new()),
            kept_calculations: Mutex::new(std::collections::HashMap::new()),
            calculations_waiting: std::sync::atomic::AtomicUsize::new(0),
            subscription_failures: Queue::new(stamps),
            subscription_notices: Queue::new(stamps),
            market_data_types: Queue::new(stamps),
            subscription_data_types: Mutex::new(std::collections::HashMap::new()),
            companion_refusals: Queue::new(stamps),
            tick_req_params: Queue::new(stamps),
            last_tick_req_params: Mutex::new(std::collections::HashMap::new()),
            last_subscription_failure: Mutex::new(std::collections::HashMap::new()),
            subscription_failures_direct: Queue::new(stamps),
            market_data_over: Mutex::new(None),
            tick_req_params_direct: Queue::new(stamps),
            venue_errors: Queue::new(stamps),
            series_ticks: Mutex::new(std::collections::HashMap::new()),
            quote_attribute_masks: Mutex::new(std::collections::HashMap::new()),
            contract_figures: Mutex::new(std::collections::HashMap::new()),
            stated_figures: Mutex::new(std::collections::HashMap::new()),
            numbered_figures: Mutex::new(std::collections::HashMap::new()),
            paired_figures: Mutex::new(std::collections::HashMap::new()),
            stated_rows: Mutex::new(std::collections::HashMap::new()),
            chain_model_parameters: Mutex::new(std::collections::HashMap::new()),
            snapshot_answers: Mutex::new(std::collections::HashMap::new()),
            pricing_quotes: Mutex::new(std::collections::HashMap::new()),
            attached_quote_instruments: Mutex::new(std::collections::HashMap::new()),
            scanned_strategies: Mutex::new(std::collections::HashMap::new()),
            closing_option_model: Mutex::new(std::collections::HashMap::new()),
            short_sale_restricted: Mutex::new(std::collections::HashSet::new()),
            clock_skew_millis: AtomicI64::new(0),
            unread_wire: Mutex::new(Vec::new()),
        }
    }

    /// Read a quote snapshot (lock-free via SeqLock). A slot no quote has
    /// reached yet reads as the empty quote.
    #[inline]
    pub fn quote(&self, id: InstrumentId) -> Quote {
        self.try_quote(id).unwrap_or_default()
    }

    /// A quote snapshot, or `None` for an id past every slot the table holds:
    /// a slot this session never handed out is a caller's mistake, not a quote
    /// of nothing.
    #[inline]
    pub fn try_quote(&self, id: InstrumentId) -> Option<Quote> {
        self.quotes.get(id).map(SeqQuote::read)
    }

    /// A quote snapshot and the occupancy of the slot it was written under,
    /// read together: a reader can tell a quote of the contract that left a
    /// slot from one of the contract that took it.
    #[inline]
    pub fn quote_with_generation(&self, id: InstrumentId) -> (Quote, u64) {
        self.quotes.get(id).map(SeqQuote::read_with_generation).unwrap_or_default()
    }

    /// The occupancy a slot is held under now, as the engine named it.
    #[inline]
    pub fn generation_of(&self, id: InstrumentId) -> u64 {
        self.generations.get(id).map_or(0, |g| g.load(Ordering::Acquire))
    }

    /// Name the occupancy a slot is held under. Engine side, where the slot is
    /// taken or changes hands: the slot's quote is written again under the new
    /// name, so a reader is not left holding a quote named for an occupancy
    /// that has gone until the next tick arrives.
    #[doc(hidden)]
    pub fn set_generation(&self, id: InstrumentId, generation: u64) {
        let held = self.generations.get_or_grow(id);
        if held.swap(generation, Ordering::AcqRel) == generation {
            return;
        }
        let slot = self.quotes.get_or_grow(id);
        slot.write(&slot.read(), generation);
    }

    /// Say that a slot has been given back, and what the client had asked for
    /// up to then.
    #[doc(hidden)] pub fn note_released_slot(
        &self, instrument: crate::types::InstrumentId, released_at: u64,
    ) {
        self.released_slots.lock().unwrap().push((instrument, released_at));
        self.last_tick_req_params.lock().unwrap().remove(&instrument);
        self.last_subscription_failure.lock().unwrap().remove(&instrument);
        // And what is still queued under it, not only what is cached. It names
        // a slot rather than a contract, so the next contract to take the slot
        // is who it reaches: an increment acknowledged for the contract that
        // left arrives as the new one's. A reader stalled in a callback is all
        // it takes for the release to land in between.
        //
        // A move is not among these. It stands ahead of the release in the
        // session's order, and a reader moves the callers before it reads the
        // slot as given back.
        self.tick_req_params.retain(|(at, ..)| *at != instrument);
        self.subscription_notices.retain(|(at, ..)| *at != instrument);
        self.market_data_types.retain(|(at, ..)| *at != instrument);
        self.subscription_data_types.lock().unwrap().remove(&instrument);
        // And the two streams that carry a slot of their own. A headline is
        // about the contract that was named when it arrived, and a model was
        // solved against that contract's volatility and price: delivered after
        // the release, both read as the next occupant's. The model has a cache
        // beside it that is already dropped with the slot, and the queue in
        // front of that cache was not — so the stale answer was gone from the
        // lookup and still on its way to the caller.
        //
        // An account-wide notice is not among these. It names no contract, so
        // no slot can carry it to the wrong one. Nor is an answer to a
        // calculation, which belongs to the question that asked it.
        self.tick_news.retain(|(_, n)| n.instrument != instrument);
        self.option_ticks.retain(|(_, t)| t.instrument != instrument);
    }

    /// The slots given back since this was last asked.
    pub fn take_released_slots(&self) -> Vec<(crate::types::InstrumentId, u64)> {
        std::mem::take(&mut *self.released_slots.lock().unwrap())
    }

    /// Drop a subscription failure still waiting under a slot that has gone
    /// back to the table.
    ///
    /// A failure is resolved to a request through the slot it names, and the
    /// slot's next occupant has its own requests: left queued, the reason the
    /// last contract could not be subscribed was reported to whoever is
    /// watching this one.
    #[doc(hidden)] pub fn forget_subscription_failures(&self, id: crate::types::InstrumentId) {
        // A refusal belongs to the contract that was in the slot, not to the
        // slot: left behind, the next contract to take it is answered with the
        // last one's refusal. One still queued is not dropped: it names the
        // occupancy it was about, stands ahead of the release in the session's
        // order, and is the only thing its caller will be told.
        self.last_subscription_failure.lock().unwrap().remove(&id);
    }

    /// Drop the model last published for a slot, because the slot has gone
    /// back to the table.
    ///
    /// Kept by slot rather than by contract, so nothing else can drop it: the
    /// next contract handed this slot was solved against the previous one's
    /// volatility, price and dividend, and the answer came back finite and
    /// wrong.
    #[doc(hidden)] pub fn forget_option_model(&self, instrument: crate::types::InstrumentId) {
        self.last_option_model.lock().unwrap().remove(&instrument);
        // These belong to the contract that was in the slot, not to the slot.
        self.contract_figures.lock().unwrap().remove(&instrument);
        self.stated_figures.lock().unwrap().retain(|(at, _), _| *at != instrument);
        self.numbered_figures.lock().unwrap().retain(|(at, ..), _| *at != instrument);
        self.paired_figures.lock().unwrap().retain(|(at, _), _| *at != instrument);
        self.stated_rows.lock().unwrap().retain(|(at, _), _| *at != instrument);
        self.chain_model_parameters.lock().unwrap().retain(|(at, _), _| *at != instrument);
        self.scanned_strategies.lock().unwrap().remove(&instrument);
        self.closing_option_model.lock().unwrap().remove(&instrument);
        // A restriction belongs to the contract that was in the slot, not to
        // the slot: left behind, the next contract to take it reads as
        // restricted on the strength of the last one.
        self.short_sale_restricted.lock().unwrap().remove(&instrument);
    }

    /// Number of registered instruments.
    pub fn instrument_count(&self) -> u32 {
        self.instrument_count.load(Ordering::Relaxed) as u32
    }

    /// Take every tbt trades waiting, leaving none.
    pub fn drain_tbt_trades(&self) -> Vec<TbtTrade> {
        self.tbt_trades.drain()
    }

    /// Take every tbt quotes waiting, leaving none.
    pub fn drain_tbt_quotes(&self) -> Vec<TbtQuote> {
        self.tbt_quotes.drain()
    }

    /// Take every midpoint waiting, leaving none.
    pub fn drain_tbt_mids(&self) -> Vec<TbtMid> {
        self.tbt_mids.drain()
    }

    /// Take every real time bars waiting, leaving none.
    pub fn drain_real_time_bars(&self) -> Vec<(u32, RealTimeBar)> {
        self.real_time_bars.drain()
    }

    /// Take the bars a dispatch loop should deliver, leaving behind those a
    /// stream is going to read by id.
    ///
    /// A stream cannot hold the session's turn — it outlives any one read of
    /// it — so its records are left where it will find them, the way the
    /// answering calls' own are. `mine` says which ids this session is reading
    /// for itself; see `ReferenceState::is_ours`.
    pub fn drain_real_time_bars_for_dispatch(
        &self, mine: impl Fn(u32) -> bool,
    ) -> Vec<(u32, RealTimeBar)> {
        self.real_time_bars.take_if(|e| !mine(e.0))
    }

    /// Bars answering one request, leaving other requests' alone.
    pub fn take_real_time_bars_for(&self, req_id: u32) -> Vec<RealTimeBar> {
        self.real_time_bars.take_if(|b| b.0 == req_id).into_iter().map(|b| b.1).collect()
    }

    /// Book changes answering one request.
    pub fn take_depth_updates_for(&self, req_id: u32) -> Vec<DepthUpdate> {
        self.depth_updates.take_if(|u| u.req_id == req_id)
    }

    /// Take every depth updates waiting, leaving none.
    pub fn drain_depth_updates(&self) -> Vec<DepthUpdate> {
        self.depth_updates.drain()
    }



    /// Take every tick news waiting, leaving none.
    pub fn drain_tick_news(&self) -> Vec<TickNews> {
        self.tick_news.drain().into_iter().map(|(_, n)| n).collect()
    }

    /// Take every news bulletins waiting, leaving none.
    pub fn drain_news_bulletins(&self) -> Vec<NewsBulletin> {
        self.news_bulletins.drain()
    }

    /// Take every option computations waiting, leaving none.
    pub fn drain_option_computations(&self) -> Vec<crate::types::OptionComputation> {
        self.option_computations.drain()
    }

    /// Everything the venue has sent this session that nothing reads.
    pub fn unread_wire(&self) -> Vec<(&'static str, String)> {
        self.unread_wire.lock().unwrap().iter()
            .map(|(connection, _, what)| (*connection, what.clone()))
            .collect()
    }

    #[doc(hidden)] pub fn note_unread_wire(&self, connection: &'static str, what: String) {
        // The sentence is its own key, which is what this has always done: one
        // row per distinct thing said.
        self.note_unread_wire_under(connection, what.clone(), what);
    }

    /// The same, where the thing being recorded carries a reading that changes.
    ///
    /// Keyed on what it is about rather than on the whole sentence, and the
    /// latest reading replaces the last. Deduplicating on the sentence, a
    /// record that names a figure grows a row every time that figure moves —
    /// which for an account held in another currency is every time the rate
    /// does. This is a vector scanned linearly on the trading loop, so that is
    /// unbounded memory and quadratic work, from ordinary traffic and no
    /// malformed input at all.
    #[doc(hidden)] pub fn note_unread_wire_under(
        &self, connection: &'static str, key: String, what: String,
    ) {
        let mut seen = self.unread_wire.lock().unwrap();
        match seen.iter().position(|(c, k, _)| *c == connection && *k == key) {
            Some(at) => seen[at].2 = what,
            None => seen.push((connection, key, what)),
        }
    }

    /// Learn the venue's clock from a time it stated in its own stamp form.
    ///
    /// A stamp nothing can read leaves the skew where it was: the last thing
    /// the venue said about its clock is a better answer than no answer.
    pub fn note_venue_time(&self, stamped: &str) {
        if let Some(millis) = crate::protocol::datetime::ib_datetime_to_unix_millis(stamped) {
            self.note_venue_millis(millis);
        }
    }

    /// The same, where the venue states its clock as a number of its own
    /// rather than as a stamp on something else.
    pub fn note_venue_millis(&self, venue_millis: i64) {
        // Saturating for the same reason the conversion above is: the
        // difference between a stated clock at the end of the range and this
        // machine's is not representable, and wrapping it puts the session on
        // a clock neither side named.
        self.clock_skew_millis.store(venue_millis.saturating_sub(local_millis()), Ordering::Relaxed);
        self.stamps.note_written();
    }

    /// What the venue's clock reads now: this machine's, shifted by what the
    /// venue has stated about the difference.
    ///
    /// Read rather than remembered, so the answer keeps moving on a connection
    /// that has gone quiet. Held as the last stamp seen, it stood still for as
    /// long as the venue said nothing, and a caller reading it twice a minute
    /// apart was told the same instant twice.
    pub fn venue_time_millis(&self) -> i64 {
        local_millis().saturating_add(self.clock_skew_millis.load(Ordering::Relaxed))
    }

    /// Take every venue errors waiting, leaving none.
    pub fn drain_venue_errors(&self) -> Vec<String> {
        self.venue_errors.drain()
    }

    #[doc(hidden)] pub fn push_venue_error(&self, text: String) {
        self.venue_errors.push(text);
    }

    /// Take every subscription failures waiting, leaving none.
    pub fn drain_subscription_failures(&self) -> Vec<(crate::types::InstrumentId, String)> {
        self.subscription_failures.drain().into_iter().map(|(at, _, why)| (at, why)).collect()
    }

    /// Take every companion refusal waiting, leaving none.
    ///
    /// The venue names the request it is refusing and says why — the one
    /// refusal channel on this wire that names its request. Logged and dropped,
    /// a caller that asked for the option model on a class the venue has no
    /// model for watched an acknowledged subscription that could never produce
    /// a computation, and was told nothing while the venue had said why at
    /// once.
    pub fn drain_companion_refusals(&self) -> Vec<(crate::types::InstrumentId, u32, String)> {
        self.companion_refusals.drain().into_iter().map(|(at, _, kind, why)| (at, kind, why)).collect()
    }

    /// What a subscription was acknowledged with, kept for whoever watches
    /// the contract. Engine side.
    #[doc(hidden)] pub fn push_tick_req_params(&self, instrument: crate::types::InstrumentId, params: TickReqParams) {
        // A follower joining after dispatch takes the acknowledgement reads
        // this cache, so it is ready before the acknowledgement can be read.
        self.last_tick_req_params.lock().unwrap().insert(instrument, params.clone());
        self.tick_req_params.push((instrument, self.generation_of(instrument), params));
    }

    /// What a follower should be told, if the subscription it follows was
    /// already acknowledged. `None` before that — the pending tickReqParams
    /// fans out to the follower when it arrives.
    pub fn tick_req_params_for_follower(&self, instrument: crate::types::InstrumentId) -> Option<TickReqParams> {
        self.last_tick_req_params.lock().unwrap().get(&instrument).cloned()
    }

    /// tickReqParams owed to one request that followed a live subscription.
    #[doc(hidden)] pub fn push_tick_req_params_for(&self, req_id: i64, params: TickReqParams) {
        self.tick_req_params_direct.push((req_id, params));
    }

    /// Take those, in the order they came. Client side.
    pub fn drain_tick_req_params_direct(&self) -> Vec<(i64, TickReqParams)> {
        self.tick_req_params_direct.drain()
    }

    /// Take what the subscriptions were acknowledged with, in the order it
    /// came. Client side.
    pub fn drain_tick_req_params(&self) -> Vec<(crate::types::InstrumentId, TickReqParams)> {
        self.tick_req_params.drain().into_iter().map(|(at, _, p)| (at, p)).collect()
    }



    // ── Hot-loop-side writers ──

    /// Write a slot's quote, under the occupancy the slot is held under.
    #[doc(hidden)]
    pub fn push_quote(&self, id: InstrumentId, quote: &Quote) {
        let generation = self.generation_of(id);
        self.quotes.get_or_grow(id).write(quote, generation);
        self.stamps.note_written();
    }

    pub(crate) fn push_pricing_quote(
        &self, id: InstrumentId, mode: i32, quote: &Quote, present: u8, state_mask: i64,
    ) {
        let Ok(index) = usize::try_from(mode) else { return };
        if index >= 4 { return; }
        let mut held = self.pricing_quotes.lock().unwrap();
        let views = held.entry(id).or_default();
        views.mode = mode;
        let record = &mut views.records[index];
        for (flag, destination, value) in [
            (PRICING_BID, &mut record.bid, quote.bid),
            (PRICING_ASK, &mut record.ask, quote.ask),
            (PRICING_LAST, &mut record.last, quote.last),
            (PRICING_CLOSE, &mut record.close, quote.close),
        ] {
            if present & flag != 0 { *destination = Some(value); }
        }
        for (flag, destination, value) in [
            (PRICING_BID_SIZE, &mut record.bid_size, quote.bid_size),
            (PRICING_ASK_SIZE, &mut record.ask_size, quote.ask_size),
            (PRICING_LAST_SIZE, &mut record.last_size, quote.last_size),
        ] {
            if present & flag != 0 { *destination = Some(value); }
        }
        if present & PRICING_STATE != 0 { record.state_mask = state_mask; }
    }

    pub(crate) fn pricing_quote_views(&self, id: InstrumentId) -> PricingQuoteViews {
        self.pricing_quotes.lock().unwrap().get(&id).copied().unwrap_or_default()
    }

    pub(crate) fn attached_quote_instrument(&self, con_id: i64, exchange: &str) -> Option<InstrumentId> {
        self.attached_quote_instruments.lock().unwrap().get(&(con_id, exchange.into())).copied()
    }

    pub(crate) fn note_attached_quote_instrument(&self, con_id: i64, exchange: &str, instrument: InstrumentId) {
        self.attached_quote_instruments.lock().unwrap().insert((con_id, exchange.into()), instrument);
    }

    pub(crate) fn note_pricing_subscription(&self, id: InstrumentId, mode: i32, confirmed: bool) {
        let mut held = self.pricing_quotes.lock().unwrap();
        let views = held.entry(id).or_default();
        views.mode = mode;
        views.confirmed = confirmed;
    }

    pub(crate) fn note_pricing_mark_rejected(&self, id: InstrumentId, rejected: bool) {
        self.pricing_quotes.lock().unwrap().entry(id).or_default().mark_rejected = rejected;
    }

    pub(crate) fn note_pricing_mark(&self, id: InstrumentId, price: Option<f64>) {
        self.pricing_quotes.lock().unwrap().entry(id).or_default().mark_price = price;
    }

    pub(crate) fn note_pricing_close_metadata(
        &self, id: InstrumentId, mode: i32, attributes: Option<i32>, date: Option<i32>,
    ) {
        let Ok(index) = usize::try_from(mode) else { return };
        if index >= 4 { return; }
        let mut held = self.pricing_quotes.lock().unwrap();
        let record = &mut held.entry(id).or_default().records[index];
        if let Some(attributes) = attributes.filter(|value| *value != -1) {
            record.close_attributes = attributes;
        }
        if let Some(date) = date { record.close_date = Some(date); }
    }

    pub(crate) fn note_pricing_vwap(&self, id: InstrumentId, vwap: Option<f64>) {
        self.pricing_quotes.lock().unwrap().entry(id).or_default().vwap = vwap;
    }

    pub(crate) fn note_pricing_auction(
        &self, id: InstrumentId, mode: i32, price: Option<f64>, volume: i32,
        borrow: Option<f64>, borrow_volume: i32, lend: Option<f64>, lend_volume: i32,
    ) {
        let mut held = self.pricing_quotes.lock().unwrap();
        let views = held.entry(id).or_default();
        if mode == 0 {
            views.auction_price = price;
            views.auction_volume = volume;
        }
        views.auction_borrow = borrow;
        views.auction_borrow_volume = borrow_volume;
        views.auction_lend = lend;
        views.auction_lend_volume = lend_volume;
    }

    pub(crate) fn pricing_loan_sides(&self, id: InstrumentId) -> (Option<f64>, Option<f64>) {
        let held = self.pricing_quote_views(id);
        let fallback = held.auction_price.filter(|_| held.auction_volume != 0);
        (
            held.auction_borrow.filter(|_| held.auction_borrow_volume != 0).or(fallback),
            held.auction_lend.filter(|_| held.auction_lend_volume != 0).or(fallback),
        )
    }

    /// Zero every quote a caller can read, as the engine zeroes its own copy at
    /// the same moment.
    ///
    /// The engine zeroing its own is what stops a price from before a drop
    /// being read as current — but the copy a caller reads is this one, and it
    /// was left standing. Against a baseline the drop had just cleared, every
    /// field of that stale quote read as a move and went out again as a fresh
    /// tick; then whatever the venue had not restated by the next connection
    /// went out a second time as nought.
    #[doc(hidden)] pub fn zero_all_quotes(&self) {
        let blank = Quote::default();
        for slot in self.quotes.iter() {
            // Under the occupancy it was written under: zeroing a quote does
            // not change whose it is.
            let (_, generation) = slot.read_with_generation();
            slot.write(&blank, generation);
        }
        self.stamps.note_written();
        self.pricing_quotes.lock().unwrap().clear();
    }

    #[doc(hidden)] pub fn push_tbt_trade(&self, trade: TbtTrade) {
        self.tbt_trades.push_bounded(trade, STREAM_BACKLOG_LIMIT, "tbt_trades");
    }

    #[doc(hidden)] pub fn push_tbt_quote(&self, quote: TbtQuote) {
        self.tbt_quotes.push_bounded(quote, STREAM_BACKLOG_LIMIT, "tbt_quotes");
    }

    #[doc(hidden)] pub fn push_tbt_mid(&self, mid: TbtMid) {
        self.tbt_mids.push_bounded(mid, STREAM_BACKLOG_LIMIT, "tbt_mids");
    }


    #[doc(hidden)] pub fn push_real_time_bar(&self, req_id: u32, bar: RealTimeBar) {
        self.real_time_bars.push_bounded((req_id, bar), STREAM_BACKLOG_LIMIT, "real_time_bars");
    }

    #[doc(hidden)] pub fn push_depth_update(&self, update: DepthUpdate) {
        // A book is not a stream of independent rows: each entry says insert,
        // change or delete AT a position, so it only means anything against
        // every entry before it. Shedding the oldest of these the way a quote
        // or a trade is shed does not lose old rows — it leaves every later
        // position pointing into a book missing its start, and the reader gets
        // a well-formed book with the wrong prices in it.
        //
        // So a book that has run away is dropped whole, per request, and the
        // caller is told. Nothing is a book it can trust; a wrong one reads
        // like a right one.
        // The one that has run away is dropped, which is not the same as the
        // one that pushed. Every book shares this queue and each is drained on
        // its own, so measuring the whole and dropping whoever arrives next
        // destroys the book of a caller reading diligently because of one that
        // is not — and leaves the one that is not still flooding.
        //
        // Reading the queue costs walking it, so it is only walked once the
        // whole has run out of room, and what it drops then is the longest
        // book. That is the one nobody is draining, and dropping it puts the
        // queue back under its bound, so the next push is cheap again.
        {
            // Nothing is kept for a book already given up on. What arrives
            // now describes positions in a book that no longer exists, and
            // kept, it would be handed back as though it were one.
            if self.depth_dropped.lock().unwrap().contains(&update.req_id) {
                return;
            }
            let mut held = self.depth_updates.lock();
            if held.len() >= STREAM_BACKLOG_LIMIT {
                let mut per_book: std::collections::HashMap<u32, usize> =
                    std::collections::HashMap::new();
                for (_, u) in held.iter() {
                    *per_book.entry(u.req_id).or_insert(0) += 1;
                }
                if let Some((&worst, &how_many)) = per_book.iter().max_by_key(|(_, n)| **n) {
                    held.retain(|(_, u)| u.req_id != worst);
                    self.depth_dropped.lock().unwrap().insert(worst);
                    // Told on the request that asked for it, once. The venue
                    // goes on sending this book and nothing further is kept,
                    // so a caller not told reads a subscription that is up and
                    // a book that has stopped moving — which is what a quiet
                    // market looks like.
                    self.depth_drops_unsaid.push((
                        worst,
                        format!(
                            "the book on this request went past the {STREAM_BACKLOG_LIMIT} \
                             entries kept for one and was given up whole ({how_many} \
                             entries), because part of a book is not a book — withdraw it \
                             and ask again to start another",
                        ),
                    ));
                    if worst == update.req_id {
                        // Including the one that arrived. It is usually the
                        // book that ran away that pushes next, and kept, it
                        // would be the first entry of a book starting from
                        // the middle — which is the thing being prevented.
                        log::warn!(
                            "the book on request {worst} has gone past what is kept for \
                             one and was dropped whole ({how_many} entries), because part \
                             of a book is not a book — withdraw it and ask again to start \
                             another",
                        );
                        return;
                    }
                    log::warn!(
                        "the book on request {worst} has gone past what is kept for one and \
                         was dropped whole ({how_many} entries), because part of a book is \
                         not a book — resubscribe to start it again",
                    );
                }
            }
            self.depth_updates.push_held(&mut held, update);
        }
    }

    /// Throw away bars still queued under a request.
    ///
    /// Withdrawing stops the venue sending more; it does not unsend what has
    /// already arrived and nobody has read. Left there, the next request under
    /// the same number is served the previous stream's bars.
    #[doc(hidden)] pub fn purge_real_time_bars(&self, req_id: u32) {
        self.real_time_bars.retain(|(id, _)| *id != req_id);
    }

    /// Throw away tick-by-tick records still queued under a request.
    #[doc(hidden)] pub fn purge_tbt_for(&self, req_id: i64) {
        self.tbt_trades.retain(|t| t.req_id != req_id);
        self.tbt_quotes.retain(|q| q.req_id != req_id);
        // The midpoints as well. Left queued, a stream reopened under the same
        // number before the next read was served the withdrawn stream's
        // midpoints, under its own number and about another contract.
        self.tbt_mids.retain(|m| m.req_id != req_id);
    }

    /// Remove all buffered depth updates for a given req_id (called on cancel).
    #[doc(hidden)] pub fn purge_depth_updates(&self, req_id: u32) {
        self.depth_updates.retain(|u| u.req_id != req_id);
        // Withdrawing is how a caller starts again, so this is where a book
        // that was dropped stops being refused — and where the notice of that
        // drop goes with it. Left queued, a subscription started again under
        // the same number opened with the failure of the one before it: the
        // caller was told a healthy book had been given up on.
        self.depth_dropped.lock().unwrap().remove(&req_id);
        self.depth_drops_unsaid.retain(|(id, _)| *id != req_id);
    }

    /// Whether a book was dropped for running away and has not been asked for
    /// again. Nothing is kept for one until it is.
    #[doc(hidden)] pub fn depth_was_dropped(&self, req_id: u32) -> bool {
        self.depth_dropped.lock().unwrap().contains(&req_id)
    }



    /// The book given up on under one request, if there is one, leaving the
    /// rest.
    pub fn take_depth_drop_for(&self, req_id: u32) -> Option<String> {
        self.depth_drops_unsaid.take_first(|(id, _)| *id == req_id).map(|(_, why)| why)
    }

    #[doc(hidden)] pub fn push_tick_news(&self, news: TickNews) {
        let generation = self.generation_of(news.instrument);
        self.tick_news.push_bounded((generation, news), STREAM_BACKLOG_LIMIT, "tick_news");
    }

    /// A series the caller asked for, decoded, waiting to be delivered.
    ///
    /// The extra series ride on the same subscription as the prices but arrive
    /// on records of their own, so they are queued here rather than written
    /// into the quote: a quote holds one value per field and these are not
    /// fields of a quote. The poll that delivers the quote drains them and
    /// hands each to the caller under the number the reference client uses.
    ///
    /// Bounded like every other stream the venue pushes unasked: a caller who
    /// asked for a busy series and stopped reading would otherwise hold every
    /// record of the day.
    #[doc(hidden)] pub fn push_series_tick(&self, tick: SeriesTick) {
        let mut held = self.series_ticks.lock().unwrap();
        let queued = held.entry(tick.instrument).or_default();
        // Bounded per contract, because the venue serves a series whether or
        // not the caller polls: a busy series on a contract nobody is reading
        // would otherwise hold every record of the day. Past the bound the
        // oldest go, so what a late reader gets is the most recent rather than
        // everything or nothing.
        // A tenth at a time rather than one at a time, as every other stream
        // here sheds: dropping one per push leaves every later push shifting
        // the whole vector — on the thread reading the socket, under this
        // lock, for every record of a busy series.
        if queued.len() >= STREAM_BACKLOG_LIMIT {
            let keep_to = STREAM_BACKLOG_LIMIT - STREAM_BACKLOG_LIMIT / 10;
            let shed = queued.len() - keep_to;
            queued.drain(..shed);
            log::warn!(
                "the extra series on one contract have gone past {STREAM_BACKLOG_LIMIT} \
                 unread, so the oldest of them were dropped — nothing is draining them",
            );
        }
        queued.push(tick);
        self.stamps.note_written();
    }

    /// What this contract's extra series have stated since the last read.
    pub fn drain_series_ticks(&self, instrument: crate::types::InstrumentId) -> Vec<SeriesTick> {
        self.series_ticks.lock().unwrap().remove(&instrument).unwrap_or_default()
    }

    /// The venue's whole answer to a contract's chargeable snapshot, under
    /// the numbers a gateway publishes it under. Kept apart from the series,
    /// which reach everyone watching the contract: the answer is the
    /// snapshot's own.
    #[doc(hidden)] pub fn push_snapshot_answer(
        &self, instrument: crate::types::InstrumentId, answer: Vec<SeriesTick>,
    ) {
        self.snapshot_answers.lock().unwrap().insert(instrument, answer);
        self.stamps.note_written();
    }

    /// The answer to this contract's chargeable snapshot, where the venue has
    /// given one since the last read.
    pub fn take_snapshot_answer(
        &self, instrument: crate::types::InstrumentId,
    ) -> Option<Vec<SeriesTick>> {
        self.snapshot_answers.lock().unwrap().remove(&instrument)
    }

    /// What the venue says about a contract's two prices rather than what they
    /// are: one mask for whether each side may be dealt on without a human,
    /// one for pre-open and past-limit.
    ///
    /// Kept beside the quote rather than in it. A quote is read on the hot
    /// path and sized to the cache lines it occupies; these change seldom —
    /// a side becomes eligible or stops being, once — so they are held where
    /// widening costs nothing.
    #[doc(hidden)] pub fn note_quote_attributes(
        &self, instrument: crate::types::InstrumentId, eligible: i64, state: i64,
    ) {
        self.quote_attribute_masks.lock().unwrap().insert(instrument, (eligible, state));
        self.stamps.note_written();
    }

    /// The two masks as the venue last stated them, or nothing stated.
    pub fn quote_attribute_masks(&self, instrument: crate::types::InstrumentId) -> (i64, i64) {
        self.quote_attribute_masks.lock().unwrap().get(&instrument).copied().unwrap_or((0, 0))
    }

    /// Everything held for a contract goes when its subscription does.
    #[doc(hidden)] pub fn forget_series_ticks(&self, instrument: crate::types::InstrumentId) {
        self.series_ticks.lock().unwrap().remove(&instrument);
        self.snapshot_answers.lock().unwrap().remove(&instrument);
        self.quote_attribute_masks.lock().unwrap().remove(&instrument);
        self.pricing_quotes.lock().unwrap().remove(&instrument);
        self.attached_quote_instruments.lock().unwrap().retain(|_, slot| *slot != instrument);
    }

    /// A broadcast notice, kept until someone reads it.
    ///
    /// Bounded, because the venue broadcasts these whether or not anyone
    /// subscribed and the drain only runs once someone has: a session that
    /// never asks for bulletins would otherwise hold every notice of the day
    /// for the life of the process and free none of them. Past the bound the
    /// oldest are dropped, so a late subscriber is handed the most recent
    /// [`NEWS_BULLETIN_LIMIT`] rather than everything or nothing.
    #[doc(hidden)] pub fn push_news_bulletin(&self, bulletin: NewsBulletin) {
        let mut held = self.news_bulletins.lock();
        if held.len() >= NEWS_BULLETIN_LIMIT {
            // ponytail: O(n) shift on a queue of a thousand, on an event that
            // arrives a few times an hour. A VecDeque if bulletins ever became
            // a hot path.
            held.remove(0);
        }
        self.news_bulletins.push_held(&mut held, bulletin);
    }

    /// Whether the venue is restricting short sales in this contract.
    ///
    /// The circuit breaker a venue puts on a contract that has fallen far
    /// enough in a day, which stops a short from resting below the bid. Stated
    /// on the same record as the halt, decoded, and read by nobody: a program
    /// routing a short into such a contract had the order bounced rather than
    /// knowing not to send it.
    ///
    /// Not the same question as whether the contract can be borrowed, which
    /// this client already answers beside it — a contract can be freely
    /// borrowable and still restricted.
    pub fn short_sale_restricted(&self, instrument: crate::types::InstrumentId) -> bool {
        self.short_sale_restricted.lock().unwrap().contains(&instrument)
    }

    /// Say what the venue's status record said about short sales in a
    /// contract.
    #[doc(hidden)]
    pub fn note_short_sale_restriction(
        &self, instrument: crate::types::InstrumentId, restricted: bool,
    ) {
        let mut held = self.short_sale_restricted.lock().unwrap();
        if restricted {
            held.insert(instrument);
        } else {
            held.remove(&instrument);
        }
        self.stamps.note_written();
    }

    /// What the venue last said about a contract itself: how many shares are on
    /// issue and what it opened at a year ago.
    ///
    /// Stated on the tick that carries the price extremes, and read past. The
    /// documented API reaches neither from a quote subscription — share count
    /// is a fundamentals request of its own there, and a year-ago open has no
    /// call at all.
    pub fn contract_figures(
        &self, instrument: crate::types::InstrumentId,
    ) -> Option<crate::types::ContractFigures> {
        self.contract_figures.lock().unwrap().get(&instrument).copied()
    }

    /// What one series last stated for a contract, in the order it states it.
    ///
    /// Empty until that series has been asked for and answered. A figure the
    /// venue holds nothing for arrives as the largest number its field carries,
    /// which is how the venue says it has nothing rather than saying nothing at
    /// all, and a record that stopped short states the figures before it and no
    /// more.
    pub fn stated_figures(
        &self, instrument: crate::types::InstrumentId, series: u32,
    ) -> Vec<f64> {
        self.stated_figures.lock().unwrap().get(&(instrument, series)).cloned().unwrap_or_default()
    }

    /// Which series have stated figures for a contract, in order.
    pub fn stated_figures_series(&self, instrument: crate::types::InstrumentId) -> Vec<u32> {
        let mut series: Vec<u32> = self.stated_figures.lock().unwrap()
            .keys()
            .filter(|(at, _)| *at == instrument)
            .map(|(_, series)| *series)
            .collect();
        series.sort_unstable();
        series
    }

    /// The figures one series stated for a contract under the venue's own
    /// numbering, whole or fractional.
    ///
    /// Ten series state their figures as two numbered tables — among them where
    /// a contract's volatility stands against its own past, both the
    /// volatility the market implies and the one it has actually shown: the
    /// rank, the percentile and the high and low of each.
    ///
    /// On those six the number each figure is kept under is **weeks**, and the
    /// venue keeps three windows: a quarter, a half year and a year. On the
    /// two that state a high and a low, the window is written negative for the
    /// low and positive for the high, so a year's range is the pair numbered
    /// minus fifty-two and fifty-two. Read off a live session, where a share
    /// stood at the same rank across all three windows while a fund's moved
    /// with each. Two of the
    /// numbers have a documented call to reach a caller on and the rest have
    /// none, so the rest are kept here under the number the venue gave them
    /// rather than sent as a tick number of this client's own choosing.
    ///
    /// Empty until that series has been asked for and answered.
    pub fn numbered_figures(
        &self, instrument: crate::types::InstrumentId, series: u32, fractional: bool,
    ) -> Vec<(i32, f64)> {
        self.numbered_figures
            .lock()
            .unwrap()
            .get(&(instrument, series, fractional))
            .cloned()
            .unwrap_or_default()
    }

    /// Which series have stated numbered figures for a contract, in order.
    pub fn numbered_figures_series(&self, instrument: crate::types::InstrumentId) -> Vec<u32> {
        let mut series: Vec<u32> = self.numbered_figures.lock().unwrap()
            .keys()
            .filter(|(at, ..)| *at == instrument)
            .map(|(_, series, _)| *series)
            .collect();
        series.sort_unstable();
        series.dedup();
        series
    }

    /// The run of paired figures one series stated for a contract, in the
    /// order it stated them.
    ///
    /// Two series state their figures this way: a count, then that many pairs.
    /// One holds the volatility the venue's own model puts on each point of a
    /// curve; the other holds the weight it puts on each price a contract might
    /// reach. Neither has a documented call to arrive on.
    ///
    /// Empty until that series has been asked for and answered.
    pub fn paired_figures(
        &self, instrument: crate::types::InstrumentId, series: u32,
    ) -> Vec<(f64, f64)> {
        self.paired_figures.lock().unwrap().get(&(instrument, series)).cloned().unwrap_or_default()
    }

    /// Which series have stated paired figures for a contract, in order.
    pub fn paired_figures_series(&self, instrument: crate::types::InstrumentId) -> Vec<u32> {
        let mut series: Vec<u32> = self.paired_figures.lock().unwrap()
            .keys()
            .filter(|(at, _)| *at == instrument)
            .map(|(_, series)| *series)
            .collect();
        series.sort_unstable();
        series
    }

    /// What the venue's model made of an option as it closed.
    ///
    /// The same model as the standing one and in the same shape — every greek
    /// it states, including the ones the documented API has no field for — but
    /// worked out as the contract closed rather than as it stands. The
    /// documented API has no call for it at all.
    ///
    /// `None` until the series has been asked for and answered.
    pub fn closing_option_model(
        &self, instrument: crate::types::InstrumentId,
    ) -> Option<crate::types::OptionComputation> {
        self.closing_option_model.lock().unwrap().get(&instrument).copied()
    }

    #[doc(hidden)] pub fn note_closing_option_model(
        &self, comp: crate::types::OptionComputation,
    ) {
        self.closing_option_model.lock().unwrap().insert(comp.instrument, comp);
        self.stamps.note_written();
    }

    /// The rows of three figures one series stated for a contract, in the
    /// order it stated them. What the three are is the series' own:
    ///
    /// | Series | The three figures |
    /// | --- | --- |
    /// | 547 | A quantity, what it is offered at, and a second price where the form states one |
    /// | 491 | Which strategy the leg belongs to, the contract it names, and its size |
    /// | 320, 376, 530, 532 | The venue's number for a field of a packed quote, its figure, and how far that figure's decimal point moves |
    ///
    /// On the packed quotes the venue numbers its fields itself, and what those
    /// numbers mean was read off a live session rather than guessed: nought and
    /// one are the two sides of the quote, four and five the size behind each.
    /// A side the venue is not standing behind reads as minus one hundred.
    ///
    /// The figures on those are counted in the contract's own increments, as
    /// every packed figure is — a quote of 75815 against an increment of a
    /// hundredth is 758.15 — and the increment is on the contract. No scale is
    /// put on them here, because which of the numbered fields are prices and
    /// which are counts is the venue's to change.
    ///
    /// Neither has a documented call to arrive on. `f64::MAX` stands where a
    /// form states no third figure. Empty until the series has been asked for
    /// and answered.
    pub fn stated_rows(
        &self, instrument: crate::types::InstrumentId, series: u32,
    ) -> Vec<(f64, f64, f64)> {
        self.stated_rows.lock().unwrap().get(&(instrument, series)).cloned().unwrap_or_default()
    }

    /// Which series have stated rows for a contract, in order.
    pub fn stated_rows_series(&self, instrument: crate::types::InstrumentId) -> Vec<u32> {
        let mut series: Vec<u32> = self.stated_rows.lock().unwrap()
            .keys()
            .filter(|(at, _)| *at == instrument)
            .map(|(_, series)| *series)
            .collect();
        series.sort_unstable();
        series
    }

    /// The strategies a spread scan last stated for an underlying.
    ///
    /// Each carries its legs, which shape of strategy it is and how pressing
    /// the venue takes it to be, the thirteen figures the venue states about it,
    /// and where it comes out even. The documented API has no call for any of
    /// this. Empty until a scan has been asked for and answered.
    pub fn scanned_strategies(
        &self, instrument: crate::types::InstrumentId,
    ) -> Vec<crate::types::ScannedStrategy> {
        self.scanned_strategies.lock().unwrap().get(&instrument).cloned().unwrap_or_default()
    }

    #[doc(hidden)] pub fn note_scanned_strategies(
        &self, instrument: crate::types::InstrumentId,
        strategies: Vec<crate::types::ScannedStrategy>,
    ) {
        self.scanned_strategies.lock().unwrap().insert(instrument, strategies);
        self.stamps.note_written();
    }

    /// What one of the option model's chain series last stated for an
    /// underlying: per class of its options, the underlying's price, the
    /// dividends expected and per expiry the yield, the rate, the forward and
    /// the at-the-money volatilities.
    ///
    /// The standing set is series 687 and the set as the chain closed is 691.
    /// The documented API has no call for either. Empty until the series has
    /// been asked for and answered, and gone with the subscription.
    pub fn chain_model_parameters(
        &self, instrument: crate::types::InstrumentId, series: u32,
    ) -> Vec<crate::types::ChainModelParameters> {
        self.chain_model_parameters
            .lock()
            .unwrap()
            .get(&(instrument, series))
            .cloned()
            .unwrap_or_default()
    }

    /// A chain series withdrawn: what it last stated is no longer standing.
    #[doc(hidden)] pub fn forget_chain_model_parameters(
        &self, instrument: crate::types::InstrumentId, series: u32,
    ) {
        self.chain_model_parameters.lock().unwrap().remove(&(instrument, series));
    }

    #[doc(hidden)] pub fn note_chain_model_parameters(
        &self, instrument: crate::types::InstrumentId, series: u32,
        sets: Vec<crate::types::ChainModelParameters>,
    ) {
        self.chain_model_parameters.lock().unwrap().insert((instrument, series), sets);
        self.stamps.note_written();
    }

    #[doc(hidden)] pub fn note_stated_rows(
        &self, instrument: crate::types::InstrumentId, series: u32, rows: Vec<(f64, f64, f64)>,
    ) {
        self.stated_rows.lock().unwrap().insert((instrument, series), rows);
        self.stamps.note_written();
    }

    #[doc(hidden)] pub fn note_paired_figures(
        &self, instrument: crate::types::InstrumentId, series: u32, pairs: Vec<(f64, f64)>,
    ) {
        self.paired_figures.lock().unwrap().insert((instrument, series), pairs);
        self.stamps.note_written();
    }

    #[doc(hidden)] pub fn note_numbered_figures(
        &self, instrument: crate::types::InstrumentId, series: u32,
        whole: &[(i32, f64)], fractional: &[(i32, f64)],
    ) {
        // A table the record did not carry is not a table the venue emptied:
        // left out of one message, what it last stated still stands.
        let mut held = self.numbered_figures.lock().unwrap();
        for (table, stated) in [(false, whole), (true, fractional)] {
            if !stated.is_empty() {
                held.insert((instrument, series, table), stated.to_vec());
                self.stamps.note_written();
            }
        }
    }

    #[doc(hidden)] pub fn note_stated_figures(
        &self, instrument: crate::types::InstrumentId, series: u32, figures: Vec<f64>,
    ) {
        self.stated_figures.lock().unwrap().insert((instrument, series), figures);
        self.stamps.note_written();
    }

    /// Keep what the venue stated about a contract, merging with what it said
    /// before: the two figures arrive on the same tick but either may be
    /// absent from a given one, and a figure left out is not a figure withdrawn.
    #[doc(hidden)] pub fn note_contract_figures(
        &self,
        instrument: crate::types::InstrumentId,
        shares_outstanding: Option<f64>,
        open_a_year_ago: Option<f64>,
    ) {
        let mut held = self.contract_figures.lock().unwrap();
        let entry = held.entry(instrument).or_default();
        if let Some(v) = shares_outstanding {
            entry.shares_outstanding = v;
        }
        if let Some(v) = open_a_year_ago {
            entry.open_a_year_ago = v;
        }
        self.stamps.note_written();
    }

    /// Forget what a spread scan stated for a slot, once the scan is withdrawn:
    /// the next scan of the contract states its own.
    #[doc(hidden)] pub fn forget_scanned_strategies(&self, instrument: crate::types::InstrumentId) {
        self.scanned_strategies.lock().unwrap().remove(&instrument);
    }

    /// Keep a calculation until the venue states the model it needs.
    #[doc(hidden)] pub fn keep_calculation(&self, req_id: i64, calculation: KeptCalculation) {
        let mut kept = self.kept_calculations.lock().unwrap();
        let waiting = usize::from(!calculation.answered);
        let previous = kept.insert(req_id, calculation).is_some_and(|c| !c.answered);
        if previous { self.calculations_waiting.fetch_sub(1, Ordering::Relaxed); }
        self.calculations_waiting.fetch_add(waiting, Ordering::Relaxed);
    }

    /// Stop keeping one, and say what it was.
    #[doc(hidden)] pub fn forget_calculation(&self, req_id: i64) -> Option<KeptCalculation> {
        let mut kept = self.kept_calculations.lock().unwrap();
        let removed = kept.remove(&req_id);
        if removed.as_ref().is_some_and(|c| !c.answered) {
            self.calculations_waiting.fetch_sub(1, Ordering::Relaxed);
        }
        removed
    }

    /// Whether a calculation is kept under this number.
    pub fn holds_calculation(&self, req_id: i64) -> bool {
        self.kept_calculations.lock().unwrap().contains_key(&req_id)
    }

    /// How many calculations are kept, answered or not.
    pub fn kept_calculation_count(&self) -> usize {
        self.kept_calculations.lock().unwrap().len()
    }

    /// Calculations waiting for the model after their contract was named.
    pub(crate) fn calculations_waiting_for_model(&self) -> usize {
        self.calculations_waiting.load(Ordering::Relaxed)
    }

    /// Work through the unanswered calculations `pick` names, under one
    /// acquisition: each is answered by `answer`, which says whether it was,
    /// so a model written while a caller is keeping one answers it once.
    #[doc(hidden)] pub fn answer_kept_calculations(
        &self,
        pick: impl Fn(i64, &KeptCalculation) -> bool,
        mut answer: impl FnMut(i64, &KeptCalculation) -> bool,
    ) {
        let mut kept = self.kept_calculations.lock().unwrap();
        let mut ids: Vec<i64> = kept
            .iter()
            .filter(|(id, c)| !c.answered && pick(**id, c))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            let calculation = kept[&id].clone();
            if answer(id, &calculation) {
                kept.get_mut(&id).unwrap().answered = true;
                self.calculations_waiting.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// What the venue last said its own model made of a contract.
    ///
    /// Kept as well as delivered. Delivered alone it is gone the moment a
    /// caller reads it, and answering "what would this be worth at another
    /// volatility" needs the venue's statement still to hand.
    pub fn option_model(&self, instrument: crate::types::InstrumentId) -> Option<crate::types::OptionComputation> {
        self.last_option_model.lock().unwrap().get(&instrument).copied()
    }

    #[doc(hidden)] pub fn push_option_computation(&self, comp: crate::types::OptionComputation) {
        // Only the venue's own statement becomes the model for a contract. An
        // answer worked out here belongs to the question that asked it and
        // names no contract at all — stored, it lands on slot zero, which is a
        // real contract, and the next question about that contract is answered
        // against the last caller's own volatility and price instead of the
        // venue's. It also says nothing about which model the venue used, so
        // the refusal that guards the one this client cannot solve with reads
        // as though there were nothing to guard.
        //
        // And only an answer is delivered as it stands. What a watcher of the
        // contract is sent is the model tick built from this and the venue's
        // volatilities, pushed on its own.
        if comp.answers.is_none() {
            self.last_option_model.lock().unwrap().insert(comp.instrument, comp);
            return;
        }
        self.option_computations.push_bounded(comp, STREAM_BACKLOG_LIMIT, "option_computations");
    }

    /// A model tick for whoever watches its option.
    #[doc(hidden)] pub fn push_option_tick(&self, tick: OptionTick) {
        let generation = self.generation_of(tick.instrument);
        self.option_ticks.push_bounded((generation, tick), STREAM_BACKLOG_LIMIT, "option_ticks");
    }

    #[doc(hidden)] pub fn push_companion_refusal(
        &self, instrument: crate::types::InstrumentId, kind: u32, reason: String,
    ) {
        self.companion_refusals.push((instrument, self.generation_of(instrument), kind, reason));
    }

    #[doc(hidden)] pub fn subscription_data_type(&self, instrument: InstrumentId, requested: i32) -> std::sync::Arc<std::sync::atomic::AtomicI32> {
        self.subscription_data_types.lock().unwrap().entry(instrument)
            .or_insert_with(|| std::sync::Arc::new(std::sync::atomic::AtomicI32::new(requested))).clone()
    }
    #[doc(hidden)] pub fn push_subscription_notice(&self, instrument: InstrumentId, notice: crate::error_codes::Refusal) {
        self.subscription_notices.push((instrument, self.generation_of(instrument), notice));
    }
    #[doc(hidden)] pub fn push_market_data_type(&self, instrument: InstrumentId, data_type: i32) {
        self.market_data_types.push((instrument, self.generation_of(instrument), data_type));
    }
    #[cfg(test)] pub(crate) fn drain_subscription_notices(&self) -> Vec<(InstrumentId, crate::error_codes::Refusal)> {
        self.subscription_notices.drain().into_iter().map(|(id, _, notice)| (id, notice)).collect()
    }
    #[doc(hidden)] pub fn push_subscription_failure(&self, instrument: crate::types::InstrumentId, reason: String) {
        self.last_subscription_failure.lock().unwrap()
            .insert(instrument, reason.clone());
        self.subscription_failures.push((instrument, self.generation_of(instrument), reason));
    }

    /// Why a contract a request is about to join was refused, if it was.
    /// `None` where the subscription it joins is live.
    pub fn failure_for_follower(&self, instrument: crate::types::InstrumentId) -> Option<String> {
        self.last_subscription_failure.lock().unwrap().get(&instrument).cloned()
    }

    /// Why the quote feed is done for the rest of this session, if it is.
    ///
    /// A subscription asked for after this is set cannot be served: there is
    /// no connection to write it to and no reconnect coming to replay it.
    pub fn market_data_over(&self) -> Option<&'static str> {
        *self.market_data_over.lock().unwrap()
    }

    /// Say the quote feed is done for the rest of this session.
    #[doc(hidden)] pub fn set_market_data_over(&self, why: &'static str) {
        *self.market_data_over.lock().unwrap() = Some(why);
    }

    /// And take it back, for a feed a caller has rebuilt by hand.
    ///
    /// The engine gives up on its own recovery and never picks it up again,
    /// which is what giving up means — but a caller handing in a transport of
    /// its own is not the engine's recovery, and the feed it hands in is live.
    /// Left standing, every subscription on that live feed was refused for the
    /// rest of the session.
    #[doc(hidden)] pub fn clear_market_data_over(&self) {
        *self.market_data_over.lock().unwrap() = None;
    }

    /// The venue has taken this contract's subscription, so the reason it
    /// last refused one is no longer what a joining request is owed.
    ///
    /// Only the copy kept for joiners is dropped. The queued refusals are
    /// deliveries the caller that asked has not read yet, and a later success
    /// does not unsay them — the request they name was refused.
    ///
    /// Kept until the slot went back to the table, the reason a contract could
    /// not be subscribed before a reconnect was still there afterwards, and
    /// every request joining the subscription that had since come up was told
    /// it had been refused.
    #[doc(hidden)] pub fn note_subscription_accepted(&self, id: crate::types::InstrumentId) {
        self.last_subscription_failure.lock().unwrap().remove(&id);
    }

    /// A refusal owed to one request that joined a contract already refused.
    #[doc(hidden)] pub fn push_subscription_failure_for(&self, req_id: i64, reason: String) {
        self.subscription_failures_direct.push((req_id, reason));
    }

    /// Take every refusal owed to a single request, leaving none.
    pub fn drain_subscription_failures_direct(&self) -> Vec<(i64, String)> {
        self.subscription_failures_direct.drain()
    }

    /// Publish how many slots the engine has handed out. The quote table is
    /// grown to hold them here, so a slot handed out reads as the empty quote
    /// until its first tick wherever it falls, and no tick grows the table.
    #[doc(hidden)] pub fn set_instrument_count(&self, count: u32) {
        if let Some(last) = count.checked_sub(1) {
            self.quotes.get_or_grow(last);
        }
        self.instrument_count.store(count as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod follower_tick_req_params_tests {
    use super::*;

    fn tick(min_tick: f64) -> TickReqParams {
        TickReqParams { min_tick, ..Default::default() }
    }

    /// A request that follows a live subscription is owed the increment that
    /// subscription was acknowledged with — the venue sends one tickReqParams
    /// per reqMktData, and a follower asked for none. Cleared when the slot is
    /// given back, so a reclaimed contract does not carry a stale increment.
    #[test]
    fn a_follower_is_owed_the_cached_increment() {
        let m = MarketDataState::new();
        let instrument = 5;

        assert_eq!(m.tick_req_params_for_follower(instrument), None, "none before the acknowledgement");
        m.push_tick_req_params(instrument, tick(0.01));
        assert_eq!(m.tick_req_params_for_follower(instrument), Some(tick(0.01)), "cached from the acknowledgement");

        // The follower is owed it, delivered to that request alone.
        m.push_tick_req_params_for(2, tick(0.01));
        assert_eq!(m.drain_tick_req_params_direct(), vec![(2, tick(0.01))]);
        assert!(m.drain_tick_req_params_direct().is_empty(), "taken once");

        m.note_released_slot(instrument, u64::MAX);
        assert_eq!(m.tick_req_params_for_follower(instrument), None, "cleared when the slot is given back");
    }

    /// Publishing waits for the follower's copy to be ready, so a follower
    /// joining after dispatch has taken its recipients still has an answer.
    #[test]
    fn the_followers_increment_is_ready_before_the_acknowledgement() {
        let market = MarketDataState::new();
        let queued = market.tick_req_params.lock();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| market.push_tick_req_params(5, tick(0.025)));
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let cached = loop {
                let cached = market.tick_req_params_for_follower(5);
                if cached.is_some() || std::time::Instant::now() >= deadline {
                    break cached;
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            assert!(queued.is_empty());
            drop(queued);
            writer.join().unwrap();
            assert_eq!(cached, Some(tick(0.025)), "publication cannot precede the follower's copy");
        });
        assert_eq!(market.drain_tick_req_params(), vec![(5, tick(0.025))]);
        assert_eq!(market.tick_req_params_for_follower(5), Some(tick(0.025)));
    }
}

#[cfg(test)]
mod depth_backlog_tests {
    use super::*;

    fn entry(req_id: u32) -> DepthUpdate {
        DepthUpdate {
            req_id,
            position: 0,
            market_maker: String::new(),
            operation: 0,
            side: 1,
            price: 1.0,
            size: 1.0,
            is_smart_depth: false,
        }
    }

    /// The book that is dropped is the longest one, not whoever happened to
    /// push at the moment the queue filled.
    ///
    /// Kept apart from the test below because there the flooder is also the
    /// pusher, so dropping the longest and dropping the pusher pick the same
    /// book and neither rule is pinned. Here a caller reading its own small
    /// book is the one that pushes at the bound: taking the pusher would give
    /// up the book that was being read and leave the runaway streaming.
    #[test]
    fn the_longest_book_is_dropped_and_not_the_one_that_pushed() {
        let market = MarketDataState::new();
        let flooding = 7;
        let reading = 9;

        for _ in 0..STREAM_BACKLOG_LIMIT - 1 {
            market.push_depth_update(entry(flooding));
        }
        // Fills the queue exactly, so the next push is the one that walks it.
        market.push_depth_update(entry(reading));
        market.push_depth_update(entry(reading));

        assert!(market.depth_was_dropped(flooding), "the longest book was the one given up");
        assert!(!market.depth_was_dropped(reading), "not the one that pushed");
        assert_eq!(
            market.take_depth_updates_for(reading).len(), 2,
            "and the pusher's own book is whole, the update that triggered it included",
        );
    }

    /// A book that runs away unread is dropped whole, and nothing further is
    /// kept for it until the caller asks again.
    ///
    /// A book only means anything whole: each entry names a position, and
    /// once entries are gone everything after them describes a book that no
    /// longer exists. Kept, those would be handed back reading exactly like a
    /// real book. And the one dropped is the one that ran away, not whoever
    /// happened to push next — they share a queue, and each is drained on its
    /// own.
    #[test]
    fn a_runaway_book_is_dropped_and_not_quietly_restarted() {
        let market = MarketDataState::new();

        // One caller reads nothing; another keeps one entry outstanding.
        let flooding = 7;
        let reading = 9;
        market.push_depth_update(entry(reading));
        for _ in 0..STREAM_BACKLOG_LIMIT {
            market.push_depth_update(entry(flooding));
        }

        assert!(market.depth_was_dropped(flooding), "the book that ran away was given up");
        assert!(!market.depth_was_dropped(reading), "and the one being read was not");
        assert_eq!(
            market.take_depth_updates_for(reading).len(), 1,
            "a caller that was reading keeps its book",
        );

        // Nothing further is kept for the dropped one: a part of a book is
        // not a book.
        market.push_depth_update(entry(flooding));
        assert!(
            market.take_depth_updates_for(flooding).is_empty(),
            "nothing is handed back for a book that was given up",
        );

        // Withdrawing is how it starts again.
        market.purge_depth_updates(flooding);
        assert!(!market.depth_was_dropped(flooding));
        market.push_depth_update(entry(flooding));
        assert_eq!(market.take_depth_updates_for(flooding).len(), 1, "and it does");
    }
}

#[cfg(test)]
mod option_model_tests {
    use super::*;
    use crate::types::OptionComputation;

    /// A book started again does not open with the failure of the one before it.
    ///
    /// Withdrawing is how a caller starts again after a book was given up on,
    /// and the notice of that giving up was left queued. The next subscription
    /// under the same number then opened with it: a healthy book, receiving a
    /// fresh picture, reported as one this client had abandoned.
    #[test]
    fn a_withdrawal_takes_the_notice_of_a_dropped_book_with_it() {
        let market = MarketDataState::new();
        for _ in 0..STREAM_BACKLOG_LIMIT + 1 {
            market.push_depth_update(DepthUpdate {
                req_id: 4, position: 0, market_maker: String::new(), operation: 0,
                side: 1, price: 100.0, size: 1.0, is_smart_depth: false,
            });
        }
        assert!(market.depth_was_dropped(4), "the book was given up on");
        assert!(
            !market.depth_drops_unsaid.is_empty(),
            "and the caller has not been told yet",
        );

        market.purge_depth_updates(4);

        assert!(!market.depth_was_dropped(4), "asked for again, it is not refused");
        assert!(
            market.depth_drops_unsaid.is_empty(),
            "and the notice of the last one does not open the next",
        );
    }

    /// An answer worked out here does not become the venue's model for a
    /// contract.
    ///
    /// It names no contract — a solve answers a request, not an instrument —
    /// so stored it lands on slot zero, which is a real contract. The next
    /// question about that contract would then be answered against the last
    /// caller's own volatility and price, and against a record saying nothing
    /// about which model the venue used.
    #[test]
    fn a_local_answer_does_not_become_the_venues_model() {
        let market = MarketDataState::new();

        // The venue's own statement for the contract in slot zero.
        market.push_option_computation(OptionComputation {
            instrument: 0,
            implied_vol: 0.25,
            price_based_vol: true,
            ..Default::default()
        });
        // And a caller's question answered here, which names no contract.
        market.push_option_computation(OptionComputation {
            implied_vol: 0.99,
            ..OptionComputation::solved(7)
        });

        let stated = market.option_model(0).expect("the venue's statement stands");
        assert_eq!(stated.implied_vol, 0.25, "the venue's volatility, not the answer's");
        assert!(stated.price_based_vol, "and what it says about the model it used");
    }
}

#[cfg(test)]
mod venue_clock_tests {
    use super::{MarketDataState, local_millis};

    /// A session that has been told nothing about the venue's clock still
    /// answers, with this machine's.
    ///
    /// The question is never put to the venue, so there is nothing to wait
    /// for and nothing to refuse over. Refused, a caller on a connected
    /// session was handed a failure for a call that cannot fail.
    #[test]
    fn an_unstated_clock_answers_this_machines_own() {
        let market = MarketDataState::new();
        assert!(
            (market.venue_time_millis() - local_millis()).abs() < 1_000,
            "no skew is no shift, not no answer",
        );
    }

    /// What the venue states shifts the answer to the venue's clock.
    #[test]
    fn a_stated_clock_shifts_the_answer_onto_it() {
        let market = MarketDataState::new();
        market.note_venue_time("20260815-12:00:00");
        assert!(
            (market.venue_time_millis() - 1_786_795_200_000).abs() < 2_000,
            "the venue's clock, from a stamp days away from this machine's",
        );

        // And stated as a number of its own, which is how the venue pushes it.
        market.note_venue_millis(1_786_795_200_000 + 86_400_000);
        assert!(
            (market.venue_time_millis() - (1_786_795_200_000 + 86_400_000)).abs() < 2_000,
            "the later statement is the one in force",
        );
    }

    /// A clock at the end of what the type holds is clamped, not wrapped.
    ///
    /// The venue states its clock in seconds and it is held in milliseconds, so
    /// a second count near the end of the range does not survive the
    /// conversion. Wrapped, the product came out negative and the session ran a
    /// thousand years behind a clock nobody stated — every stamp this client
    /// puts on a request, and every answer it reads a time off, with it. The
    /// reference conversion saturates and so does this one.
    #[test]
    fn a_clock_past_what_the_type_holds_is_clamped() {
        let market = MarketDataState::new();
        market.note_venue_millis(i64::MAX);
        assert!(
            market.venue_time_millis() > 0,
            "the clock stays a clock, however far out the statement is",
        );
        market.note_venue_millis(i64::MIN);
        assert!(
            market.venue_time_millis() < 0,
            "and the same at the other end, without wrapping back to a future date",
        );
    }

    /// The answer keeps moving while the venue says nothing.
    ///
    /// Held as the stamp on the last message seen, it stood still on a quiet
    /// connection: two readings a moment apart named the same instant, and a
    /// caller measuring the difference between the clocks watched it drift by
    /// exactly the time it had been waiting.
    #[test]
    fn the_answer_advances_on_a_quiet_connection() {
        let market = MarketDataState::new();
        market.note_venue_time("20260815-12:00:00");
        let first = market.venue_time_millis();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(
            market.venue_time_millis() > first,
            "the clock ran on though the venue stated nothing further",
        );
    }
    /// The point between the two goes in and comes out, like every other
    /// stream beside it.
    #[test]
    fn a_midpoint_pushed_is_a_midpoint_drained() {
        let market = MarketDataState::new();
        market.push_tbt_mid(crate::types::TbtMid {
            instrument: 0, req_id: 7, price: 150_250_000_000, timestamp: 1,
        });
        let out = market.drain_tbt_mids();
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].req_id, 7);
        assert!(market.drain_tbt_mids().is_empty(), "and only once");
    }

}
