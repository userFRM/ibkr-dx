//! What a surface asks the engine to do.
//!
//! One channel carries every request from both surfaces into the loop, so the
//! commands are one enum. A request names the contract it is about rather than
//! spelling out its fields, and states separately what narrows a lookup when
//! the caller passed no id.

use super::*;

/// Optional request-side filters for a by-symbol contract-details lookup.
/// Empty/zero fields are omitted from the request.
#[derive(Debug, Clone, Default)]
pub struct SecDefFilters {
    /// Where the contract is listed.
    pub primary_exchange: String,
    /// The venue's name for the contract.
    pub local_symbol: String,
    /// An option's expiry or a future's month.
    pub last_trade_date_or_contract_month: String,
    /// An option's strike.
    pub strike: f64,
    /// `C` or `P`.
    pub right: String,
    /// How many units one contract is worth.
    pub multiplier: String,
    /// Which class of the chain.
    pub trading_class: String,
    /// Identifier lookup (e.g. ISIN): raw identifier and its type. When set, the
    /// lookup rides the identifier instead of the symbol.
    pub sec_id: String,
    /// Which identifier `sec_id` is: `CUSIP`, `SEDOL`, `ISIN`, `RIC`, `FIGI`
    /// or `BB_SYMBOL`, spelled exactly so. Any other name, a lower-case one
    /// included, is not an identifier kind to a gateway: the lookup goes by
    /// symbol and the identifier is left out.
    pub sec_id_type: String,
    /// Who issued it. A lookup that states one is answered under a fixed-income
    /// security type whatever the caller named, so it narrows the lookup the
    /// way an identifier does rather than describing the contract.
    pub issuer_id: String,
}

/// The contract a request names.
///
/// Every one of these requests names a contract, and each used to carry it as
/// loose fields — five of them on most, nine on a subscription — copied out of
/// the caller's contract at seventeen call sites and destructured back at the
/// other end. One field per request rather than five says the same thing, and
/// says it the way the reference client does: its calls take a contract too.
///
/// The last four tell one option from another on the same underlying. They are
/// empty on anything without an expiry, which is every share and every
/// currency pair.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContractRef {
    /// The venue's number for it, or `0` where the caller named it by
    /// description and the lookup has not answered yet.
    pub con_id: i64,
    /// Its ticker.
    pub symbol: String,
    /// What kind of contract it is, in the reference client's spelling.
    pub sec_type: String,
    /// Where it is to be traded or quoted.
    pub exchange: String,
    /// What it is priced in.
    pub currency: String,
    /// When it expires.
    pub last_trade_date: String,
    /// What it may be exercised at.
    pub strike: f64,
    /// Whether it is a call or a put.
    pub right: String,
    /// How many of the underlying one contract is.
    pub multiplier: String,
}

/// What a caller asked for of the calendar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalendarQuery {
    /// The contract to fetch events for, where the caller named one rather
    /// than writing its own filter.
    pub con_id: Option<i64>,
    /// The caller's own filter document, passed to the venue as written.
    pub filter: String,
    /// The window, stated as the venue states dates.
    pub start_date: String,
    /// Its end.
    pub end_date: String,
    /// How many events at most. Stated as text, which is how the venue takes
    /// it, and left out entirely when the caller set no limit.
    pub total_limit: Option<i64>,
    /// Whether to fill from the watchlist, the portfolio, and competitors.
    pub fill_watchlist: bool,
    /// Whether to include what the account holds.
    pub fill_portfolio: bool,
    /// Whether to include the issuer's competitors.
    pub fill_competitors: bool,
}

/// A request the engine answers from what the session holds, once the venue
/// has stated it: the account's holdings and figures, and what it is working.
///
/// Held in the engine's loop until then, so a call that asks returns at once
/// and the answer stands where the venue's statement completes.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// `req_positions`: every holding, then `position_end`.
    Positions,
    /// `req_positions_multi`, under the caller's number.
    PositionsMulti {
        /// The caller's number.
        req_id: i64,
        /// The account named.
        account: String,
        /// The model label echoed on the answer.
        model_code: String,
    },
    /// `req_account_updates(true, ..)`: the subscription starts where its
    /// answer stands.
    AccountUpdates {
        /// The account named.
        account: String,
    },
    /// `req_account_updates_multi`, under the caller's number.
    AccountUpdatesMulti {
        /// The caller's number.
        req_id: i64,
        /// The account named.
        account: String,
        /// The model label echoed on the answer.
        model_code: String,
        /// Whether only the ledger and the net liquidation were asked for.
        ledger_and_nlv: bool,
    },
    /// `req_open_orders` or `req_all_open_orders`, once the venue has named
    /// what the account is working.
    OpenOrders(crate::types::model::Question),
    /// `req_ids`, once the venue has named what the account is working: the
    /// id is floored above it.
    NextValidId,
}

/// A calculation answered from the venue's model for a contract, as the
/// caller asked it: the engine keeps it on the slot the request is served on
/// and answers it where the venue states the model.
#[derive(Debug, Clone)]
pub struct Calculation {
    /// The contract it is on.
    pub contract: crate::types::model::Contract,
    /// Whether it inverts a price or prices a volatility.
    pub wants_volatility: bool,
    /// The option price or volatility the caller supplied.
    pub option_price: f64,
    /// The underlying price the caller supplied.
    pub under_price: f64,
}

/// An order a caller placed, as its call states it.
///
/// The engine names the contract where the caller described it, registers it,
/// runs the checks a gateway makes against the orders it holds, builds the
/// order, and sends it or keeps it: none of that waits on the caller's thread.
#[derive(Debug, Clone)]
pub struct Placement {
    /// The allocator also used for children whose API and venue numbers differ.
    pub(crate) allocator: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The number it is placed under.
    pub order_id: OrderId,
    /// The contract as the caller stated it, legs and hedge included.
    pub contract: crate::types::model::Contract,
    /// The order as this session sends it, under its number.
    pub order: crate::types::model::Order,
    /// What a gateway says about an order it places anyway, said once the
    /// order has gone or is kept.
    pub warnings: Vec<crate::error_codes::Refusal>,
}

/// An exercise or a lapse of an option position, as its call states it.
#[derive(Debug, Clone)]
pub struct Exercise {
    /// The caller's allocator, when the engine must assign a number after replay.
    pub allocator: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// The number the caller stated, which a refusal is reported under.
    pub req_id: i64,
    /// The order number it goes out under.
    pub order_id: OrderId,
    /// Whether the caller stated that number, which may not be a working
    /// order's, or was given one.
    pub stated: bool,
    /// The option.
    pub contract: crate::types::model::Contract,
    /// 1 exercises, 2 lapses.
    pub action: u8,
    /// How many contracts.
    pub qty: Qty,
    /// The account it is taken on, where one is named.
    pub account: String,
    /// What the instruction states about itself.
    pub states: ExerciseStates,
    /// Whether the caller overrides the natural action: an exercise of an
    /// option out of the money, or a lapse of one in it, goes anyway rather
    /// than being refused.
    pub override_: bool,
}

/// What an exercise states beyond the instruction itself.
///
/// Three fields an order carries and an exercise did not take, though the same
/// request goes out either way and the same tags carry them: when a person
/// entered it, whose account it is for, and whether that person is a
/// professional. A caller that named any of them was answered as though they
/// had named none.
#[derive(Debug, Default, Clone)]
pub struct ExerciseStates {
    /// When a person entered it, where a person did.
    pub manual_order_time: String,
    /// The account it is taken for, where that is not the one connected.
    pub customer_account: String,
    /// Whether the person it is for is a professional.
    pub professional_customer: bool,
}

/// A bracket, placed as the one instruction the engine has for it: three
/// orders under three consecutive numbers, linked by the venue.
#[derive(Debug, Clone)]
pub struct Bracket {
    /// The contract all three are on.
    pub contract: crate::types::model::Contract,
    /// The entry's number; the exits are the next two.
    pub parent_id: OrderId,
    /// Whether the entry buys or sells.
    pub side: Side,
    /// How much.
    pub quantity: f64,
    /// Where the entry is, where the profit is taken, and where the loss is
    /// stopped.
    pub entry: f64,
    /// Where the profit-taking exit sits.
    pub take_profit: f64,
    /// Where the protective exit sits.
    pub stop_loss: f64,
}

/// A withdrawal the engine confirms in its place in the session's order, after
/// every record of the exchange it ends and ahead of anything after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retirement {
    /// `cancel_positions` or `req_account_updates(false, ..)`: the question's
    /// cancel, which a wrapper hears on `question_retired`.
    Question(crate::types::model::Question),
    /// `cancel_positions_multi`, under the caller's number.
    PositionsMulti(i64),
    /// `cancel_account_updates_multi`, under the caller's number.
    AccountUpdatesMulti(i64),
}

/// Which contract a news withdrawal is about.
///
/// The slot is what the engine routes on, and a caller that has one names it.
/// One that holds no slot names the contract by the venue's number, and the
/// engine resolves it against the mapping it already keeps. The surfaces send
/// neither: the engine asks for a request's headlines and withdraws them with
/// the last request that asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsSubject {
    /// The engine's own slot for the contract.
    Slot(InstrumentId),
    /// The venue's number for it, for a caller that holds no slot.
    Contract(i64),
}

/// Commands sent from the control plane to the hot loop via SPSC channel.
///
/// The submitting command is much larger than the rest, because it carries an
/// order's whole attribute block — which grew as the fields the venue reads
/// were filled in. Boxing it would even the variants out at the cost of an
/// allocation per order placed; the channel holds sixty-four of these, so the
/// size it saves is measured in tens of kilobytes and the cost is on the path
/// an order takes.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ControlCommand {
    /// Subscribe to market data for a contract.
    /// The contract's exchange and security type determine farm routing
    /// (empty = UsFarm default).
    /// `mode_9887` encodes per-request market-data mode via FIX field 9887:
    /// 0 = REALTIME (absent, default fan-out 264=442 BID_ASK + 264=443 LAST),
    /// 1 = DELAYED, 2 = FROZEN, 3 = DELAYED_FROZEN (the same 264=442 + 264=443 pair, each with 9887=N).
    ///
    /// The engine names the contract where the caller described it, registers
    /// it, and serves the request off the subscription the contract already
    /// has or asks for one; a record in the session's order says which slot
    /// the request is served on, or why it is not.
    Subscribe {
        /// The caller's number for the request.
        req_id: i64,
        /// The contract this names.
        contract: ContractRef,
        /// What narrows the lookup that names it, where the caller described
        /// the contract rather than numbering it. What identifies a contract
        /// and what filters for it are separate here, as they are on every
        /// other request that names one by description.
        filters: SecDefFilters,
        /// Which feed to serve the subscription from: live, delayed or frozen.
        mode_9887: i32,
        /// The delayed feed to request after a live refusal states it is available.
        delayed_mode: Option<i32>,
        /// Ask for the venue's chargeable one-shot snapshot rather than a
        /// stream. It is a request type of its own, asked for under the
        /// snapshot action and never with a feed named beside it, and the
        /// venue bills for each one.
        regulatory_snapshot: bool,
        /// Whether the caller asked for a snapshot: the first whole quote,
        /// then its end, then the request is withdrawn.
        snapshot: bool,
        /// The extra series the caller named, by the venue's number for each.
        ///
        /// The number a caller states is the venue's own: a series is asked
        /// for as a subscription of its own under that number, the way the
        /// option model, the trading status and the venue map already are.
        /// Empty where the caller named none.
        generic_ticks: Vec<u32>,
        /// The providers to ask for the contract's headlines, where the caller
        /// named the headlines series; none where it did not. Asked for once
        /// per contract, by the first request that wants them.
        news: Option<String>,
        /// What a spread scan looks for, where this request is one: it rides
        /// the scan's own request, so two scans of one contract each state
        /// their own.
        spread_scan: Option<String>,
        /// A calculation the request's model answers, kept on the slot the
        /// request is served on and answered where the venue states the model.
        calculation: Option<Box<Calculation>>,
    },
    /// Withdraw a market-data request, by the caller's number for it.
    ///
    /// The engine took the request before this, so it decides whether there
    /// is one to withdraw: a request still being named or held is forgotten,
    /// one served off a subscription somebody else is also watching leaves it
    /// up, and the last one on a contract takes the subscription down.
    CancelMktData {
        /// The caller's number for the request.
        req_id: i64,
    },
    /// Withdraw a calculation: the engine forgets the question, and the watch
    /// it opened for it where it opened one. A watch the caller opened itself
    /// stays.
    CancelCalculation {
        /// The caller's number for the calculation.
        req_id: i64,
    },
    /// Subscribe to tick-by-tick data via historical data connection.
    SubscribeTbt {
        /// The caller's number for the request.
        req_id: i64,
        /// The contract this names.
        contract: ContractRef,
        /// Which stream is wanted.
        tbt_type: TbtType,
        /// How many past ticks to send before the stream, where the caller
        /// asked for a prelude. Zero for none, which is the venue's default.
        number_of_ticks: u32,
        /// Whether the caller asked for changes that move only the size to be
        /// left out.
        ignore_size: bool,
    },
    /// Unsubscribe from tick-by-tick data.
    /// Withdraw one tick stream, named by the request that opened it.
    ///
    /// Named by the request and not by the contract: a contract can carry
    /// several streams, and withdrawing "the one on this contract" takes
    /// whichever was opened first and leaves the caller's own running.
    UnsubscribeTbt {
        /// The caller's number for the request.
        req_id: i64,
    },
    /// Subscribe to per-contract news ticks via CCP (264=292).
    SubscribeNews {
        /// The venue's id for the contract.
        con_id: i64,
        /// The contract's ticker.
        symbol: String,
        /// What kind of contract it is, as the venue states it. The
        /// subscription carries it and the venue routes on it.
        sec_type: String,
        /// Which news providers.
        providers: String,
    },
    /// Unsubscribe from per-contract news ticks.
    UnsubscribeNews {
        /// Which contract's headlines to stop.
        subject: NewsSubject,
    },
    /// Ask the venue to state the account's figures now.
    ///
    /// The session is subscribed to these at logon and the venue restates them
    /// on its own schedule, which is unhurried: a session that has just opened
    /// waits tens of seconds for its first set. This asks for them, which is
    /// what a caller subscribing to account updates expects to have happened
    /// by the time the call returns.
    RefreshAccount {
        /// Which account.
        account: String,
    },
    /// Subscribe to whole-account P&L via CCP (6040=142).
    SubscribePnl {
        /// The caller's number for the request.
        req_id: i64,
        /// Whether this is a single-position profit request.
        single: bool,
        /// Which account.
        account: String,
    },
    /// Ask for, or replace, a partition of the advisor's own configuration.
    ///
    /// `command` says which of asking, replacing or removing is meant;
    /// `partition` names which part — its groups, its allocation profiles, its
    /// models. A replacement carries the configuration as its own document.
    AdvisorConfig {
        /// The caller's number for a replacement, which the answer carries
        /// back. Minus one where the caller stated none, as asking for a
        /// partition does.
        req_id: i64,
        /// Which operation on the configuration.
        command: i32,
        /// Which part of it, as the venue names it.
        partition: String,
        /// The same part as the reference client numbers it, which is what the
        /// answer to a question about it is delivered under.
        fa_data_type: i32,
        /// The configuration itself, where one is being written.
        document: Option<String>,
    },
    /// Cancel P&L subscription.
    CancelPnl {
        /// The caller's number for the request.
        req_id: i64,
        /// Whether this is a single-position profit request.
        single: bool,
    },
    /// Update a strategy parameter.
    UpdateParam {
        /// Which setting.
        key: String,
        /// What to set it to.
        value: String,
    },
    /// Submit an order from external caller (bridge mode).
    Order(OrderRequest),
    /// Place an order, or restate one the venue is working, as the call
    /// stated it.
    Place(Box<Placement>),
    /// Withdraw an order, by its number: one the engine still holds is never
    /// sent, and one it sent is cancelled once the venue has named what the
    /// account is working.
    CancelOrder {
        /// The order's number.
        order_id: OrderId,
        /// What the withdrawal states about itself.
        stated: crate::types::model::OrderCancel,
    },
    /// Withdraw the working order the venue knows by this permanent number.
    CancelOrderByPermId {
        /// The venue's permanent number for it.
        perm_id: i64,
    },
    /// Withdraw every order: what the engine holds, and every order the
    /// account is working once the venue has named them.
    GlobalCancel {
        /// What each withdrawal states about itself.
        stated: crate::types::model::OrderCancel,
    },
    /// Exercise or lapse an option position.
    Exercise(Box<Exercise>),
    /// Place a bracket.
    Bracket(Box<Bracket>),
    /// Register an instrument from external caller (bridge mode).
    /// `identity` is what separates two contracts sharing a symbol: expiry,
    /// strike, right and multiplier, joined. Empty for a stock or a currency
    /// pair, which those four fields do not distinguish. An order names its
    /// contract by the instrument, so the instrument has to know this or the
    /// order goes out unable to say which strike or contract month it means.
    RegisterInstrument {
        /// The contract this names.
        contract: ContractRef,
        /// What separates this contract from others sharing its symbol.
        identity: String,
    },
    /// Answer a question from what the session holds, once the venue has
    /// stated it. Held in the engine until then.
    Ask(Ask),
    /// Withdraw a question, or a numbered request answered from what the
    /// session holds: one still held is never answered, and the withdrawal is
    /// confirmed where it stands in the session's order.
    Retire(Retirement),
    /// Request historical bar data via historical data connection.
    FetchHistorical {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The contract this names.
        contract: ContractRef,
        /// The end of the window asked for. Empty means now.
        end_date_time: String,
        /// How far back from that end the window reaches.
        duration: String,
        /// How long one bar covers.
        bar_size: String,
        /// Which series is wanted: `TRADES`, `MIDPOINT`, `BID`, `ASK`.
        what_to_show: String,
        /// Whether to count only regular trading hours.
        use_rth: bool,
        /// Whether the venue keeps sending once the window is answered.
        keep_up_to_date: bool,
        /// Whether a contract that has already expired is in scope, as the
        /// caller's contract states it.
        include_expired: bool,
        /// What tells two contracts on one underlying apart, for the
        /// lookup that names this one when the caller passed no id.
        filters: SecDefFilters,
    },
    /// Measure auth-connection round-trip time: sends a
    /// test request immediately; the sample lands in
    /// `SharedState::last_ccp_rtt` when the reply arrives.
    Ping,
    /// Cancel a historical data request.
    CancelHistorical {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Request head timestamp via historical data connection.
    FetchHeadTimestamp {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The contract this names.
        contract: ContractRef,
        /// Which series is wanted: `TRADES`, `MIDPOINT`, `BID`, `ASK`.
        what_to_show: String,
        /// Whether to count only regular trading hours.
        use_rth: bool,
        /// Whether an expired contract is meant, as the bar query states it.
        include_expired: bool,
        /// What tells two contracts on one underlying apart, for the
        /// lookup that names this one when the caller passed no id.
        filters: SecDefFilters,
    },
    /// Request contract details via auth connection.
    FetchContractDetails {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The contract this names.
        contract: ContractRef,
        /// Whether a contract that has already expired is in scope, as the
        /// caller's contract states it.
        include_expired: bool,
        /// What else narrows the lookup: an expiry, a strike, an identifier.
        filters: SecDefFilters,
    },
    /// Cancel a head timestamp request.
    CancelHeadTimestamp {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Search for matching symbols via auth connection.
    FetchMatchingSymbols {
        /// The caller's number for the request.
        req_id: u32,
        /// The text to match against.
        pattern: String,
    },
    /// Ask what corporate-event types the calendar carries.
    FetchCalendarMetaData {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Ask the calendar for events, under a filter or for one contract.
    FetchCalendarEvents {
        /// The caller's number for the request.
        req_id: u32,
        /// What is being asked of the calendar.
        query: Box<crate::types::CalendarQuery>,
    },
    /// Stop waiting on a calendar query. One message and one answer, so what
    /// is withdrawn is the answer.
    CancelCalendar {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Request the option chain of an underlying via auth connection.
    FetchOptionParams {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The contract's ticker, for a request that names one by description.
        symbol: String,
        /// Which venue's futures options to answer for, if any.
        fut_fop_exchange: String,
        /// What kind of contract the underlying is.
        underlying_sec_type: String,
        /// The venue's id for the underlying.
        underlying_con_id: i64,
    },
    /// Request available exchanges for market depth.
    FetchMktDepthExchanges,
    /// Ask the venue for the orders it has finished, rather than answering
    /// from what this session watched finish.
    ///
    /// The venue states them as ordinary execution reports, ending with the
    /// same sentinel the opening replay ends with, so what this carries is the
    /// window they are read in rather than any payload of its own.
    ///
    /// One question on the wire at a time, and each answered where its
    /// sentinel stands in the session's order: a second asked while the first
    /// is out waits its turn.
    FetchCompletedOrders {
        /// Whether only the orders an API placed are asked for.
        api_only: bool,
    },
    /// Request scanner parameter XML via historical data connection.
    FetchScannerParams,
    /// Subscribe to a scanner scan via historical data connection.
    SubscribeScanner {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The engine's own slot for the contract.
        instrument: String,
        /// Which market the scan runs over.
        location_code: String,
        /// Which scan it is.
        scan_code: String,
        /// The most rows wanted.
        max_items: u32,
        /// What else narrows the lookup: an expiry, a strike, an identifier.
        filters: Vec<(String, String)>,
    },
    /// Cancel a scanner subscription.
    CancelScanner {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Ask for a contract's corporate actions.
    ///
    /// Answered against the contract rather than the request, so the reply
    /// names which contract it is for and this id is only what the request
    /// went out under.
    FetchAdjustments {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The venue's id for the contract.
        con_id: u32,
        /// The contract's security type.
        sec_type: String,
        /// The venue it is listed on.
        exchange: String,
        /// The first day of the range asked for, as `YYYYMMDD`.
        start_date: String,
        /// The last day of it.
        end_date: String,
    },
    /// Request historical news via historical data connection.
    FetchHistoricalNews {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The venue's id for the contract.
        con_id: u32,
        /// Which news providers to ask, separated by the venue's
        /// separator.
        provider_codes: String,
        /// When the algorithm should begin.
        start_time: String,
        /// When it should stop.
        end_time: String,
        /// The most records wanted.
        max_results: i32,
    },
    /// Request a news article via historical data connection.
    FetchNewsArticle {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// Which news provider.
        provider_code: String,
        /// The venue's id for the article.
        article_id: String,
    },
    /// Request fundamental data via historical data connection.
    FetchFundamentalData {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The venue's id for the contract.
        con_id: u32,
        /// Which fundamental report.
        report_type: String,
    },
    /// Cancel fundamental data request.
    CancelFundamentalData {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Withdraw a historical news query the venue is still serving.
    CancelHistoricalNews {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Withdraw a corporate-actions query the venue is still serving.
    CancelCorporateActions {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Request histogram data via historical data connection.
    FetchHistogramData {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The venue's id for the contract.
        con_id: u32,
        /// What kind of contract it is, as the query has to describe it.
        sec_type: String,
        /// Where it is routed, likewise.
        exchange: String,
        /// Whether to count only regular trading hours.
        use_rth: bool,
        /// Over what window.
        period: String,
    },
    /// Cancel histogram data request.
    CancelHistogramData {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Request historical ticks via historical data connection.
    FetchHistoricalTicks {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The contract this names.
        contract: ContractRef,
        /// The start of the window asked for.
        start_date_time: String,
        /// The end of the window asked for. Empty means now.
        end_date_time: String,
        /// How many records are wanted.
        number_of_ticks: u32,
        /// Which series is wanted: `TRADES`, `MIDPOINT`, `BID`, `ASK`.
        what_to_show: String,
        /// Whether to count only regular trading hours.
        use_rth: bool,
        /// Whether a bid/ask change that moves only a size is left out.
        ignore_size: bool,
        /// Whether a contract that has already expired is in scope, as the
        /// caller's contract states it.
        include_expired: bool,
        /// What tells two contracts on one underlying apart, for the
        /// lookup that names this one when the caller passed no id.
        filters: SecDefFilters,
    },
    /// Subscribe to real-time 5-second bars via historical data connection.
    SubscribeRealTimeBar {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The contract this names.
        contract: ContractRef,
        /// Which series is wanted: `TRADES`, `MIDPOINT`, `BID`, `ASK`.
        what_to_show: String,
        /// Whether to count only regular trading hours.
        use_rth: bool,
        /// What tells two contracts on one underlying apart, for the
        /// lookup that names this one when the caller passed no id.
        filters: SecDefFilters,
    },
    /// Cancel real-time bar subscription.
    CancelRealTimeBar {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// Request historical schedule via historical data connection.
    FetchHistoricalSchedule {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The contract this names.
        contract: ContractRef,
        /// The end of the window asked for. Empty means now.
        end_date_time: String,
        /// How far back from that end the window reaches.
        duration: String,
        /// Whether to count only regular trading hours.
        use_rth: bool,
        /// What tells two contracts on one underlying apart, for the
        /// lookup that names this one when the caller passed no id.
        filters: SecDefFilters,
    },
    /// Subscribe to market depth (L2) for a contract.
    SubscribeDepth {
        /// The caller's number for the request this answers.
        req_id: u32,
        /// The contract this names.
        contract: ContractRef,
        /// How many levels of the book are wanted.
        num_rows: i32,
        /// Whether the book was asked for on no particular venue.
        is_smart_depth: bool,
        /// What tells two contracts on one underlying apart, for the
        /// lookup that names this one when the caller passed no id.
        filters: SecDefFilters,
    },
    /// Unsubscribe from market depth.
    UnsubscribeDepth {
        /// The caller's number for the request.
        req_id: u32,
    },
    /// End the session with the venue. Sent before [`ControlCommand::Shutdown`]
    /// by a caller that is disconnecting. A caller that only stops the engine
    /// and keeps its connections does not send it, because a logout ends the
    /// session those connections belong to.
    Logout,
    /// Graceful shutdown.
    Shutdown,
    /// Take both transports away, as a maintenance window does.
    ///
    /// For proving recovery, which cannot be proved by waiting for the venue
    /// to do it. Recovering once is not the same as recovering repeatedly:
    /// state kept from the connection that went, a subscription re-asked under
    /// an id the venue already holds, a host learned and then forgotten — none
    /// of those show up until the second time.
    ///
    /// Both, because a maintenance window takes both: the auth transport
    /// carries the orders and the data one carries the quotes, and a client
    /// that recovers one is still not trading.
    ForceDisconnect,
}
