//! What the venue has answered about contracts, history and news.

use super::*;
use super::record::{Queue, Stamps};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;
use crate::control::historical::{HistoricalResponse, HeadTimestampResponse};
use crate::control::contracts::{ContractDefinition, OptionChainScope, SymbolMatch};
use crate::control::scanner::ScannerResult;
use crate::control::news::NewsHeadline;
use crate::control::histogram::HistogramEntry;
use crate::control::contracts::MarketRule;
use crate::types::*;
use crate::types::model as api;

#[derive(Default)]
struct PresetState {
    list: Option<Vec<(String, String, String)>>,
    generation: u64,
    sequence: u64,
    values: HashMap<String, (u64, crate::control::order_presets::PresetValues)>,
    waiting: HashMap<String, (String, u64, std::sync::mpsc::SyncSender<crate::control::order_presets::PresetValues>)>,
}

/// A contract's corporate actions as the venue stated them.
type StatedActions = (
    crate::control::adjustments::AdjustedContract,
    Vec<crate::control::adjustments::Adjustment>,
);

/// What the venue states about a contract's issuer, by its id for the
/// contract and the series the statement arrived on.
/// What each contract's series have stated, by contract and then by series,
/// with the contracts in the order they were first heard of.
///
/// Nested rather than keyed by the pair, so the oldest contract can be dropped
/// without walking everything held.
type CompanyData = (
    std::collections::HashMap<u32, std::collections::HashMap<u32, Vec<(String, String)>>>,
    std::collections::VecDeque<u32>,
);

/// Which of the venue's records a number is held for.
///
/// A request number means one thing per kind of request: a caller may hold the
/// same number for bars and for a scan at once, and each kind is answered on a
/// queue of its own. Recorded by the number alone, the first reader to claim it
/// held it against every kind — so another kind's records were withheld from
/// the dispatch loop that was going to deliver them, and nothing else read them
/// either, because the reader holding the number reads somewhere else.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RecordKind {
    /// Bars, as the venue prints them.
    Bars,
    /// A contract's book: its entries, and the notice that ends one.
    Depth,
    /// A scan's rows.
    Scanner,
    /// The one answer to a question this client asked on its own account.
    Answer,
    /// The quotes of a stream `watch` opened, which runs until its caller
    /// withdraws it.
    Quotes,
}

impl RecordKind {
    /// Every kind, for the one queue that cannot say which it belongs to.
    const ALL: [Self; 5] = [Self::Bars, Self::Depth, Self::Scanner, Self::Answer, Self::Quotes];
}

/// A BBO exchange id and the code of the security type it is stated for.
type BboKey = (String, u16);

/// Each security type a map of venues is stated for, as the API spells it,
/// and the code the venue gives it.
const SEC_TYPE_CODES: [(&str, u16); 23] = [
    ("STK", 1), ("CFD", 2), ("OPT", 3), ("FOP", 4), ("WAR", 5), ("FUT", 6), ("FWD", 7),
    ("BAG", 8), ("CASH", 10), ("IND", 11), ("BOND", 12), ("BILL", 13), ("FIXED", 14),
    ("FUND", 15), ("SLB", 16), ("NEWS", 17), ("CMDTY", 18), ("BSK", 19), ("IOPT", 20),
    ("ICU", 21), ("ICS", 22), ("PHYSS", 23), ("CRYPTO", 24),
];

/// The code a security type is stated under beside a BBO exchange id, as the
/// API spells the type or as the wire does; nought for one with no code.
fn sec_type_code(sec_type: &str) -> u16 {
    let named = sec_type.trim().to_ascii_uppercase();
    let named = if named == "CS" { "STK" } else { named.as_str() };
    SEC_TYPE_CODES.iter().find(|(name, _)| *name == named).map_or(0, |(_, code)| *code)
}

/// The security type a code names, as the API spells it.
fn sec_type_named(code: u16) -> Option<&'static str> {
    SEC_TYPE_CODES.iter().find(|(_, stated)| *stated == code).map(|(name, _)| *name)
}

/// A BBO exchange as a caller names it: the id, and the security type's code
/// where the name carries one.
///
/// Read the way a gateway reads it: four to eight characters are the id and,
/// in the last four, a code in hexadecimal, kept to its lowest byte. A code
/// that names no type leaves the whole name as the id.
fn split_bbo_exchange(named: &str) -> (&str, Option<u16>) {
    if (4..=8).contains(&named.len())
        && named.is_char_boundary(named.len() - 4)
        && let Ok(code) = i32::from_str_radix(&named[named.len() - 4..], 16)
        && let Ok(code) = u16::try_from(code as i8)
        && sec_type_named(code).is_some()
    {
        return (&named[..named.len() - 4], Some(code));
    }
    (named, None)
}

/// How many contracts what the company series state is kept for, the one
/// heard of longest ago making way for the next. A bound of this client's own
/// on what it keeps past a subscription, so a scanner swept across thousands
/// of contracts is not kept whole for the life of the session.
// ponytail: the number the slot tables start at; raise it if a caller needs
// more contracts' company data held at once.
const COMPANY_DATA_HELD: usize = 4096;

/// Order types allowed for each contract and exchange.
type AttachedOrderTypes = HashMap<(u32, String), Vec<(String, i32)>>;

/// The contract selected for an attached parent's indicative quote.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum CachedAttachedQuote {
    Original,
    Proxy(api::Contract),
    Unavailable,
}

/// What attaching orders to one combination has settled: its confirmed
/// definition, terms, legs and price rule, and whether it may be restricted
/// to regular hours or placed natively.
#[derive(Clone, Debug, Default)]
pub(crate) struct AttachedCombo {
    pub definition: Option<ContractDefinition>,
    pub frame: Option<crate::types::AttachedComboFrame>,
    /// The confirmed legs and the factor their ratios were divided by.
    pub legs: Option<(Vec<crate::types::model::ComboLeg>, f64)>,
    pub price_rule: Option<MarketRule>,
    pub regular_hours: Option<bool>,
    pub native: Option<bool>,
}

/// What the logon states about orders for an amount of money rather than a
/// quantity, as it states it.
#[derive(Clone, Debug, Default)]
pub struct MoneyOrderTerms {
    /// The security types it takes them on, tag 8334: a list separated by
    /// commas.
    pub types: String,
    /// The order types it takes them on, tag 8351, listed the same way.
    pub order_types: String,
    /// Whether the account takes them, tag 8335.
    pub account: bool,
    /// Each product's default size and the precision of its currency, tag
    /// 6052: `type,product,size,most,precision` entries separated by
    /// semicolons.
    pub product_defaults: String,
}

/// Historical data, contract definitions, scanners, news archives, market rules,
/// contract cache.
pub struct ReferenceState {
    /// The numbers this session is itself reading records under, each against
    /// the kind of record it is reading — a number means one thing per kind of
    /// request, so holding one for bars holds it for bars alone.
    ///
    /// Held here rather than beside the counter that hands them out: the
    /// queues these guard belong to a session, and two sessions in one process
    /// count from the same number, so a set shared between them would let one
    /// release what the other is waiting on.
    ours_in_flight: Mutex<std::collections::HashSet<(RecordKind, i64)>>,
    pub(super) historical_taken: Queue<HistoricalTaken>,
    pub(super) historical_data: Queue<(u32, HistoricalResponse)>,
    pub(super) historical_over: Queue<u32>,
    pub(super) head_timestamps: Queue<(u32, HeadTimestampResponse)>,
    pub(super) contract_details: Queue<(u32, ContractDefinition)>,
    pub(super) contract_details_end: Queue<u32>,
    pub(super) matching_symbols: Queue<(u32, Vec<SymbolMatch>)>,
    /// Orders this session did not place, paired with the number it reaches
    /// them under. Drained to `order_bound` on each surface.
    pub(super) orders_bound: Queue<(i64, i64, i64)>,
    /// Which of those pairings have been stated, for as long as the session
    /// lasts. Kept apart from the queue above because the queue is emptied
    /// every time a caller reads it, and a set that forgets on being read
    /// cannot say whether something was said before.
    orders_bound_said: Mutex<std::collections::HashSet<i64>>,
    /// The calendar's answers, as the venue wrote them. Two shapes on one
    /// envelope — what event types exist, and the events themselves — kept
    /// apart so a caller waiting on one is not handed the other.
    pub(super) calendar_meta_data: Queue<(u32, String)>,
    pub(super) calendar_events: Queue<(u32, String)>,
    /// A whole option chain answer: the underlying's conId, and one entry per
    /// scope the venue listed. The list is what the dispatcher reports before
    /// ending the request, so an empty one still ends it.
    pub(super) option_params: Queue<(u32, i64, Vec<OptionChainScope>)>,
    pub(super) scanner_params: Queue<String>,
    /// A partition of the advisor's configuration the venue has stated, as the
    /// number it is asked for under and the document itself.
    pub(super) advisor_config: Queue<(i32, String)>,
    /// The end of a replacement, as the caller's number for it and what the
    /// venue said about it.
    pub(super) advisor_replaced: Queue<(i64, String)>,
    /// An advisor request the venue refused, as the caller's number for it,
    /// the code it is reported under, what the venue said and what it is
    /// about: a replacement under its number, or the question of a partition.
    pub(super) advisor_refused: Queue<(i64, i32, String, api::ErrorOrigin)>,
    pub(super) scanner_data: Queue<(u32, ScannerResult)>,
    pub(super) historical_news: Queue<(u32, Vec<NewsHeadline>, bool)>,
    pub(super) news_articles: Queue<(u32, i32, String)>,
    pub(super) fundamental_data: Queue<(u32, String)>,
    pub(super) histogram_data: Queue<(u32, Vec<HistogramEntry>)>,
    pub(super) historical_ticks: Queue<(u32, HistoricalTickData, String, bool)>,
    pub(super) historical_schedules: Queue<(u32, HistoricalScheduleResponse)>,
    /// A contract's corporate actions, against the contract they belong to.
    ///
    /// Keyed by the contract and not by a request, because that is how the
    /// venue sends them: the reply names the contract it is about, and one
    /// arrives per contract rather than per question asked.
    adjustments: Mutex<std::collections::HashMap<String, StatedActions>>,
    /// A slot for each request somebody is waiting on, holding its answer once
    /// one arrives.
    ///
    /// Separate from the record above because that one holds what arrived last
    /// for a contract, and two questions about one contract would otherwise
    /// share a slot — the second answer replacing the first before the first
    /// caller had looked.
    ///
    /// A slot exists only while somebody waits, so an answer to a request
    /// nobody is waiting on is dropped rather than kept. A call that asks and
    /// waits makes one and gives it up with a guard that runs on every way out
    /// of the wait. The guard matters more than it looks — the request can fail
    /// to go out at all, and a slot left by a call that never waited is one
    /// nothing reclaims. A request sent on its own makes one that
    /// `EClient::adjustments_for` gives up with the answer and
    /// `EClient::cancel_adjustments` gives up without it; one the venue refuses,
    /// or that is dropped with its connection, is given up by the engine.
    adjustments_by_request: Mutex<std::collections::HashMap<u32, Option<Vec<crate::control::adjustments::Adjustment>>>>,
    /// Errors surfaced for in-flight reference queries: (the number a reader
    /// holds it under, code, message, what it is about). Drained by the
    /// dispatcher and forwarded to `Wrapper::error_from`.
    pub(super) historical_errors: Queue<(u32, i32, String, api::ErrorOrigin)>,
    market_rules: Mutex<Vec<MarketRule>>,
    depth_exchanges_cache: Mutex<Option<Vec<DepthMktDataDescription>>>,
    depth_exchanges_pending: Mutex<bool>,
    /// The answers to those asks, each pushed when the table it states is
    /// read: at the ask where one is held, or as the table arrives.
    pub(super) depth_exchanges_answers: Queue<Vec<DepthMktDataDescription>>,
    /// Contract cache from CCP exec reports (con_id -> api::Contract).
    contract_cache: Mutex<HashMap<i64, api::Contract>>,
    /// The entries above that are the venue's own definitions, not seeds.
    defined_contracts: Mutex<std::collections::HashSet<i64>>,
    contract_definitions: Mutex<HashMap<(u32, String), ContractDefinition>>,
    attached_order_types: Mutex<AttachedOrderTypes>,
    combo_definitions: Mutex<HashMap<(String, String, String), ContractDefinition>>,
    combo_excluded_exchanges: Mutex<String>,
    aggregate_exchanges: Mutex<String>,
    smart_combo_contracts: Mutex<HashMap<String, i32>>,
    attached_quote_contracts: Mutex<HashMap<(u32, u8), CachedAttachedQuote>>,
    attached_combos: Mutex<HashMap<String, AttachedCombo>>,
    attached_combo_confirmations: Mutex<HashMap<String, std::sync::mpsc::SyncSender<Vec<u8>>>>,
    attached_combo_rules: Mutex<HashMap<String, crate::control::attached_combos::ComboRules>>,
    attached_combo_rules_pending: Mutex<std::collections::HashSet<String>>,
    /// Which venue each bit of a quote's exchange mask refers to, per BBO
    /// exchange and security type: every key an acknowledgement or a map has
    /// named this session, and the map the venue stated for it — `None` where
    /// one is coming and has not arrived.
    smart_component_maps: Mutex<HashMap<BboKey, Option<Vec<crate::types::SmartComponent>>>>,
    /// The BBO exchange and security type each subscribed contract's
    /// acknowledgement named.
    bbo_keys: Mutex<HashMap<crate::types::InstrumentId, BboKey>>,
    /// Requests for a map that had not arrived when they were made: the
    /// request, the BBO exchange it named and when it was asked.
    smart_component_asks: Mutex<Vec<(i64, String, std::time::Instant)>>,
    /// Whether the venue states each subscribed contract's snapshot as
    /// chargeable, as its last acknowledgement to say anything did.
    snapshot_permissions: Mutex<HashMap<crate::types::InstrumentId, u32>>,
    /// Gateway-local init data (populated during connection, read-only after).
    news_providers: Mutex<Vec<crate::types::NewsProvider>>,
    soft_dollar_tiers: Mutex<Vec<crate::types::SoftDollarTier>>,
    family_codes: Mutex<Vec<crate::types::FamilyCode>>,
    white_branding_id: Mutex<String>,
    /// Session ID surfaced to webapp REST clients as `x-ccp-session-id`.
    ccp_session_id: Mutex<String>,
    /// Logical-name → host URL map pushed by the venue during logon.
    misc_urls: Mutex<HashMap<String, String>>,
    /// Another session already on this account at connect: address, login time,
    /// and whether this one is held to reading only.
    competing_session: Mutex<Option<(String, String, bool)>>,
    /// Why this session ended, once it has ended for good.
    session_over: Mutex<Option<&'static str>>,
    /// Why the trading connection stopped for good, where it has.
    trading_over: Mutex<Option<&'static str>>,
    /// Security type → the order types the venue permits for it, from logon tag 6652.
    order_permissions: Mutex<HashMap<String, Vec<String>>>,
    /// Feature tokens the venue enables for this account, from logon tag 6542
    /// and from the account configuration that follows it.
    enabled_features: Mutex<Vec<String>>,
    /// The accounts the logon names as the login's own, and whether the login
    /// is an advisor's (logon tag 6108).
    login: Mutex<(Vec<String>, bool)>,
    /// Whether the logon named accounts `AllNonProp` leaves out.
    all_non_prop_leaves_out: AtomicBool,
    /// Whether the logon asks for the venue's refusal of an order stated on a
    /// status report to be told (tag 6130).
    refusals_told: AtomicBool,
    /// The broker the logon names the login as being with (tag 6053).
    broker: Mutex<String>,
    /// What the logon names the broker for short (tag 6322), and the product
    /// it names this session as (tag 6054), each empty where it names none.
    names: Mutex<(String, String)>,
    /// What the logon states about orders for an amount of money.
    money_orders: Mutex<MoneyOrderTerms>,
    /// The part of a unit a size is shown to on a contract that states no
    /// least size of its own (tag 8079).
    size_fraction: Mutex<String>,
    /// Where the executions the session opened with start, in unix seconds.
    executions_held_from: Mutex<Option<i64>>,
    /// Whether the venue granted the older spelling of Nasdaq. Settled when
    /// the grants are, because a contract definition is parsed under it and a
    /// lock and a scan per definition is not what that path is for.
    island_granted: AtomicBool,
    /// Which algorithms the venue offers, by provider and security type.
    algorithms: Mutex<HashMap<String, Vec<String>>>,
    /// The order presets the account holds, by the key the venue names each
    /// set under, with its attributes and last-change time.
    order_presets: Mutex<PresetState>,
    /// What a derivative's underlying is, by the venue's id for the
    /// derivative.
    ///
    /// Stated on the definition, and held because the schedule an option is
    /// priced against belongs to its underlying rather than to the option.
    under_con_ids: Mutex<std::collections::HashMap<u32, u32>>,
    /// What each contract pays out, by the venue's id for it.
    ///
    /// Kept rather than drained: a schedule is a fact about the contract that
    /// every option on it is priced against, and the option that needs it next
    /// is not the one whose question fetched it.
    dividend_schedules: Mutex<std::collections::HashMap<u32, crate::control::dividends::Schedule>>,
    /// The rates the venue states for each currency, kept as the schedules
    /// above are: each is asked for once and every option priced in the
    /// currency reads it.
    currency_rates: Mutex<std::collections::HashMap<String, Vec<(String, f64)>>>,
    /// The sessions the venue states, by the key it joins them to a
    /// definition on, and that key for each contract whose key is known: every
    /// contract on one key has its sessions, which are kept once for the key,
    /// as a gateway keeps them.
    contract_schedules: Mutex<(HashMap<u32, String>, HashMap<String, crate::control::contracts::ContractSchedule>)>,
    /// What the venue states about the issuer, by its id for the contract and
    /// the series the statement arrived on.
    ///
    /// Kept rather than drained, as the schedules above are: these are facts
    /// about the contract and the caller that reads one is not the arrival
    /// that brought it. Each series is replaced whole when it is restated,
    /// because that is how the venue states it — one message carries the
    /// series' whole set of pairs.
    company_data: Mutex<CompanyData>,
    /// The scan a caller has asked for on an underlying, as the venue takes it.
    spread_scans: Mutex<std::collections::HashMap<u32, String>>,
}

impl ReferenceState {


    /// An empty one, stamping from its own counter.
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::stamping(&Stamps::default())
    }

    /// An empty one, stamping from the session's counter.
    pub(super) fn stamping(stamps: &Stamps) -> Self {
        Self {
            ours_in_flight: Mutex::new(Default::default()),
            historical_taken: Queue::new(stamps),
            historical_data: Queue::with_capacity(stamps, 16),
            historical_over: Queue::new(stamps),
            head_timestamps: Queue::with_capacity(stamps, 8),
            contract_details: Queue::with_capacity(stamps, 16),
            contract_details_end: Queue::with_capacity(stamps, 8),
            matching_symbols: Queue::with_capacity(stamps, 8),
            orders_bound: Queue::new(stamps),
            orders_bound_said: Mutex::new(std::collections::HashSet::new()),
            calendar_meta_data: Queue::new(stamps),
            calendar_events: Queue::new(stamps),
            option_params: Queue::with_capacity(stamps, 4),
            scanner_params: Queue::new(stamps),
            advisor_config: Queue::new(stamps),
            advisor_replaced: Queue::new(stamps),
            advisor_refused: Queue::new(stamps),
            scanner_data: Queue::with_capacity(stamps, 8),
            historical_news: Queue::with_capacity(stamps, 8),
            news_articles: Queue::with_capacity(stamps, 8),
            fundamental_data: Queue::with_capacity(stamps, 4),
            histogram_data: Queue::with_capacity(stamps, 4),
            historical_ticks: Queue::with_capacity(stamps, 4),
            historical_schedules: Queue::with_capacity(stamps, 4),
            adjustments: Mutex::new(std::collections::HashMap::new()),
            adjustments_by_request: Mutex::new(std::collections::HashMap::new()),
            historical_errors: Queue::with_capacity(stamps, 4),
            market_rules: Mutex::new(Vec::new()),
            depth_exchanges_cache: Mutex::new(None),
            depth_exchanges_pending: Mutex::new(false),
            depth_exchanges_answers: Queue::new(stamps),
            contract_cache: Mutex::new(HashMap::new()),
            defined_contracts: Mutex::new(std::collections::HashSet::new()),
            contract_definitions: Mutex::new(HashMap::new()),
            attached_order_types: Mutex::new(HashMap::new()),
            combo_definitions: Mutex::new(HashMap::new()),
            combo_excluded_exchanges: Mutex::new(String::new()),
            aggregate_exchanges: Mutex::new(String::new()),
            smart_combo_contracts: Mutex::new(crate::control::aggregate_exchanges::smart_combo_ids(0, None)),
            attached_quote_contracts: Mutex::new(HashMap::new()),
            attached_combos: Mutex::new(HashMap::new()),
            attached_combo_confirmations: Mutex::new(HashMap::new()),
            attached_combo_rules: Mutex::new(HashMap::new()),
            attached_combo_rules_pending: Mutex::new(std::collections::HashSet::new()),
            smart_component_maps: Mutex::new(HashMap::new()),
            bbo_keys: Mutex::new(HashMap::new()),
            smart_component_asks: Mutex::new(Vec::new()),
            snapshot_permissions: Mutex::new(HashMap::new()),
            news_providers: Mutex::new(Vec::new()),
            soft_dollar_tiers: Mutex::new(Vec::new()),
            family_codes: Mutex::new(Vec::new()),
            white_branding_id: Mutex::new(String::new()),
            ccp_session_id: Mutex::new(String::new()),
            misc_urls: Mutex::new(HashMap::new()),
            competing_session: Mutex::new(None),
            session_over: Mutex::new(None),
            trading_over: Mutex::new(None),
            order_permissions: Mutex::new(HashMap::new()),
            enabled_features: Mutex::new(Vec::new()),
            login: Mutex::new((Vec::new(), false)),
            all_non_prop_leaves_out: AtomicBool::new(false),
            refusals_told: AtomicBool::new(false),
            broker: Mutex::new(String::new()),
            names: Mutex::new((String::new(), String::new())),
            money_orders: Mutex::new(MoneyOrderTerms::default()),
            size_fraction: Mutex::new(String::new()),
            executions_held_from: Mutex::new(None),
            island_granted: AtomicBool::new(false),
            algorithms: Mutex::new(HashMap::new()),
            order_presets: Mutex::new(PresetState { sequence: 2, ..Default::default() }),
            under_con_ids: Mutex::new(std::collections::HashMap::new()),
            dividend_schedules: Mutex::new(std::collections::HashMap::new()),
            currency_rates: Mutex::new(std::collections::HashMap::new()),
            contract_schedules: Mutex::new((HashMap::new(), HashMap::new())),
            company_data: Mutex::new(Default::default()),
            spread_scans: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Take every historical data waiting, leaving none.
    pub fn drain_historical_data(&self) -> Vec<(u32, HistoricalResponse)> {
        self.historical_data.drain()
    }

    /// Take every head timestamps waiting, leaving none.
    pub fn drain_head_timestamps(&self) -> Vec<(u32, HeadTimestampResponse)> {
        self.head_timestamps.drain()
    }

    /// Take every contract details waiting, leaving none.
    pub fn drain_contract_details(&self) -> Vec<(u32, ContractDefinition)> {
        self.contract_details.drain()
    }

    /// The definitions a dispatch loop should deliver, leaving an answering
    /// call's own where that call will find them.
    pub fn drain_contract_details_for_dispatch(&self) -> Vec<(u32, ContractDefinition)> {
        self.drain_dispatchable(&self.contract_details)
    }



    /// Take every calendar meta data a dispatch loop should deliver, leaving behind
    /// what a waiting answering call will take.
    pub fn drain_calendar_meta_data_for_dispatch(&self) -> Vec<(u32, String)> {
        self.drain_dispatchable(&self.calendar_meta_data)
    }

    /// Take every calendar events a dispatch loop should deliver, leaving behind
    /// what a waiting answering call will take.
    pub fn drain_calendar_events_for_dispatch(&self) -> Vec<(u32, String)> {
        self.drain_dispatchable(&self.calendar_events)
    }



    /// What a refusal against no request at all is carried as.
    ///
    /// The queue holds the request id unsigned, and a refusal that answers no
    /// request has none to hold — the reference client states those under -1,
    /// and reporting one under 0 puts it on a caller's own request instead.
    /// Read back by [`request_id_reported`].
    pub const NO_REQUEST: u32 = u32::MAX;

    /// The request id to report an error under, as the caller numbers them.
    pub fn request_id_reported(stored: u32) -> i64 {
        if stored == Self::NO_REQUEST { -1 } else { i64::from(stored) }
    }

    /// Take the refusals a dispatch loop should deliver, leaving behind those a
    /// reader is going to take by id. `mine` says which ids those are, because
    /// the two dispatch loops answer their own questions differently.
    pub fn drain_historical_errors_for_dispatch(
        &self, mine: impl Fn(u32) -> bool,
    ) -> Vec<(u32, i32, String)> {
        self.historical_errors
            .take_if(|e| !mine(e.0))
            .into_iter()
            .map(|(id, code, msg, _)| (id, code, msg))
            .collect()
    }

    /// The first id this client's own answering calls ask under.
    ///
    /// A dispatch loop tells these apart from a caller's own requests and
    /// leaves them where the waiting call will find them, so the band has to
    /// sit above every id a caller can present. "Far above what a caller is
    /// likely to use" was not that: a session numbers its orders from the
    /// account's own counter so a restart does not reissue an id the account
    /// has already used, which puts a caller's ids near the epoch in seconds —
    /// past `0x6A00_0000` today and climbing, where the old base was
    /// `0x3000_0000`. Every request a program made through that counter was
    /// read as this client's own and its answer withheld, so the request never
    /// completed and nothing said why.
    ///
    /// Above that counter, and below [`ENGINE_ID_BASE`], where the engine's own
    /// requests (cache auto-fetch, scanner enrichment) start.
    pub const ASK_ID_BASE: u32 = 0xC000_0000;

    /// Whether a request id belongs to one of this client's answering calls.
    ///
    /// The band, not everything above its floor: the engine's own requests are
    /// nobody's answering call, and their replies are taken by the engine
    /// rather than left for a call that is going to want them.
    pub fn is_ask_id(req_id: u32) -> bool {
        (Self::ASK_ID_BASE..ENGINE_ID_BASE).contains(&req_id)
    }

    /// Record that this session is reading records of one kind under this id.
    pub fn note_ours(&self, kind: RecordKind, req_id: i64) {
        self.ours_in_flight.lock().unwrap().insert((kind, req_id));
    }

    /// Stop holding an id, whether the question was answered or given up on.
    pub fn forget_ours(&self, kind: RecordKind, req_id: i64) {
        self.ours_in_flight.lock().unwrap().remove(&(kind, req_id));
    }

    /// Whether records of this kind under this id belong to a reader of this
    /// session's.
    ///
    /// Read from what was recorded when the id was handed out, so a caller's
    /// own number — however large, and whatever counter it came from — is
    /// never mistaken for one of these. Asked one kind at a time, because a
    /// number means one thing per kind: a stream reading bars under 7 says
    /// nothing about the scan a caller is running under 7.
    pub fn is_ours(&self, kind: RecordKind, req_id: i64) -> bool {
        self.ours_in_flight.lock().unwrap().contains(&(kind, req_id))
    }

    /// Whether any reader of this session's holds this id, whatever it holds
    /// it for.
    ///
    /// For the refusals, which share one queue keyed by the number alone: the
    /// calendar, the contract lookups, the trading connection and the scanner
    /// all write to it, so alone among the queues it cannot say which kind of
    /// request failed. Until it can, the most that may be asked of a number
    /// there is whether anybody is reading under it at all.
    pub fn held_under_any_kind(&self, req_id: i64) -> bool {
        let held = self.ours_in_flight.lock().unwrap();
        RecordKind::ALL.iter().any(|kind| held.contains(&(*kind, req_id)))
    }

    /// Whether a refusal under this id belongs to a reader that takes it out
    /// of the queue itself, so a dispatch loop has to leave it there.
    ///
    /// The streams the answering shape hands back read by id and hold no turn,
    /// because a stream outlives any one read of the session. This client's own
    /// answering calls are recorded the same way and are not that: they pump a
    /// dispatch loop themselves and receive through it, so their answers must go
    /// on being delivered to it or they are withheld from the call waiting for
    /// them. The band the second are numbered in is what tells the two apart.
    ///
    /// For the refusals alone. Every other queue names one kind of record and
    /// asks about that kind; see [`held_under_any_kind`](Self::held_under_any_kind).
    pub fn left_for_its_reader(&self, req_id: u32) -> bool {
        !Self::is_ask_id(req_id) && self.held_under_any_kind(i64::from(req_id))
    }

    /// Drain what a dispatch loop should deliver, leaving behind what a waiting
    /// answering call is going to take.
    ///
    /// Only for a dispatch loop whose answering calls take their replies out of
    /// these queues by id. A dispatch loop that *is* how its answering calls
    /// receive must use the plain drain, or it withholds from itself — which is
    /// what happened, and the tests of the day could not see it: they filled
    /// the queues by hand, with ids of their own choosing, so the band was
    /// never the one a session hands out.
    pub fn drain_dispatchable<T>(&self, q: &Queue<(u32, T)>) -> Vec<(u32, T)> {
        q.take_if(|(id, _)| !self.is_ours(RecordKind::Answer, *id as i64))
    }

    /// Take the one answer belonging to a request, leaving the rest.
    fn take_one<T>(q: &Queue<(u32, T)>, req_id: u32) -> Option<T> {
        q.take_first(|(id, _)| *id == req_id).map(|(_, item)| item)
    }

    // Withdrawing a request stops the venue sending more; it does not unsend
    // what already arrived and is waiting to be read. Left there, the next
    // request under the same number is answered with the previous one's, and
    // where one of those said it was the last, terminated before its own
    // answer arrives.
    //
    // One per kind, not one for all of them. A request number means one thing
    // per kind of request, so a caller may hold the same number for bars and
    // for a head timestamp at once — and withdrawing either must not take the
    // other's answers with it.

    // The refusals are not purged with them, and cannot be as things stand.
    // They share one queue keyed by request number alone — written by the
    // calendar, the contract lookups, the trading connection and the scanner
    // as well — so throwing away "the refusals under 7" throws away another
    // kind's reason for failing, which is the very thing the paragraph above
    // forbids. A reason left standing is read back as the next request's, and
    // that is the lesser of the two until the queue carries which kind it
    // belongs to.

    /// Throw away bars still queued under a request.
    pub fn purge_historical_for(&self, req_id: u32) {
        self.historical_taken.retain(|taken| taken.req_id != req_id);
        self.historical_data.retain(|(id, _)| *id != req_id);
    }

    /// Throw away a trading schedule still queued under a request.
    pub fn purge_historical_schedule_for(&self, req_id: u32) {
        self.historical_schedules.retain(|(id, _)| *id != req_id);
    }

    /// Throw away a head timestamp still queued under a request.
    pub fn purge_head_timestamp_for(&self, req_id: u32) {
        self.head_timestamps.retain(|(id, _)| *id != req_id);
    }

    /// Throw away calendar answers still queued under a request.
    pub fn purge_calendar_for(&self, req_id: u32) -> bool {
        let mut meta = self.calendar_meta_data.lock();
        let mut events = self.calendar_events.lock();
        let before = meta.len() + events.len();
        meta.retain(|(_, (id, _))| *id != req_id);
        events.retain(|(_, (id, _))| *id != req_id);
        meta.len() + events.len() != before
    }

    /// Throw away a report still queued under a request.
    pub fn purge_fundamental_for(&self, req_id: u32) {
        self.fundamental_data.retain(|(id, _)| *id != req_id);
    }

    /// Throw away headlines still queued under a request.
    pub fn purge_historical_news_for(&self, req_id: u32) {
        self.historical_news.retain(|(id, ..)| *id != req_id);
    }

    /// Throw away a histogram still queued under a request.
    pub fn purge_histogram_for(&self, req_id: u32) {
        self.histogram_data.retain(|(id, _)| *id != req_id);
    }

    /// Bars answering one request. The venue may answer in several parts, so
    /// this takes every part waiting and the caller stops on the one that says
    /// it is the last.
    pub fn take_historical_for(&self, req_id: u32) -> Vec<HistoricalResponse> {
        self.historical_data.take_if(|(id, _)| *id == req_id)
            .into_iter().map(|(_, response)| response).collect()
    }

    /// Take the head timestamp answering one request, leaving the rest.
    pub fn take_head_timestamp_for(&self, req_id: u32) -> Option<HeadTimestampResponse> {
        Self::take_one(&self.head_timestamps, req_id)
    }

    /// Take the matching symbols answering one request, leaving the rest.
    pub fn take_matching_symbols_for(&self, req_id: u32) -> Option<Vec<SymbolMatch>> {
        Self::take_one(&self.matching_symbols, req_id)
    }

    /// The option chains answered for one request.
    pub fn take_option_params_for(&self, req_id: u32) -> Option<(i64, Vec<OptionChainScope>)> {
        let (_, underlying, scopes) = self.option_params.take_first(|(id, ..)| *id == req_id)?;
        Some((underlying, scopes))
    }

    /// Take the histogram answering one request, leaving the rest.
    pub fn take_histogram_for(&self, req_id: u32) -> Option<Vec<HistogramEntry>> {
        Self::take_one(&self.histogram_data, req_id)
    }

    /// Take the fundamental answering one request, leaving the rest.
    pub fn take_fundamental_for(&self, req_id: u32) -> Option<String> {
        Self::take_one(&self.fundamental_data, req_id)
    }

    /// What the venue last stated for a contract, over whatever range was last
    /// asked about.
    ///
    /// Not every action of the contract's life: each question replaces what the
    /// one before it left, and a narrower question leaves a narrower answer.
    /// Read rather than taken, so a caller adjusting one series does not spend
    /// it for the next.
    pub fn adjustments_for(&self, con_id: &str)
        -> Option<(crate::control::adjustments::AdjustedContract, Vec<crate::control::adjustments::Adjustment>)>
    {
        self.adjustments.lock().unwrap().get(con_id).cloned()
    }

    /// Say that an answer to this request is going to be waited for.
    ///
    /// Nothing is filed for a request nobody said they would wait on. Without
    /// that, every answer to every request leaves a slot behind: an answer
    /// arriving after its asker gave up recreates the slot it had just
    /// removed, and grows the map for as long as the session lasts.
    ///
    /// Paired with [`stop_waiting_for_adjustments`](Self::stop_waiting_for_adjustments),
    /// which every path out of a wait goes through, and which a request sent
    /// on its own reaches once its answer is taken, once it is withdrawn, and
    /// once the engine gives the query up.
    pub fn expect_adjustments(&self, req_id: u32) {
        self.adjustments_by_request.lock().unwrap().insert(req_id, None);
    }

    /// Give up on an answer, whether or not one arrived.
    pub fn stop_waiting_for_adjustments(&self, req_id: u32) {
        self.adjustments_by_request.lock().unwrap().remove(&req_id);
    }

    /// The answer to one request, if it has arrived.
    ///
    /// Kept apart from the contract's own record, which holds whatever arrived
    /// last and is what a caller reads to ask "what does this session know
    /// about this contract". A caller waiting on an answer is asking something
    /// narrower — "what answered the question I asked" — and the two must not
    /// share a slot: an answer filed for one request and then overwritten by a
    /// late answer to another is an answer that arrived and was lost, and the
    /// caller waiting on it is told nothing came.
    pub fn take_adjustments_answering(&self, req_id: u32)
        -> Option<Vec<crate::control::adjustments::Adjustment>>
    {
        let mut waiting = self.adjustments_by_request.lock().unwrap();
        match waiting.get_mut(&req_id) {
            Some(slot @ Some(_)) => slot.take(),
            _ => None,
        }
    }

    /// Forget what a contract's actions were, so the next answer is the next
    /// answer.
    ///
    /// This record is what a caller reads to ask what the session knows about
    /// a contract, and it holds whatever arrived last. Clearing it before
    /// asking is what stops that reader stating a previous question's answer
    /// once a newer one has been asked for.
    ///
    /// It is not what makes a wait its own. A caller waiting on an answer
    /// watches the slot its request made, which no other question can fill.
    pub fn forget_adjustments(&self, con_id: &str) {
        self.adjustments.lock().unwrap().remove(con_id);
    }

    #[doc(hidden)] pub fn note_adjustments(
        &self,
        contract: crate::control::adjustments::AdjustedContract,
        actions: Vec<crate::control::adjustments::Adjustment>,
        answering: u32,
    ) {
        if contract.con_id.is_empty() {
            return;
        }
        // Filed only where somebody said they would wait for it. An answer to
        // a request nobody is waiting on has nowhere to go, and giving it one
        // is how this map would grow for the life of the session.
        if let Some(slot) = self.adjustments_by_request.lock().unwrap().get_mut(&answering) {
            *slot = Some(actions.clone());
        }
        self.adjustments.lock().unwrap()
            .insert(contract.con_id.clone(), (contract, actions));
    }

    /// Take the historical schedule answering one request, leaving the rest.
    pub fn take_historical_schedule_for(&self, req_id: u32) -> Option<HistoricalScheduleResponse> {
        Self::take_one(&self.historical_schedules, req_id)
    }

    /// Take only the definitions answering one request, leaving every other
    /// request's alone.
    ///
    /// The plain drain empties the queue for whoever calls it first. A caller
    /// asking one question needs its own answer without swallowing the answers
    /// belonging to a dispatch loop running beside it.
    pub fn take_contract_details_for(&self, req_id: u32) -> Vec<ContractDefinition> {
        self.contract_details.take_if(|(id, _)| *id == req_id)
            .into_iter().map(|(_, def)| def).collect()
    }

    /// Whether the venue has said it has no more to say about one request.
    pub fn take_contract_details_end_for(&self, req_id: u32) -> bool {
        !self.contract_details_end.take_if(|id| *id == req_id).is_empty()
    }

    pub fn take_error_for(&self, req_id: u32) -> Option<(i32, String)> {
        let (_, code, msg, _) = self.historical_errors.take_first(|(id, ..)| *id == req_id)?;
        Some((code, msg))
    }

    /// Take the book reset queued under `req_id`, if one is, and nothing else.
    ///
    /// A stream reads it apart from the refusals so it can be noted before
    /// the level that follows it is handed over, while a refusal still ends
    /// the stream only after the levels that preceded it.
    pub fn take_reset_for(&self, req_id: u32) -> Option<(i32, String)> {
        let (_, code, msg, _) = self.historical_errors.take_first(|(id, code, ..)| {
            *id == req_id && *code == crate::error_codes::DEPTH_BOOK_RESET
        })?;
        Some((code, msg))
    }

    /// Take every contract details end waiting, leaving none.
    pub fn drain_contract_details_end(&self) -> Vec<u32> {
        self.contract_details_end.drain()
    }

    /// Take every calendar meta data waiting, leaving none.
    pub fn drain_calendar_meta_data(&self) -> Vec<(u32, String)> {
        self.calendar_meta_data.drain()
    }



    /// The calendar's answer to one request, of either shape, leaving the rest.
    pub fn take_calendar_for(&self, req_id: u32) -> Option<String> {
        Self::take_one(&self.calendar_meta_data, req_id)
            .or_else(|| Self::take_one(&self.calendar_events, req_id))
    }

    /// Take every matching symbols waiting, leaving none.
    pub fn drain_matching_symbols(&self) -> Vec<(u32, Vec<SymbolMatch>)> {
        self.matching_symbols.drain()
    }

    /// Take every option params waiting, leaving none.
    pub fn drain_option_params(&self) -> Vec<(u32, i64, Vec<OptionChainScope>)> {
        self.option_params.drain()
    }



    /// Take every scanner params waiting, leaving none.
    pub fn drain_scanner_params(&self) -> Vec<String> {
        self.scanner_params.drain()
    }

    /// Take every advisor partition waiting, leaving none.
    pub fn drain_advisor_config(&self) -> Vec<(i32, String)> {
        self.advisor_config.drain()
    }

    /// Take every finished replacement waiting, leaving none.
    pub fn drain_advisor_replaced(&self) -> Vec<(i64, String)> {
        self.advisor_replaced.drain()
    }

    /// Take every refused replacement waiting, leaving none.
    pub fn drain_advisor_refused(&self) -> Vec<(i64, i32, String)> {
        self.advisor_refused.drain().into_iter().map(|(id, code, text, _)| (id, code, text)).collect()
    }

    /// Take every scanner data waiting, leaving none.
    pub fn drain_scanner_data(&self) -> Vec<(u32, ScannerResult)> {
        self.scanner_data.drain()
    }

    /// Take the scan results a dispatch loop should deliver, leaving behind
    /// those a stream is going to read by id.
    ///
    /// A scan answers repeatedly for as long as it runs, and the stream that
    /// opened it reads its batches out of this queue. Drained whole beside one,
    /// a batch went to a callback and the stream waited for the next.
    pub fn drain_scanner_data_for_dispatch(
        &self, mine: impl Fn(u32) -> bool,
    ) -> Vec<(u32, ScannerResult)> {
        // Partitioned rather than removed one at a time: each `remove` shifts
        // the tail, so a pass over a queue that has grown costs the square of
        // it — under the lock the hot loop pushes into, on exactly the path a
        // stalled reader takes when it resumes. The `take_*_for` siblings were
        // already changed for this; these were not.
        self.scanner_data.take_if(|e| !mine(e.0))
    }

    /// The scan results arrived under one request, leaving the rest.
    pub fn take_scanner_data_for(&self, req_id: u32) -> Vec<ScannerResult> {
        self.scanner_data.take_if(|(id, _)| *id == req_id)
            .into_iter().map(|(_, result)| result).collect()
    }

    /// Take every historical news waiting, leaving none.
    pub fn drain_historical_news(&self) -> Vec<(u32, Vec<NewsHeadline>, bool)> {
        self.historical_news.drain()
    }

    /// The headlines answering one request, leaving anything a dispatch loop
    /// is going to deliver where it is.
    pub fn take_historical_news_for(&self, req_id: u32) -> Option<(Vec<NewsHeadline>, bool)> {
        let (_, headlines, has_more) = self.historical_news.take_first(|(id, ..)| *id == req_id)?;
        Some((headlines, has_more))
    }



    /// Take every news articles waiting, leaving none.
    pub fn drain_news_articles(&self) -> Vec<(u32, i32, String)> {
        self.news_articles.drain()
    }

    /// Take every fundamental data waiting, leaving none.
    pub fn drain_fundamental_data(&self) -> Vec<(u32, String)> {
        self.fundamental_data.drain()
    }

    /// Take every histogram data waiting, leaving none.
    pub fn drain_histogram_data(&self) -> Vec<(u32, Vec<HistogramEntry>)> {
        self.histogram_data.drain()
    }

    /// Take every historical ticks waiting, leaving none.
    pub fn drain_historical_ticks(&self) -> Vec<(u32, HistoricalTickData, String, bool)> {
        self.historical_ticks.drain()
    }

    /// Take every historical schedules waiting, leaving none.
    pub fn drain_historical_schedules(&self) -> Vec<(u32, HistoricalScheduleResponse)> {
        self.historical_schedules.drain()
    }

    /// Take every historical errors waiting, leaving none.
    pub fn drain_historical_errors(&self) -> Vec<(u32, i32, String)> {
        self.historical_errors.drain().into_iter().map(|(id, code, msg, _)| (id, code, msg)).collect()
    }

    /// Get cached market rules.
    pub fn market_rules(&self) -> Vec<MarketRule> {
        self.market_rules.lock().unwrap().clone()
    }

    /// Get a market rule by ID.
    pub fn market_rule(&self, rule_id: i32) -> Option<MarketRule> {
        self.market_rules.lock().unwrap().iter().find(|r| r.rule_id == rule_id).cloned()
    }

    /// Get cached contract by con_id.
    pub fn get_contract(&self, con_id: i64) -> Option<api::Contract> {
        self.contract_cache.lock().unwrap().get(&con_id).cloned()
    }

    pub(crate) fn contract_definition(&self, con_id: u32, exchange: &str) -> Option<ContractDefinition> {
        let exchange = crate::control::contracts::exchange_to_fix(exchange);
        let definitions = self.contract_definitions.lock().unwrap();
        definitions.get(&(con_id, exchange.into()))
            .or_else(|| definitions.get(&(con_id, String::new()))).cloned()
    }

    pub(crate) fn contract_definition_exact(&self, con_id: u32, exchange: &str) -> Option<ContractDefinition> {
        self.contract_definitions.lock().unwrap().get(&(
            con_id, crate::control::contracts::exchange_to_fix(exchange).to_string(),
        )).cloned()
    }

    pub(crate) fn attached_quote_contract(&self, key: (u32, u8)) -> Option<CachedAttachedQuote> {
        self.attached_quote_contracts.lock().unwrap().get(&key).cloned()
    }

    pub(crate) fn cache_attached_quote_contract(&self, key: (u32, u8), selection: CachedAttachedQuote) {
        self.attached_quote_contracts.lock().unwrap().insert(key, selection);
    }

    pub(crate) fn preferred_market(&self, group: i32) -> Option<String> {
        crate::control::aggregate_exchanges::preferred_market(&self.aggregate_exchanges.lock().unwrap(), group)
    }

    pub(crate) fn supports_attached_order_type(&self, definition: &ContractDefinition, api_type: &str) -> bool {
        let name = match api_type {
            "STP LMT" => "STPLMT",
            "TRAIL LIMIT" => "TRAILLMT",
            "STP PRT" => "STPPROT",
            name => name,
        };
        if definition.con_id == 0 && definition.sec_type == crate::control::contracts::SecurityType::Combo {
            return !definition.order_type_key.is_empty() && definition.order_type_key != "NONE"
                && definition.order_type_rules.iter().find(|(kind, _)| kind == name)
                    .is_some_and(|(_, simulation)| *simulation != 4);
        }
        let exchange = crate::control::contracts::exchange_to_fix(&definition.exchange);
        let tables = self.attached_order_types.lock().unwrap();
        tables.get(&(definition.con_id, exchange.into()))
            .or_else(|| tables.get(&(definition.con_id, String::new())))
            .and_then(|types| types.iter().find(|(kind, _)| kind == name))
            .is_some_and(|(_, simulation)| *simulation != 4)
    }

    pub(crate) fn cache_contract_definition(&self, definition: ContractDefinition) {
        if definition.sec_type == crate::control::contracts::SecurityType::Combo {
            self.combo_definitions.lock().unwrap().entry((
                definition.symbol.clone(),
                crate::control::contracts::exchange_to_fix(&definition.exchange).into(),
                definition.currency.clone(),
            )).or_insert_with(|| definition.clone());
        }
        if definition.con_id == 0 { return; }
        let exchange = crate::control::contracts::exchange_to_fix(&definition.exchange).to_string();
        if !definition.order_type_key.is_empty() && definition.order_type_key != "NONE" && !definition.order_type_rules.is_empty() {
            let mut tables = self.attached_order_types.lock().unwrap();
            tables.entry((definition.con_id, exchange.clone())).or_insert_with(|| definition.order_type_rules.clone());
            tables.entry((definition.con_id, String::new())).or_insert_with(|| definition.order_type_rules.clone());
        }
        let mut definitions = self.contract_definitions.lock().unwrap();
        definitions.insert((definition.con_id, String::new()), definition.clone());
        definitions.insert((definition.con_id, exchange), definition);
    }

    pub(crate) fn combo_definition(&self, symbol: &str, exchange: &str, currency: &str) -> Option<ContractDefinition> {
        self.combo_definitions.lock().unwrap().get(&(
            symbol.into(), crate::control::contracts::exchange_to_fix(exchange).into(), currency.into(),
        )).cloned()
    }

    pub(crate) fn combo_excluded_exchanges(&self) -> String {
        self.combo_excluded_exchanges.lock().unwrap().clone()
    }

    pub(crate) fn set_combo_excluded_exchanges(&self, value: String) {
        *self.combo_excluded_exchanges.lock().unwrap() = value;
    }

    pub(crate) fn set_aggregate_exchanges(&self, value: String) {
        *self.aggregate_exchanges.lock().unwrap() = value;
    }

    pub(crate) fn set_smart_combo_contracts(&self, usd: i32, stated: Option<&str>) {
        *self.smart_combo_contracts.lock().unwrap() = crate::control::aggregate_exchanges::smart_combo_ids(usd, stated);
    }

    /// The generic SMART combination contract for a currency, where the
    /// logon named one.
    pub(crate) fn smart_combo_conid(&self, currency: &str) -> Option<i32> {
        self.smart_combo_contracts.lock().unwrap().get(currency).copied()
    }

    pub(crate) fn carries_smart_combo_leg(&self, exchange: &str, security_type: &str) -> bool {
        crate::control::aggregate_exchanges::carries_smart_leg(
            &self.aggregate_exchanges.lock().unwrap(), exchange, security_type,
        )
    }

    /// What attaching orders to a combination has settled so far, under the
    /// combination's key.
    pub(crate) fn attached_combo(&self, key: &str) -> Option<AttachedCombo> {
        self.attached_combos.lock().unwrap().get(key).cloned()
    }

    /// Record something attaching orders to a combination settled.
    pub(crate) fn update_attached_combo(&self, key: &str, update: impl FnOnce(&mut AttachedCombo)) {
        update(self.attached_combos.lock().unwrap().entry(key.into()).or_default());
    }

    pub(crate) fn expect_attached_combo_confirmation(&self, key: String) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        self.attached_combo_confirmations.lock().unwrap().insert(key, send);
        receive
    }

    pub(crate) fn stop_waiting_for_combo_confirmation(&self, key: &str) {
        self.attached_combo_confirmations.lock().unwrap().remove(key);
    }

    pub(crate) fn answer_attached_combo_confirmation(&self, key: &str, answer: Vec<u8>) {
        if let Some(waiting) = self.attached_combo_confirmations.lock().unwrap().remove(key) {
            let _ = waiting.send(answer);
        }
    }

    pub(crate) fn request_attached_combo_rules(&self, exchange: &str) -> bool {
        let mut pending = self.attached_combo_rules_pending.lock().unwrap();
        if self.attached_combo_rules(exchange).is_some() { return false; }
        pending.insert(exchange.into())
    }

    pub(crate) fn forget_attached_combo_rule_request(&self, exchange: &str) {
        self.attached_combo_rules_pending.lock().unwrap().remove(exchange);
    }

    pub(crate) fn attached_combo_rules(&self, exchange: &str) -> Option<crate::control::attached_combos::ComboRules> {
        self.attached_combo_rules.lock().unwrap().get(exchange).cloned()
    }

    pub(crate) fn set_attached_combo_rules(&self, exchange: String, rules: crate::control::attached_combos::ComboRules) {
        let mut pending = self.attached_combo_rules_pending.lock().unwrap();
        self.attached_combo_rules.lock().unwrap().insert(exchange.clone(), rules);
        pending.remove(&exchange);
    }

    // ── Hot-loop-side writers ──

    #[doc(hidden)] pub fn push_historical_taken(&self, taken: HistoricalTaken) {
        self.historical_taken.push(taken);
    }

    #[doc(hidden)] pub fn push_historical_data(&self, req_id: u32, response: HistoricalResponse) {
        self.historical_data.push((req_id, response));
    }

    /// A bar request ended with no end of its own: refused, or failed after
    /// its history. A gateway states only the error, and nothing answers under
    /// the number after it.
    #[doc(hidden)] pub fn push_historical_over(&self, req_id: u32) {
        self.historical_over.push(req_id);
    }

    #[doc(hidden)] pub fn push_head_timestamp(&self, req_id: u32, response: HeadTimestampResponse) {
        self.head_timestamps.push((req_id, response));
    }

    #[doc(hidden)] pub fn push_contract_details(&self, req_id: u32, def: ContractDefinition) {
        self.cache_contract_definition(def.clone());
        self.contract_details.push((req_id, def));
    }

    #[doc(hidden)] pub fn push_contract_details_end(&self, req_id: u32) {
        self.contract_details_end.push(req_id);
    }

    #[doc(hidden)] pub fn push_calendar_meta_data(&self, req_id: u32, json: String) {
        self.calendar_meta_data.push((req_id, json));
    }

    #[doc(hidden)] pub fn push_calendar_events(&self, req_id: u32, json: String) {
        self.calendar_events.push((req_id, json));
    }

    /// Say that an order this session did not place is reachable here.
    ///
    /// The venue replays what the account is working when a session opens, and
    /// this client gives each of those a number of its own. That pairing —
    /// the permanent id the venue keeps and the number a caller uses here — is
    /// what `order_bound` carries, and it was worked out and never said.
    #[doc(hidden)] pub fn push_order_bound(&self, perm_id: i64, client_id: i64, order_id: i64) {
        if perm_id == 0 || order_id == 0 {
            return;
        }
        // Once per pairing, for the life of the session. Checked against what
        // has been said rather than against what is waiting to be said: the
        // queue is emptied every time a caller reads it, so a check against it
        // only covers pairings nobody has read yet — and the venue replays
        // these orders on every reconnect, and on a drop each is marked
        // uncertain and recovered again. A caller counting them would have
        // been counting reconnects.
        if !self.orders_bound_said.lock().unwrap().insert(perm_id) {
            return;
        }
        self.orders_bound.push((perm_id, client_id, order_id));
    }

    /// The pairings not yet handed over.
    pub fn drain_orders_bound(&self) -> Vec<(i64, i64, i64)> {
        self.orders_bound.drain()
    }

    #[doc(hidden)] pub fn push_matching_symbols(&self, req_id: u32, matches: Vec<SymbolMatch>) {
        self.matching_symbols.push((req_id, matches));
    }

    #[doc(hidden)] pub fn push_option_params(&self, req_id: u32, underlying_con_id: i64, scopes: Vec<OptionChainScope>) {
        self.option_params.push((req_id, underlying_con_id, scopes));
    }

    #[doc(hidden)] pub fn push_advisor_config(&self, fa_data_type: i32, xml: String) {
        self.advisor_config.push((fa_data_type, xml));
    }

    #[doc(hidden)] pub fn push_advisor_replaced(&self, req_id: i64, text: String) {
        self.advisor_replaced.push((req_id, text));
    }

    #[doc(hidden)] pub fn push_advisor_refused(&self, origin: api::ErrorOrigin, code: i32, text: String) {
        self.advisor_refused.push((origin.id(), code, text, origin));
    }

    #[doc(hidden)] pub fn push_scanner_params(&self, xml: String) {
        self.scanner_params.push(xml);
    }

    /// Throw away scan rows still queued under a request.
    ///
    /// Separate from the answers above because a request number means one
    /// thing per kind of request: a caller may hold the same number for a scan
    /// and for a set of bars, and withdrawing one must not take the other's.
    pub fn purge_scanner_data_for(&self, req_id: u32) {
        self.scanner_data.retain(|(id, _)| *id != req_id);
    }

    #[doc(hidden)] pub fn push_scanner_data(&self, req_id: u32, result: ScannerResult) {
        // A retained stream may never be read or dropped. Shed whole refreshes
        // so every surviving batch still carries all the rows the venue sent.
        self.scanner_data.push_bounded((req_id, result), STREAM_BACKLOG_LIMIT, "scanner_data");
    }

    #[doc(hidden)] pub fn push_historical_news(&self, req_id: u32, headlines: Vec<NewsHeadline>, has_more: bool) {
        self.historical_news.push((req_id, headlines, has_more));
    }

    #[doc(hidden)] pub fn push_news_article(&self, req_id: u32, article_type: i32, article_text: String) {
        self.news_articles.push((req_id, article_type, article_text));
    }

    #[doc(hidden)] pub fn push_fundamental_data(&self, req_id: u32, data: String) {
        self.fundamental_data.push((req_id, data));
    }

    #[doc(hidden)] pub fn push_histogram_data(&self, req_id: u32, entries: Vec<HistogramEntry>) {
        self.histogram_data.push((req_id, entries));
    }

    #[doc(hidden)] pub fn push_historical_ticks(&self, req_id: u32, data: HistoricalTickData, what_to_show: String, done: bool) {
        self.historical_ticks.push((req_id, data, what_to_show, done));
    }

    #[doc(hidden)] pub fn push_historical_schedule(&self, req_id: u32, response: HistoricalScheduleResponse) {
        self.historical_schedules.push((req_id, response));
    }

    /// A refusal of the request numbered `req_id`, which nothing more follows:
    /// the session's where it names no request, and a lookup's this client
    /// made for itself where it is numbered in the band those take.
    #[doc(hidden)] pub fn push_historical_error(&self, req_id: u32, code: i32, message: String) {
        let origin = match req_id {
            Self::NO_REQUEST => api::ErrorOrigin::Session,
            id if id >= Self::ASK_ID_BASE => api::ErrorOrigin::Internal(id),
            id => api::ErrorOrigin::Request { id: i64::from(id), ends: true },
        };
        self.push_error_from(req_id, origin, code, message);
    }

    /// Something said under the number `req_id` a reader holds it under,
    /// with what it is about.
    #[doc(hidden)] pub fn push_error_from(&self, req_id: u32, origin: api::ErrorOrigin, code: i32, message: String) {
        self.historical_errors.push((req_id, code, message, origin));
    }

    #[doc(hidden)] pub fn push_market_rules(&self, rules: Vec<MarketRule>) {
        let mut lock = self.market_rules.lock().unwrap();
        for rule in rules {
            if let Some(existing) = lock.iter_mut().find(|r| r.rule_id == rule.rule_id) {
                *existing = rule;
            } else {
                lock.push(rule);
            }
        }
    }

    /// The depth exchanges an ask is waiting for, once the venue has named
    /// them, and whether it has.
    ///
    /// The venue names them in the routing table a market-data connection is
    /// given at logon, so an ask made while none is up stays open until one
    /// is: spent on nothing, the ask was answered with nothing and the table
    /// answered nobody. A table naming no book is an answer, and an empty one.
    pub fn drain_depth_exchanges(&self) -> Option<Vec<DepthMktDataDescription>> {
        self.depth_exchanges_answers.take_first(|_| true)
    }

    /// Every exchange the venue named as serving a book, as it named them.
    ///
    /// Read rather than drained: a caller reading the list must not empty it.
    pub fn depth_exchanges(&self) -> Vec<DepthMktDataDescription> {
        self.depth_exchanges_cache.lock().unwrap().clone().unwrap_or_default()
    }

    /// Replaced whole: a reconnect states the table again, and added to what
    /// was already held it leaves every exchange in it twice.
    ///
    /// An ask still waiting for a table is answered with this one, here, in
    /// its place in the session's order.
    #[doc(hidden)] pub fn push_depth_exchanges(&self, descs: Vec<DepthMktDataDescription>) {
        let mut pending = self.depth_exchanges_pending.lock().unwrap();
        *self.depth_exchanges_cache.lock().unwrap() = Some(descs.clone());
        if std::mem::take(&mut *pending) {
            self.depth_exchanges_answers.push(descs);
        }
    }

    /// An ask for the exchanges that serve a book: answered with the table
    /// held, or when one arrives.
    #[doc(hidden)] pub fn notify_depth_exchanges(&self) {
        let mut pending = self.depth_exchanges_pending.lock().unwrap();
        match self.depth_exchanges_cache.lock().unwrap().clone() {
            Some(held) => self.depth_exchanges_answers.push(held),
            None => *pending = true,
        }
    }

    #[doc(hidden)] pub fn cache_contract(&self, con_id: i64, contract: api::Contract) {
        let mut cache = self.contract_cache.lock().unwrap();
        if let Some(existing) = cache.get_mut(&con_id) {
            // Merged field by field: what the incoming states stands, what it
            // leaves empty is kept. Merged on seven names alone, a definition
            // arriving over an entry a fill had seeded lost its strike, its
            // right, its expiry and its multiplier — which is all that tells
            // two options on one underlying apart.
            let api::Contract {
                symbol, sec_type, exchange, currency, local_symbol, primary_exchange, trading_class,
                last_trade_date_or_contract_month, last_trade_date, strike, right, multiplier,
                sec_id_type, sec_id, description, issuer_id, combo_legs_descrip, ..
            } = contract;
            for (field, stated) in [
                (&mut existing.symbol, symbol), (&mut existing.sec_type, sec_type),
                (&mut existing.exchange, exchange), (&mut existing.currency, currency),
                (&mut existing.local_symbol, local_symbol), (&mut existing.primary_exchange, primary_exchange),
                (&mut existing.trading_class, trading_class),
                (&mut existing.last_trade_date_or_contract_month, last_trade_date_or_contract_month),
                (&mut existing.last_trade_date, last_trade_date), (&mut existing.right, right),
                (&mut existing.multiplier, multiplier), (&mut existing.sec_id_type, sec_id_type),
                (&mut existing.sec_id, sec_id), (&mut existing.description, description),
                (&mut existing.issuer_id, issuer_id), (&mut existing.combo_legs_descrip, combo_legs_descrip),
            ] {
                if !stated.is_empty() { *field = stated; }
            }
            if strike != 0.0 { existing.strike = strike; }
        } else {
            cache.insert(con_id, contract);
        }
    }

    /// Cache what the venue's own definition of a contract states, and mark
    /// the entry as defined. An entry seeded from a fill or an order names the
    /// contract without defining it, and the lookup that would define it was
    /// skipped for the entry being there at all.
    #[doc(hidden)] pub fn cache_definition(&self, con_id: i64, contract: api::Contract) {
        self.cache_contract(con_id, contract);
        self.defined_contracts.lock().unwrap().insert(con_id);
    }

    /// Whether the venue's own definition of this contract has been cached,
    /// as against an entry that merely names it.
    pub fn has_definition(&self, con_id: i64) -> bool {
        self.defined_contracts.lock().unwrap().contains(&con_id)
    }

    // ── Gateway-local init data ──

    /// Which venue each bit of this contract's exchange masks refers to: the
    /// map the venue stated for the BBO exchange and security type its
    /// subscription was acknowledged under. Empty until that map is stated.
    pub fn smart_components_of(
        &self, instrument: crate::types::InstrumentId,
    ) -> Vec<crate::types::SmartComponent> {
        let Some(key) = self.bbo_keys.lock().unwrap().get(&instrument).cloned() else {
            return Vec::new();
        };
        self.smart_component_maps.lock().unwrap().get(&key).cloned().flatten().unwrap_or_default()
    }

    /// The BBO exchange a contract's subscription was acknowledged under, as a
    /// gateway states it on `tick_req_params`: the venue's id for it, with the
    /// security type's four-digit code behind it where the id is four
    /// characters or fewer. Empty where no acknowledgement named one.
    pub fn bbo_exchange_of(&self, instrument: crate::types::InstrumentId) -> String {
        let Some((id, sec_type)) = self.bbo_keys.lock().unwrap().get(&instrument).cloned() else {
            return String::new();
        };
        if id.is_empty() || id.len() > 4 || sec_type == 0 {
            return id;
        }
        format!("{id}{sec_type:04X}")
    }

    /// The map a caller names by BBO exchange, read the way a gateway reads
    /// the name: four to eight characters are the id and, in the last four, a
    /// security type's code in hexadecimal; anything else, or a code naming no
    /// type, is the id alone and matches the id under any type.
    ///
    /// `Err` where no subscription or map has named that key: a gateway
    /// refuses it. `Ok(None)` where one has and its map has not arrived yet.
    fn smart_components_named(
        &self, bbo_exchange: &str,
    ) -> Result<Option<Vec<crate::types::SmartComponent>>, crate::error_codes::Refusal> {
        let (id, sec_type) = split_bbo_exchange(bbo_exchange);
        self.smart_component_maps
            .lock()
            .unwrap()
            .iter()
            .find(|((k, t), _)| *k == id && sec_type.is_none_or(|stated| stated == *t))
            .map(|(_, map)| map.clone())
            .ok_or_else(|| {
                crate::error_codes::Refusal::validation("Invalid BBO exchange/security type code")
            })
    }

    /// Ask for the map a caller names by BBO exchange.
    ///
    /// A key nothing has named is refused as a gateway refuses it. A map held
    /// is the answer. One that has not arrived is waited for, as a gateway
    /// waits for it, and answered from [`Self::drain_smart_component_answers`]
    /// — the call does not wait for it.
    pub fn ask_smart_components(
        &self, req_id: i64, bbo_exchange: &str,
    ) -> Result<Option<Vec<crate::types::SmartComponent>>, crate::error_codes::Refusal> {
        let held = self.smart_components_named(bbo_exchange)?;
        if held.is_none() {
            self.smart_component_asks.lock().unwrap().push((
                req_id, bbo_exchange.to_string(), std::time::Instant::now(),
            ));
        }
        Ok(held)
    }

    /// The asks whose map has arrived, and those that have waited as long as
    /// a gateway waits — two seconds — refused in the gateway's words, under
    /// the number it states a refusal of its own under.
    pub fn drain_smart_component_answers(
        &self, now: std::time::Instant,
    ) -> Vec<(i64, Result<Vec<crate::types::SmartComponent>, crate::error_codes::Refusal>)> {
        const WAIT: std::time::Duration = std::time::Duration::from_millis(2000);
        let mut answered = Vec::new();
        self.smart_component_asks.lock().unwrap().retain(|(req_id, named, asked_at)| {
            let answer = match self.smart_components_named(named) {
                Ok(Some(components)) => Ok(components),
                Ok(None) if now.duration_since(*asked_at) < WAIT => return true,
                Ok(None) => {
                    let (id, sec_type) = split_bbo_exchange(named);
                    let named = sec_type.and_then(sec_type_named).unwrap_or("null");
                    Err(crate::error_codes::Refusal::unnumbered(format!(
                        "Unable to retrieve smart components for BBO exchange {id} and security \
                         type {named}",
                    )))
                }
                Err(why) => Err(why),
            };
            answered.push((*req_id, answer));
            false
        });
        answered
    }

    /// Note the BBO exchange a contract's subscription was acknowledged under.
    #[doc(hidden)] pub fn note_bbo_exchange(
        &self, instrument: crate::types::InstrumentId, id: &str, sec_type: &str,
    ) {
        let key = (id.to_string(), sec_type_code(sec_type));
        self.smart_component_maps.lock().unwrap().entry(key.clone()).or_insert(None);
        self.bbo_keys.lock().unwrap().insert(instrument, key);
    }

    /// Whether the venue states a contract's snapshot as chargeable, by the
    /// venue's number, as its acknowledgement says. Nought says nothing and
    /// leaves what was said before standing, and so does a number outside
    /// the five a gateway knows (0 to 4), which a gateway does not keep.
    #[doc(hidden)] pub fn note_snapshot_permission(
        &self, instrument: crate::types::InstrumentId, stated: u32,
    ) {
        if (1..=4).contains(&stated) {
            self.snapshot_permissions.lock().unwrap().insert(instrument, stated);
        }
    }

    /// The venue's number for whether a contract's snapshot is chargeable, as
    /// `tick_req_params` states it; nought where no acknowledgement said.
    pub fn snapshot_permission_of(&self, instrument: crate::types::InstrumentId) -> u32 {
        self.snapshot_permissions.lock().unwrap().get(&instrument).copied().unwrap_or(0)
    }

    /// The contract in the slot is gone, and what its acknowledgements said of
    /// it with it; the maps it named stay, as the venue's statements about a
    /// key rather than about the contract.
    #[doc(hidden)] pub fn forget_bbo_exchange(&self, instrument: crate::types::InstrumentId) {
        self.bbo_keys.lock().unwrap().remove(&instrument);
        self.snapshot_permissions.lock().unwrap().remove(&instrument);
    }

    /// Keep the map the venue stated beside a contract's subscription, under
    /// the key that contract's acknowledgement named — or, where none named
    /// one, under no BBO exchange and the contract's own type, which is then
    /// the contract's key.
    #[doc(hidden)] pub fn set_smart_components_of(
        &self, instrument: crate::types::InstrumentId, sec_type: &str,
        components: Vec<crate::types::SmartComponent>,
    ) {
        let key = self.bbo_keys.lock().unwrap()
            .entry(instrument)
            .or_insert_with(|| (String::new(), sec_type_code(sec_type)))
            .clone();
        self.smart_component_maps.lock().unwrap().insert(key, Some(components));
    }

    /// Every provider this account may read.
    pub fn news_providers(&self) -> Vec<crate::types::NewsProvider> {
        self.news_providers.lock().unwrap().clone()
    }

    /// Every soft dollar tier it may direct commission to.
    pub fn soft_dollar_tiers(&self) -> Vec<crate::types::SoftDollarTier> {
        self.soft_dollar_tiers.lock().unwrap().clone()
    }

    /// Every account family this login belongs to.
    pub fn family_codes(&self) -> Vec<crate::types::FamilyCode> {
        self.family_codes.lock().unwrap().clone()
    }

    /// How the venue brands this login.
    pub fn white_branding_id(&self) -> String {
        self.white_branding_id.lock().unwrap().clone()
    }

    /// Session ID surfaced to webapp REST clients as the `x-ccp-session-id` header.
    /// Empty until gateway logon completes.
    pub fn ccp_session_id(&self) -> String {
        self.ccp_session_id.lock().unwrap().clone()
    }

    /// Logical-name → host URL map pushed by the venue during logon. Empty when
    /// no URL set was pushed; consumers should fall back to a documented literal
    /// (e.g. `api.ibkr.com` for `region_dam`).
    pub fn misc_urls(&self) -> HashMap<String, String> {
        self.misc_urls.lock().unwrap().clone()
    }

    /// Single lookup against the URL map. Returns `None` when missing.
    pub fn misc_url(&self, key: &str) -> Option<String> {
        self.misc_urls.lock().unwrap().get(key).cloned()
    }

    /// Security type → the order types the venue permits for it. Stated by the
    /// venue at logon; empty until logon completes.
    pub fn order_permissions(&self) -> HashMap<String, Vec<String>> {
        self.order_permissions.lock().unwrap().clone()
    }

    /// The order types permitted for one security type, or `None` when the venue
    /// does not permit the type at all. A combination is named `COMB`.
    pub fn permitted_order_types(&self, sec_type: &str) -> Option<Vec<String>> {
        let key = if matches!(sec_type, "BAG" | "COMBO") { "COMB" } else { sec_type };
        self.order_permissions.lock().unwrap().get(key).cloned()
    }

    /// Feature tokens the venue enables for this account.
    pub fn enabled_features(&self) -> Vec<String> {
        self.enabled_features.lock().unwrap().clone()
    }

    /// Whether the venue enables one feature for this account, read without
    /// copying the list.
    pub fn enables(&self, feature: &str) -> bool {
        self.enabled_features.lock().unwrap().iter().any(|f| f == feature)
    }

    /// The accounts the logon names as the login's own, family-linked ones
    /// left out, and whether the login is an advisor's.
    pub fn login(&self) -> (Vec<String>, bool) {
        self.login.lock().unwrap().clone()
    }

    /// Whether the logon stated this login is an advisor's.
    pub fn advisor(&self) -> bool {
        self.login.lock().unwrap().1
    }

    #[doc(hidden)] pub fn set_advisor(&self, advisor: bool) {
        self.login.lock().unwrap().1 = advisor;
    }

    /// Whether the logon named accounts `AllNonProp` leaves out.
    pub fn all_non_prop_leaves_out(&self) -> bool {
        self.all_non_prop_leaves_out.load(Ordering::Relaxed)
    }

    #[doc(hidden)] pub fn set_all_non_prop_leaves_out(&self, named: bool) {
        self.all_non_prop_leaves_out.store(named, Ordering::Relaxed);
    }

    /// Whether the logon asks for the venue's refusal of an order stated on a
    /// status report to be told to the program that placed it.
    pub(crate) fn refusals_told(&self) -> bool {
        self.refusals_told.load(Ordering::Relaxed)
    }

    #[doc(hidden)] pub fn set_refusals_told(&self, told: bool) {
        self.refusals_told.store(told, Ordering::Relaxed);
    }

    /// The broker the logon names the login as being with, empty where it
    /// names none.
    pub(crate) fn broker(&self) -> String {
        self.broker.lock().unwrap().clone()
    }

    #[doc(hidden)] pub fn set_broker(&self, broker: String) {
        *self.broker.lock().unwrap() = broker;
    }

    #[doc(hidden)] pub fn set_names(&self, short_broker: String, product: String) {
        *self.names.lock().unwrap() = (short_broker, product);
    }

    /// What a gateway says as the trading connection goes: 1100.
    pub(crate) fn connectivity_lost(&self) -> String {
        format!("Connectivity between {} has been lost.", self.connectivity_between())
    }

    /// What a gateway says as it comes back: 1102, with the data farms as they
    /// stand.
    pub(crate) fn connectivity_restored(&self, farms: &str) -> String {
        format!("Connectivity between {} has been restored - data maintained.{farms}", self.connectivity_between())
    }

    /// Who the connectivity notices name at each end, as a gateway names them
    /// from the logon: the broker's short name, or IBKR where the logon names
    /// Interactive Brokers or no broker at all; and the product, TWS where it
    /// is Trader Workstation and Trader Workstation where the logon names none.
    fn connectivity_between(&self) -> String {
        let broker = self.broker();
        let (short_broker, product) = self.names.lock().unwrap().clone();
        let company = if !short_broker.trim().is_empty() {
            short_broker
        } else if broker.is_empty() || broker.contains("Interactive Brokers") {
            "IBKR".to_string()
        } else {
            broker
        };
        let product = if product.starts_with("Trader Workstation") {
            "TWS".to_string()
        } else if product.is_empty() {
            "Trader Workstation".to_string()
        } else {
            product
        };
        format!("{company} and {product}")
    }

    /// What the logon states about orders for an amount of money.
    pub(crate) fn money_orders(&self) -> MoneyOrderTerms {
        self.money_orders.lock().unwrap().clone()
    }

    #[doc(hidden)] pub fn set_money_orders(&self, terms: MoneyOrderTerms) {
        *self.money_orders.lock().unwrap() = terms;
    }

    /// The part of a unit a size is shown to on a contract that states no
    /// least size of its own, as the logon states it; empty where it states
    /// none.
    pub(crate) fn size_fraction(&self) -> String {
        self.size_fraction.lock().unwrap().clone()
    }

    #[doc(hidden)] pub fn set_size_fraction(&self, fraction: String) {
        *self.size_fraction.lock().unwrap() = fraction;
    }

    /// Where the executions the session opened with start, in unix seconds:
    /// what it holds reaches back that far. `None` before a logon states it.
    pub fn executions_held_from(&self) -> Option<i64> {
        *self.executions_held_from.lock().unwrap()
    }

    #[doc(hidden)] pub fn set_executions_held_from(&self, from: Option<i64>) {
        *self.executions_held_from.lock().unwrap() = from;
    }

    /// Which algorithms the venue offers, keyed `PROVIDER/SECTYPE`.
    ///
    /// The venue states this on the session; it is not a property of a
    /// contract. An algorithm absent here is one this account may not use.
    pub fn algorithms(&self) -> HashMap<String, Vec<String>> {
        self.algorithms.lock().unwrap().clone()
    }

    /// The algorithms offered for one security type, across every provider.
    pub fn algorithms_for(&self, sec_type: &str) -> Vec<String> {
        let want = format!("/{}", sec_type.to_ascii_uppercase());
        let mut out: Vec<String> = self.algorithms.lock().unwrap()
            .iter()
            .filter(|(k, _)| k.to_ascii_uppercase().ends_with(&want))
            .flat_map(|(_, v)| v.iter().cloned())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The order presets the account holds, as `(key, attributes, changed at)`.
    ///
    /// The venue keeps a set of order defaults per security type and fills
    /// parts of an order the caller left unstated from them — the size a
    /// compete order competes with and the offset it competes by, where the
    /// caller named neither. So two identical calls on two accounts are not
    /// the same order, and nothing in the reference client's surface says so.
    ///
    /// The key is the venue's own, `s=STK` or `s=CASH&tc=EUR`. The attributes
    /// are as the venue writes them: `v=` the set's variant, `a=1` where it is
    /// active. The values behind it are asked for separately, when an order
    /// attaching from the set needs them.
    pub fn order_presets(&self) -> Vec<(String, String, String)> {
        self.order_presets.lock().unwrap().list.clone().unwrap_or_default()
    }

    #[doc(hidden)] pub fn set_order_presets(&self, presets: Vec<(String, String, String)>) {
        let mut state = self.order_presets.lock().unwrap();
        if state.list.as_ref() != Some(&presets) {
            state.generation = state.generation.wrapping_add(1);
        }
        state.list = Some(presets);
    }

    /// Keep a values answer under its key. An error remains an error in the
    /// answer; it does not remove the account's preset list. One that answers
    /// no request this client is waiting on is kept as out of date.
    #[doc(hidden)] pub fn set_order_preset_values(&self, values: crate::control::order_presets::PresetValues) {
        let mut state = self.order_presets.lock().unwrap();
        let generation = match state.waiting.remove_entry(&values.request_key) {
            Some((_, (key, generation, answer))) if key == values.key => {
                let _ = answer.try_send(values.clone());
                generation
            }
            Some((request, waiting)) => {
                state.waiting.insert(request, waiting);
                u64::MAX
            }
            None => u64::MAX,
        };
        state.values.insert(values.key.clone(), (generation, values));
    }

    pub(crate) fn order_preset_list(&self) -> Option<Vec<(String, String, String)>> {
        self.order_presets.lock().unwrap().list.clone()
    }

    pub(crate) fn current_order_preset_values(&self, key: &str) -> Option<crate::control::order_presets::PresetValues> {
        let state = self.order_presets.lock().unwrap();
        state.values.get(key).filter(|(generation, _)| *generation == state.generation).map(|(_, values)| values.clone())
    }

    pub(crate) fn expect_order_preset_values(&self, key: &str) -> (String, std::sync::mpsc::Receiver<crate::control::order_presets::PresetValues>) {
        let mut state = self.order_presets.lock().unwrap();
        state.sequence += 1;
        let request = format!("OPR.{}", state.sequence);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let generation = state.generation;
        state.waiting.insert(request.clone(), (key.into(), generation, send));
        (request, receive)
    }

    pub(crate) fn stop_waiting_for_preset_values(&self, request: &str) {
        self.order_presets.lock().unwrap().waiting.remove(request);
    }

    /// What this derivative is written on, where the venue has said.
    pub fn under_con_id(&self, con_id: u32) -> Option<u32> {
        self.under_con_ids.lock().unwrap().get(&con_id).copied()
    }

    #[doc(hidden)] pub fn note_under_con_id(&self, con_id: u32, under_con_id: u32) {
        if con_id != 0 && under_con_id != 0 {
            self.under_con_ids.lock().unwrap().insert(con_id, under_con_id);
        }
    }

    /// What a contract pays out over the life of an option on it, where the
    /// venue has stated it for this contract.
    pub fn dividend_schedule(
        &self, con_id: u32,
    ) -> Option<crate::control::dividends::Schedule> {
        self.dividend_schedules.lock().unwrap().get(&con_id).cloned()
    }

    #[doc(hidden)] pub fn set_dividend_schedule(
        &self, con_id: u32, schedule: crate::control::dividends::Schedule,
    ) {
        self.dividend_schedules.lock().unwrap().insert(con_id, schedule);
    }

    /// The rates the venue states for a currency, where it has.
    pub(crate) fn currency_rates(&self, currency: &str) -> Option<Vec<(String, f64)>> {
        self.currency_rates.lock().unwrap().get(currency).cloned()
    }

    pub(crate) fn set_currency_rates(&self, currency: &str, rates: Vec<(String, f64)>) {
        self.currency_rates.lock().unwrap().insert(currency.to_string(), rates);
    }

    /// What is read from a contract's sessions, where the venue has stated
    /// them.
    pub(crate) fn contract_schedule<R>(
        &self, con_id: u32, read: impl FnOnce(&crate::control::contracts::ContractSchedule) -> R,
    ) -> Option<R> {
        let (keys, schedules) = &*self.contract_schedules.lock().unwrap();
        keys.get(&con_id).and_then(|key| schedules.get(key)).map(read)
    }

    /// The key a contract's sessions are joined to it on, where it is known.
    pub(crate) fn schedule_key(&self, con_id: u32) -> Option<String> {
        self.contract_schedules.lock().unwrap().0.get(&con_id).cloned()
    }

    pub(crate) fn note_schedule_key(&self, con_id: u32, key: &str) {
        self.contract_schedules.lock().unwrap().0.insert(con_id, key.to_string());
    }

    pub(crate) fn set_contract_schedule(
        &self, key: &str, schedule: crate::control::contracts::ContractSchedule,
    ) {
        self.contract_schedules.lock().unwrap().1.insert(key.to_string(), schedule);
    }

    /// What the venue states about a contract's company or its terms on one
    /// series, as the pairs it wrote.
    ///
    /// Seventeen series carry it, each asked for by the venue's own number for
    /// it on the market data request:
    ///
    /// | Series | What it states |
    /// | --- | --- |
    /// | 386 | What the company has coming, and when |
    /// | 434, 548 | The two analyst ratings |
    /// | 454 | What institutions and insiders hold, and the shares on issue |
    /// | 505 | What a fund will take part in |
    /// | 628, 633 | A fund family's figures, and the same by financial year |
    /// | 631 | The technical readings taken on the contract |
    /// | 669 | The ratios the venue keeps a history of |
    /// | 678, 699 | Two scores worked out from what is written about the company |
    /// | 700 | How the company scores against the principles an account can screen on |
    /// | 703 | What margin dealing in the contract takes |
    /// | 705 | The same screening principles, as the venue's own fields |
    /// | 726 | The lens the venue publishes over the company's accounts |
    /// | 750 | The price the venue holds the contract against for reference |
    /// | 752 | Whether the contract passes a religious screen |
    ///
    /// Empty until one of them has been asked for and answered, and empty for
    /// a contract whose subscriptions do not cover it — the venue answers a
    /// series the account cannot see with silence rather than with a refusal.
    ///
    /// The keys are the venue's own, unchanged. They are the venue's to add to
    /// and to rename, so nothing here reads them or promises a set of them.
    ///
    /// What one contract states is not dropped when the subscription that
    /// fetched it ends — it is a fact about the contract rather than about the
    /// watch, so a caller that comes back to a contract finds it still there.
    ///
    /// Held for four thousand and ninety-six contracts, and the contract heard
    /// of longest ago is dropped to make room. One ordinary
    /// share stating every series is about twenty-three kilobytes, so what this
    /// holds is bounded at a hundred megabytes or so however long a session
    /// runs — where before it grew with every contract ever watched.
    pub fn company_data(&self, con_id: u32, series: u32) -> Vec<(String, String)> {
        self.company_data.lock().unwrap().0
            .get(&con_id)
            .and_then(|held| held.get(&series))
            .cloned()
            .unwrap_or_default()
    }

    /// Which of those series have been stated for a contract, in order.
    pub fn company_data_series(&self, con_id: u32) -> Vec<u32> {
        let held = self.company_data.lock().unwrap();
        let Some(held) = held.0.get(&con_id) else { return Vec::new() };
        let mut series: Vec<u32> = held.keys().copied().collect();
        series.sort_unstable();
        series
    }

    /// The scan a caller has asked for on an underlying, as the venue takes it.
    pub fn spread_scan(&self, con_id: u32) -> Option<String> {
        self.spread_scans.lock().unwrap().get(&con_id).cloned()
    }

    #[doc(hidden)] pub fn note_spread_scan(&self, con_id: u32, stated: String) {
        self.spread_scans.lock().unwrap().insert(con_id, stated);
    }

    #[doc(hidden)] pub fn note_company_data(
        &self, con_id: u32, series: u32, pairs: Vec<(String, String)>,
    ) {
        if con_id == 0 {
            return;
        }
        let (held, order) = &mut *self.company_data.lock().unwrap();
        if held.entry(con_id).or_default().insert(series, pairs).is_none()
            && held[&con_id].len() == 1
        {
            // First time this contract has stated anything. Held for this
            // many contracts and no more: a session sweeping a scanner across
            // thousands would otherwise keep every one of them for as long as
            // it ran.
            order.push_back(con_id);
            while order.len() > COMPANY_DATA_HELD
                && let Some(gone) = order.pop_front()
            {
                held.remove(&gone);
            }
        }
    }

    #[doc(hidden)] pub fn set_algorithms(&self, algorithms: HashMap<String, Vec<String>>) {
        *self.algorithms.lock().unwrap() = algorithms;
    }

    /// Add feature tokens the venue states after logon. What logon already
    /// stated is kept; this only ever adds.
    #[doc(hidden)] pub fn add_enabled_features(&self, more: Vec<String>) {
        let mut have = self.enabled_features.lock().unwrap();
        for token in more {
            if !have.contains(&token) {
                have.push(token);
            }
        }
        self.settle_island_grant(&have);
    }

    /// The token that grants the older spelling of Nasdaq, as the venue
    /// reads it: off the granted list at logon, held on its own from then on.
    fn settle_island_grant(&self, granted: &[String]) {
        self.island_granted.store(
            granted.iter().any(|t| t == ISLAND_FOR_NASDAQ_GRANT),
            Ordering::Relaxed,
        );
    }

    /// Whether the venue grants the older spelling of Nasdaq to this account.
    pub fn island_granted(&self) -> bool {
        self.island_granted.load(Ordering::Relaxed)
    }

    #[doc(hidden)] pub fn set_order_permissions(&self, perms: HashMap<String, Vec<String>>) {
        *self.order_permissions.lock().unwrap() = perms;
    }

    #[doc(hidden)] pub fn set_login(&self, accounts: Vec<String>, advisor: bool) {
        *self.login.lock().unwrap() = (accounts, advisor);
    }

    #[doc(hidden)] pub fn set_enabled_features(&self, features: Vec<String>) {
        self.settle_island_grant(&features);
        *self.enabled_features.lock().unwrap() = features;
    }

    #[doc(hidden)] pub fn set_news_providers(&self, providers: Vec<crate::types::NewsProvider>) {
        *self.news_providers.lock().unwrap() = providers;
    }

    #[doc(hidden)] pub fn set_soft_dollar_tiers(&self, tiers: Vec<crate::types::SoftDollarTier>) {
        *self.soft_dollar_tiers.lock().unwrap() = tiers;
    }

    #[doc(hidden)] pub fn set_family_codes(&self, codes: Vec<crate::types::FamilyCode>) {
        *self.family_codes.lock().unwrap() = codes;
    }

    #[doc(hidden)] pub fn set_white_branding_id(&self, id: String) {
        *self.white_branding_id.lock().unwrap() = id;
    }

    #[doc(hidden)] pub fn set_ccp_session_id(&self, id: String) {
        *self.ccp_session_id.lock().unwrap() = id;
    }

    /// Another session that already held this account when this one connected,
    /// as the venue named it: where it connected from, when it logged in, and
    /// whether this session may look but not trade.
    ///
    /// `None` when this session is alone. The venue permits one logon at a time
    /// and takes the account from the older session without saying which it
    /// dropped, so a caller that wants to know before it starts work asks here.
    pub fn competing_session(&self) -> Option<(String, String, bool)> {
        self.competing_session.lock().unwrap().clone()
    }

    /// Why this session ended, if it has. `Some` means no request can be
    /// answered any more: the transports are down and nothing is trying to
    /// bring them back, so a caller that keeps asking is waiting out a timeout
    /// per call for an answer that cannot arrive.
    pub fn session_over(&self) -> Option<&'static str> {
        *self.session_over.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Why the trading connection is gone for good, where it is.
    ///
    /// Its own, apart from the session's: one flag for every transport says a
    /// quote feed the venue will not serve again has ended the trading
    /// connection too, and an order refused on that reading is an order the
    /// venue would have taken. This is set only where the trading connection
    /// itself has stopped being retried.
    pub fn trading_over(&self) -> Option<&'static str> {
        *self.trading_over.lock().unwrap()
    }

    /// Record why the trading connection stopped for good. The first stands.
    #[doc(hidden)] pub fn set_trading_over(&self, why: &'static str) {
        let mut over = self.trading_over.lock().unwrap();
        if over.is_none() {
            *over = Some(why);
        }
    }

    #[doc(hidden)] pub fn clear_trading_over(&self) {
        *self.trading_over.lock().unwrap() = None;
    }

    /// Record why this session ended. The first reason stands.
    ///
    /// A session ends once, and what ended it is the first thing that did. The
    /// tidying that follows is not a second reason: a session taken away by a
    /// login elsewhere, or refused by the venue, is shut down afterwards like
    /// any other. A shutdown overwriting the reason reports "the caller asked
    /// to stop" for a session the caller did not stop, and discards the reason
    /// there was something to say about.
    #[doc(hidden)] pub fn set_session_over(&self, why: &'static str) {
        let mut over = self.session_over.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if over.is_none() {
            *over = Some(why);
        }
    }

    #[doc(hidden)] pub fn clear_session_over(&self) {
        *self.session_over.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    #[doc(hidden)] pub fn set_competing_session(&self, other: Option<(String, String, bool)>) {
        *self.competing_session.lock().unwrap() = other;
    }

    #[doc(hidden)] pub fn set_misc_urls(&self, urls: HashMap<String, String>) {
        *self.misc_urls.lock().unwrap() = urls;
    }
}

/// The first id the engine asks its own questions under — a cold-cache
/// auto-fetch, a scanner enrichment. Nothing a caller or an answering call
/// issues reaches this far.
pub const ENGINE_ID_BASE: u32 = 0xF000_0000;

/// The band stays clear of the engine's own requests. Ordered by construction,
/// so a base moved into them fails the build rather than a test that might not
/// be run.
const _: () = assert!(ReferenceState::ASK_ID_BASE < ENGINE_ID_BASE);

#[cfg(test)]
mod ask_id_band {
    use super::{RecordKind, ReferenceState};

    /// A caller's id is not one of this client's own.
    ///
    /// A session numbers its requests from the account's order counter, which
    /// is seeded near the epoch in seconds. Those ids sat inside the old band,
    /// so every answer to them was held back for a waiting internal call that
    /// did not exist, and the request hung with nothing reported. The failure
    /// was invisible offline because the queues were filled by hand.
    #[test]
    fn an_id_seeded_from_the_account_counter_is_the_callers() {
        // What a session hands out today, and will for decades.
        for seeded in [1_786_766_504_u32, 1_900_000_000, 2_500_000_000] {
            assert!(
                !ReferenceState::is_ask_id(seeded),
                "{seeded} is a caller's id, so its answer must be delivered",
            );
        }
        // The floor itself, and the id just under it, which is still theirs.
        assert!(ReferenceState::is_ask_id(ReferenceState::ASK_ID_BASE));
        assert!(!ReferenceState::is_ask_id(ReferenceState::ASK_ID_BASE - 1));
        // And the ceiling: the engine's own questions are not answering calls,
        // so their replies are not withheld for one.
        assert!(!ReferenceState::is_ask_id(super::ENGINE_ID_BASE));
        assert!(ReferenceState::is_ask_id(super::ENGINE_ID_BASE - 1));
    }

    /// And such an answer actually leaves the queue a dispatch loop drains.
    #[test]
    fn the_dispatch_loop_delivers_it() {
        let state = ReferenceState::new();
        // Recorded, because that is what makes it this session's own now. Read
        // off its size, the test passed only when some other test in the same
        // run had happened to record it — and failed on its own.
        state.note_ours(RecordKind::Answer, ReferenceState::ASK_ID_BASE as i64);
        let q = crate::bridge::Queue::new(&crate::bridge::Stamps::default());
        q.push((1_786_766_504_u32, "theirs"));
        q.push((ReferenceState::ASK_ID_BASE, "ours"));
        let out = state.drain_dispatchable(&q);
        assert_eq!(out.len(), 1, "the caller's answer is delivered");
        assert_eq!(out[0].1, "theirs");
        assert_eq!(q.len(), 1, "and ours is left for the waiting call");
    }

    /// Two sessions in one process do not hold each other's ids.
    ///
    /// Both count their own questions from the same number, so a record shared
    /// between them would let one release what the other is waiting on, and an
    /// answer meant for a waiting call would be handed to a dispatch loop that
    /// never asked.
    #[test]
    fn one_session_does_not_release_what_another_is_waiting_on() {
        let mine = ReferenceState::new();
        let theirs = ReferenceState::new();
        let id = ReferenceState::ASK_ID_BASE as i64;

        mine.note_ours(RecordKind::Answer, id);
        theirs.note_ours(RecordKind::Answer, id);
        theirs.forget_ours(RecordKind::Answer, id);

        assert!(
            mine.is_ours(RecordKind::Answer, id),
            "another session's release took mine with it",
        );
        assert!(
            !theirs.is_ours(RecordKind::Answer, id),
            "and its own release still took effect",
        );
    }

    /// Dispatch leaves a retained scan's batches for its reader, even when
    /// that reader never advances. The oldest refreshes leave whole.
    #[test]
    fn an_unread_scan_keeps_a_bounded_backlog_of_whole_batches() {
        let state = ReferenceState::new();
        let limit = super::STREAM_BACKLOG_LIMIT;
        state.note_ours(RecordKind::Scanner, 7);
        for batch in 0..=limit {
            let result = crate::control::scanner::parse_scanner_response(&format!(
                "<ScanResponse><scanTime>20260908 14:30:00</scanTime>\
                 <Contract><contractID>{}</contractID></Contract>\
                 <Contract><contractID>{}</contractID></Contract></ScanResponse>",
                batch + 1, batch + 2,
            )).expect("a complete refresh");
            state.push_scanner_data(7, result);
        }
        assert!(state.drain_scanner_data_for_dispatch(
            |id| state.is_ours(RecordKind::Scanner, id as i64),
        ).is_empty(), "the stream's batches stay queued");

        let batches = state.take_scanner_data_for(7);
        let shed = limit / 10;
        assert_eq!(batches.len(), limit - shed + 1, "unread refreshes stay bounded");
        for (offset, batch) in batches.iter().enumerate() {
            let first = (shed + offset + 1) as u32;
            assert_eq!(batch.con_ids, vec![first, first + 1]);
            assert_eq!(
                batch.entries.iter().map(|entry| entry.con_id).collect::<Vec<_>>(),
                batch.con_ids,
                "each surviving refresh keeps every row in order",
            );
            assert_eq!(batch.scan_time, "20260908 14:30:00");
            assert!(batch.error_text.is_empty());
        }
        assert!(state.take_scanner_data_for(7).is_empty());
    }
}

#[cfg(test)]
mod adjustments_store_tests {
    use super::*;

    /// The actions a session was answered with are held against their contract.
    ///
    /// Read rather than taken, because a caller adjusting one series must not
    /// spend them for the next: two questions about the same contract are
    /// answered from the one reply the venue sent for it.
    #[test]
    fn the_actions_stay_against_the_contract_they_name() {
        let state = ReferenceState::new();
        let answered = "conc\n756733,-1,-1\nconexch\n756733,AMEX,20090223\nCD\n\
20240315,1.594937,USD,20240314,20240318,20240430,R,NA\n";
        let (contract, actions) = crate::control::adjustments::parse_adjustments(answered);
        state.note_adjustments(contract, actions, 1);

        let (held, acts) = state.adjustments_for("756733").expect("held against its contract");
        assert_eq!(held.exchange, "AMEX");
        assert_eq!(acts.len(), 1);
        assert!(state.adjustments_for("756733").is_some(), "and still held after reading");
        assert!(state.adjustments_for("999").is_none(), "a contract with none says so");
    }


    /// An answer about a contract is cleared before that contract is asked again.
    ///
    /// The record is kept against the contract, so a second question over a
    /// different range finds the first question's answer waiting. Whoever waits
    /// on the record must clear it first, or they are handed an answer to a
    /// question they did not ask — and over a narrower range that is a series
    /// adjusted by fewer actions than moved it.
    #[test]
    fn an_old_answer_is_cleared_before_the_same_contract_is_asked_again() {
        use crate::control::adjustments::{AdjustedContract, Adjustment, AdjustmentKind};
        let state = ReferenceState::new();
        let split = vec![Adjustment {
            kind: Some(AdjustmentKind::Split),
            date: "20240610".into(),
            value: "10".into(),
            ..Default::default()
        }];
        state.note_adjustments(
            AdjustedContract { con_id: "4815747".into(), ..Default::default() },
            split, 1,
        );
        assert!(state.adjustments_for("4815747").is_some(), "the answer is held");

        state.forget_adjustments("4815747");
        assert!(
            state.adjustments_for("4815747").is_none(),
            "cleared, so the next look can only find the next answer",
        );
        // Another contract's answer is untouched by it.
        state.note_adjustments(
            AdjustedContract { con_id: "756733".into(), ..Default::default() },
            Vec::new(), 1,
        );
        state.forget_adjustments("4815747");
        assert!(state.adjustments_for("756733").is_some(), "one contract at a time");
    }


    /// A late answer to a question already given up on is not the next one's.
    ///
    /// Clearing the record before asking is not enough on its own. A caller
    /// that waited and gave up leaves its question outstanding, and the answer
    /// can still arrive afterwards and be filed — sitting there for the next
    /// question about the same contract to pick up, over a range it never asked
    /// about. What each answer belongs to is kept beside it, so the next caller
    /// can see that this one is not theirs.
    #[test]
    fn a_late_answer_belongs_to_the_question_that_asked_it() {
        use crate::control::adjustments::{AdjustedContract, Adjustment, AdjustmentKind};
        let state = ReferenceState::new();
        let split = vec![Adjustment {
            kind: Some(AdjustmentKind::Split),
            date: "20240610".into(),
            value: "10".into(),
            ..Default::default()
        }];
        // The first question is given up on; its answer arrives anyway.
        state.expect_adjustments(7);
        state.note_adjustments(
            AdjustedContract { con_id: "4815747".into(), ..Default::default() },
            split, 7,
        );
        // The second question, over some other range, must not take it.
        assert!(
            state.take_adjustments_answering(8).is_none(),
            "an answer to request 7 is not the answer to request 8",
        );
        assert!(
            state.take_adjustments_answering(7).is_some(),
            "and it is still the answer to the one that did ask it",
        );
        assert!(
            state.take_adjustments_answering(7).is_none(),
            "taken, so it is not there to be taken twice",
        );

        // An answer to one question does not displace an answer to another
        // that is still waiting to be read.
        state.expect_adjustments(9);
        state.expect_adjustments(10);
        state.note_adjustments(
            AdjustedContract { con_id: "4815747".into(), ..Default::default() },
            Vec::new(), 9,
        );
        state.note_adjustments(
            AdjustedContract { con_id: "4815747".into(), ..Default::default() },
            Vec::new(), 10,
        );
        assert!(
            state.take_adjustments_answering(9).is_some(),
            "the first answer survives the second arriving before anyone read it",
        );

        // An answer nobody said they would wait for is dropped, so a session
        // that keeps asking without waiting does not keep growing.
        state.note_adjustments(
            AdjustedContract { con_id: "4815747".into(), ..Default::default() },
            Vec::new(), 11,
        );
        assert!(
            state.take_adjustments_answering(11).is_none(),
            "nobody waited on request 11, so its answer had nowhere to go",
        );
        // A slot made and then given up before any answer arrives leaves
        // nothing behind — which is the case a caller whose request failed to
        // go out at all lands in, and the reason giving up is a guard rather
        // than a line at the end of the happy path.
        state.expect_adjustments(12);
        state.stop_waiting_for_adjustments(12);
        state.note_adjustments(
            AdjustedContract { con_id: "4815747".into(), ..Default::default() },
            Vec::new(), 12,
        );
        assert!(
            state.take_adjustments_answering(12).is_none(),
            "the wait was given up, so the late answer is dropped rather than kept",
        );
        // The plain reader is unchanged: it states what is known about the
        // contract, which is a different question from whose answer it is.
        assert!(state.adjustments_for("4815747").is_some());
    }

    /// What the company series state is kept for a bounded number of
    /// contracts, and no more.
    ///
    /// Kept for every contract ever watched, a session sweeping a scanner
    /// across thousands holds all of them for as long as it runs.
    #[test]
    fn what_a_contract_stated_is_kept_for_a_bounded_number_of_contracts() {
        let state = ReferenceState::new();
        let cap = super::COMPANY_DATA_HELD as u32;

        for con_id in 1..=cap {
            state.note_company_data(con_id, 434, vec![("RATING".into(), "2".into())]);
        }
        assert_eq!(
            state.company_data(1, 434).len(), 1, "the first is still held at the cap",
        );

        // One more contract, and the one heard of longest ago makes way.
        state.note_company_data(cap + 1, 434, vec![("RATING".into(), "3".into())]);
        assert!(
            state.company_data(1, 434).is_empty(),
            "the contract heard of longest ago was kept past the cap",
        );
        assert_eq!(
            state.company_data(cap + 1, 434),
            vec![("RATING".to_string(), "3".to_string())],
            "and the newest is held",
        );
        assert_eq!(state.company_data(2, 434).len(), 1, "the rest are untouched");

        // A contract stating a second series is not a second contract, so it
        // costs nothing against the cap.
        state.note_company_data(cap + 1, 548, vec![("SIRECOMM1".into(), "9".into())]);
        assert_eq!(state.company_data_series(cap + 1), vec![434, 548]);
        assert_eq!(state.company_data(2, 434).len(), 1, "nothing else made way for it");
    }
}

#[cfg(test)]
mod attached_definition_tests {
    use super::*;

    #[test]
    fn full_definitions_keep_exchange_specific_rules_and_the_default() {
        let state = ReferenceState::new();
        let mut definition = ContractDefinition {
            con_id: 42, exchange: "SMART".into(), market_rule_id: Some(26),
            min_tick: 0.25, price_magnifier: 100, under_sec_type: "CASH".into(),
            order_types: vec!["LMT".into(), "STP".into()], ..Default::default()
        };
        state.cache_contract_definition(definition.clone());
        definition.exchange = "ISLAND".into();
        definition.market_rule_id = Some(27);
        state.cache_contract_definition(definition);
        let smart = state.contract_definition(42, "BEST").unwrap();
        assert_eq!(smart.market_rule_id, Some(26));
        assert_eq!((smart.min_tick, smart.price_magnifier), (0.25, 100));
        assert_eq!(smart.under_sec_type, "CASH");
        assert_eq!(smart.order_types, ["LMT", "STP"]);
        assert_eq!(state.contract_definition(42, "NASDAQ").unwrap().market_rule_id, Some(27));
        assert_eq!(state.contract_definition(42, "NYSE").unwrap().market_rule_id, Some(27));
        assert!(state.contract_definition(43, "SMART").is_none());
    }

    #[test]
    fn dispatched_definition_values_survive_draining_the_callback_queue() {
        let state = ReferenceState::new();
        state.push_contract_details(1, ContractDefinition {
            con_id: 42, exchange: "SMART".into(), market_rule_id: Some(26), ..Default::default()
        });
        assert_eq!(state.drain_contract_details().len(), 1);
        assert_eq!(state.contract_definition(42, "SMART").unwrap().market_rule_id, Some(26));
        state.cache_contract_definition(ContractDefinition::default());
        assert!(state.contract_definition(0, "").is_none());
    }

    #[test]
    fn attached_types_use_the_associated_names_and_first_simulation_code() {
        let state = ReferenceState::new();
        let definition = crate::control::contracts::parse_secdef_response(
            b"35=d\x0155=ABC\x01167=CS\x016008=42\x01207=BEST\x016430=stock\x016432=1\x016430=stock\x016431=STP/4,STP/1,STPLMT/2,TRAIL/0,TRAILLMT/3,STPPROT/5\x01", false,
        ).unwrap();
        state.cache_contract_definition(definition);
        let definition = state.contract_definition(42, "SMART").unwrap();
        assert!(!state.supports_attached_order_type(&definition, "STP"));
        for kind in ["STP LMT", "TRAIL", "TRAIL LIMIT", "STP PRT"] {
            assert!(state.supports_attached_order_type(&definition, kind), "{kind}");
        }
        assert!(!state.supports_attached_order_type(&definition, "MIT"));
        let mut refreshed = definition.clone();
        refreshed.order_type_rules = vec![("STP".into(), 1), ("MIT".into(), 1)];
        state.cache_contract_definition(refreshed);
        let refreshed = state.contract_definition(42, "SMART").unwrap();
        assert!(!state.supports_attached_order_type(&refreshed, "STP"));
        assert!(!state.supports_attached_order_type(&refreshed, "MIT"));
        assert!(state.supports_attached_order_type(&refreshed, "TRAIL"));
    }

    #[test]
    fn a_repeated_rule_id_updates_the_stated_ladder() {
        let state = ReferenceState::new();
        state.push_market_rules(crate::control::contracts::parse_market_rules(
            b"6019=1\x016031=26\x016026=1\x016023=0\x016027=0.01\x01"
        ));
        state.push_market_rules(crate::control::contracts::parse_market_rules(
            b"6019=1\x016031=26\x016026=1\x016023=0\x016027=0.05\x01"
        ));
        assert_eq!(state.market_rules().len(), 1);
        assert_eq!(state.market_rule(26).unwrap().price_increments[0].increment, 0.05);
    }
    #[test]
    fn attached_combo_definitions_without_identifiers_keep_their_type_table() {
        let state = ReferenceState::new();
        let definition = ContractDefinition { sec_type: crate::control::contracts::SecurityType::Combo,
            symbol: "ABC".into(), exchange: "SMART".into(), currency: "USD".into(),
            order_type_key: "combo".into(), order_type_rules: vec![("STP".into(), 0), ("TRAIL".into(), 4)],
            ..Default::default()
        };
        state.cache_contract_definition(definition.clone());
        let mut changed = definition;
        changed.order_type_rules = vec![("TRAIL".into(), 0)];
        state.cache_contract_definition(changed);
        let definition = state.combo_definition("ABC", "SMART", "USD").unwrap();
        assert!(state.supports_attached_order_type(&definition, "STP"));
        assert!(!state.supports_attached_order_type(&definition, "TRAIL"));
        assert!(!state.supports_attached_order_type(&definition, "MIT"));
    }

}
