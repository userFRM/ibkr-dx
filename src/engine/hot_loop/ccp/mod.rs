use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// How long a matching-symbols request is held here before it is given up on.
///
/// Shorter than the caller's own wait, which is the rule every deadline the
/// engine keeps has to follow and the reason `NAMING_TIMEOUT` below is written
/// the same way. Held longer, as these two were, the caller gives up first and
/// is told nothing arrived; the reply then arrives, is matched to the entry the
/// caller abandoned, and is delivered under a number it never issued — while
/// the retry it made in the meantime waits behind that entry and is given up on
/// in its turn. A single timeout left the request unanswerable for as long as
/// the difference lasted.
const MATCHING_SYMBOLS_TIMEOUT: Duration =
    Duration::from_secs(crate::config::ANSWER_TIMEOUT_SECS - 3);

/// The same, for a question or a replacement put to the advisor
/// configuration.
///
/// It had none: a request the venue took and never answered was given up on
/// only when the connection next went, which on a session that stays up is
/// never. A caller reading a partition waited for as long as the process ran,
/// and one replacing a partition was never told whether its document had been
/// taken.
const ADVISOR_TIMEOUT: Duration =
    Duration::from_secs(crate::config::ANSWER_TIMEOUT_SECS - 3);

/// How many finished orders are assembled before the answer is handed over
/// whether or not the venue has said it is done.
///
/// A window the venue never ends stays open, which is right — but it must not
/// mean an unbounded number of orders held in the engine for the rest of a
/// connection that never drops.
const FINISHED_ORDERS_HELD: usize = 4_096;

/// One order the venue has finished, as the reports about it are merged.
///
/// Its status and what it filled are the latest any report stated; every other
/// field is the last report that stated it, because a report states what
/// changed and says nothing about the rest.
#[derive(Clone)]
pub(crate) struct FinishedOrder {
    /// The id every report about this order names it by.
    pub(crate) order_id: u64,
    /// The venue's own name for it, which tells it from another order sent
    /// under the same number.
    pub(crate) venue_order: String,
    /// The contract, as far as the reports have stated it.
    pub(crate) contract: crate::types::model::Contract,
    /// The order itself, the same way.
    pub(crate) order: crate::types::model::Order,
    /// What became of it, from the latest report that said.
    pub(crate) status: crate::types::OrderStatus,
    /// And how much of it filled.
    pub(crate) filled: i64,
    /// What became of it, as the venue stated it: the time it finished and the
    /// reason it was refused, both of which only the report carries.
    pub(crate) state: crate::types::model::OrderState,
    /// The order type as the wire states it, kept because four kinds travel
    /// under one letter and the instruction below is what tells them apart.
    pub(crate) ord_type: String,
    /// That instruction, kept for the same reason.
    pub(crate) exec_inst: String,
}

/// How many given-up-on dividend queries are remembered, so a late answer can
/// still be attributed. A second chance for the few most recent, not a record.
const GIVEN_UP_ON_DIVIDEND_QUERIES: usize = 64;

/// How long a question of what a contract pays out is waited on before it is
/// given up on, and may be asked again.
///
/// The question of what the venue has finished is not on a clock: its answer
/// names no question, so a window shut early handed the rest of the answer to
/// the next one. It is over at its sentinel or with its connection.
const COMPLETED_ORDERS_TIMEOUT: Duration =
    Duration::from_secs(crate::config::ANSWER_TIMEOUT_SECS - 6);

/// How long the question waits for the replay of what the account is working.
///
/// The wait every other reader of that replay keeps, and for the same reason:
/// an account with nothing working never names an order, so the replay ends
/// without saying so and this is what says it has had long enough. Held for as
/// long as the *window* instead, one question spent twelve seconds waiting and
/// the window behind it another twelve, against a caller that waits fifteen —
/// so the caller was handed an empty answer, or paid twelve seconds for one the
/// venue gives at once, on every call and on exactly the accounts most likely
/// to ask.
const COMPLETED_ORDERS_HOLD: Duration = crate::bridge::REPLAY_WAIT;

/// A search's deadline may not outlive the wait the caller keeps. Stated here
/// so a change to either, or to the caller's wait, stops the build rather than
/// quietly making the request unanswerable again.
const _: () = assert!(
    MATCHING_SYMBOLS_TIMEOUT.as_secs() < crate::config::ANSWER_TIMEOUT_SECS,
    "an engine deadline must be shorter than the wait the caller keeps",
);

pub(crate) use crate::config::OPENING_ACCOUNT_REQUEST;
use crate::bridge::{Event, SharedState};
use crate::engine::context::Context;
use crate::protocol::datetime::chrono_free_timestamp;
use crate::protocol::connection::Connection;
use crate::protocol::fix;
use crate::types::{
    InstrumentId, NewsBulletin,
    PositionInfo,
};

use super::{HeartbeatState, emit, clone_for_event, parse_price_tag, decode_tif, EventSink};

/// How long a contract-details request may go unanswered before it is ended.
///
/// A request always finishes, because something is waiting on it: ended here,
/// the caller gets error 200 and the end of the request rather than a wait
/// with nothing at the end of it.
///
/// Refreshed whenever the venue speaks on the request — a definition, or
/// fan-out activity — so it measures silence rather than the length of an
/// answer. A class naming every expiry sends for as long as that takes.
///
/// This client's own number, not one the venue states. Held below the wait of
/// whatever is covering the request: a lookup this session took to name a
/// contract is covered by [`CcpState::NAMING_TIMEOUT`], which reports in its
/// place and must not report first.
const SECDEF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What a caller is told when a replacement of the advisor's configuration
/// stands, and the code a refused one is reported under.
///
/// Both are the reference client's own, in its own words: the venue states no
/// text at all on a replacement that stands, and a program written against
/// that client reads these.
const ADVISOR_SAVED: &str = "FA data saved";
const ADVISOR_SAVE_REFUSED: i32 = 10229;

/// The same, for a lookup a caller asked for.
///
/// A caller can name a whole class, which the venue takes about ten seconds to
/// begin answering, so this is twice that. Nothing covers such a lookup: the
/// caller waits on it directly, under a wait of its own that this is held
/// below.
const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// How long this request may go unanswered, by who is waiting on it.
///
/// A request numbered in the internal band is one this session took for
/// itself, and the fallback that covers it reports sooner than this. One a
/// caller asked for has nothing covering it and can name a whole class.
fn unanswered_after(req_id: u32) -> std::time::Duration {
    if req_id >= crate::bridge::ENGINE_ID_BASE { SECDEF_TIMEOUT } else { LOOKUP_TIMEOUT }
}

/// Number of most-recent ExecIDs retained for fill deduplication. Bounds the
/// memory of `seen_exec_ids` while staying large enough that a server replay
/// after a reconnect burst still hits the window.
const EXEC_ID_WINDOW: usize = 1024;

/// How many of the venue's own names for recovered orders are held at once.
///
/// One is learned per order the venue replays, and a caller may ask for what
/// the account has finished as often as it likes, so this is a window rather
/// than a record of everything: the oldest goes when a new one arrives. The
/// names serve a withdrawal or a replacement of an order that is still
/// working, and an order that finished long ago needs none.
const WIRE_NAME_WINDOW: usize = 4_096;

/// Extract the value of a single FIX tag from a raw message.
/// `prefix` should include the tag number and `=` (e.g. `b"6256="`).
fn extract_tag_value(msg: &[u8], prefix: &[u8]) -> Option<String> {
    use crate::protocol::fix::SOH;
    for part in msg.split(|&b| b == SOH) {
        if part.starts_with(prefix) {
            return Some(String::from_utf8_lossy(&part[prefix.len()..]).into_owned());
        }
    }
    None
}

/// What a fill cost, as the venue states it.
///
/// The execution report carries no commission tag — captured against real
/// fills on two instruments, it simply is not there — so a charge taken from
/// the report is always nothing. The venue states it on a record of its own
/// that follows the report, naming the execution it belongs to, the amount
/// and the currency it is charged in.
///
/// The quantities and the price on this record are the same fill the
/// execution report already carried, and are left alone: booking the fill
/// from both is how it would be counted twice. Only what it cost is taken.
fn handle_trade_charge(parsed: &std::collections::HashMap<u32, String>, shared: &SharedState) {
    let Some(exec_id) = parsed.get(&fix::TAG_EXEC_ID).filter(|s| !s.is_empty()) else {
        return;
    };
    // Absent is not nothing: a charge the venue did not state is unstated,
    // and reporting a zero for it is the number this was written to stop.
    let Some(charged) = parsed.get(&fix::TAG_TRADE_CHARGE)
        .and_then(|s| s.parse::<f64>().ok())
    else {
        return;
    };
    shared.orders.push_charge(crate::types::model::CommissionAndFeesReport::charged(
        exec_id,
        charged,
        parsed.get(&fix::TAG_TRADE_CHARGE_CURRENCY).map(String::as_str).unwrap_or(""),
    ));
}

/// What the venue says went wrong.
///
/// It states the trouble as text and gives it no code and no severity, and for
/// all but a narrow family of requests it does not say which request failed.
/// So this reports the text against no request, which is what it is, rather
/// than guessing at an owner for it.
fn handle_venue_error(parsed: &std::collections::HashMap<u32, String>, shared: &SharedState) {
    let text = parsed.get(&58).map(String::as_str).unwrap_or("");
    if text.is_empty() {
        // The venue reported trouble and stated nothing about it. There is
        // nothing to hand a caller, so this is recorded and not forwarded.
        log::warn!("The venue reported trouble and stated nothing about it");
        return;
    }
    // A code-like identifier travels separately from the text where it travels
    // at all, so it is carried along rather than parsed into a number the
    // venue never stated.
    let told = match parsed.get(&149).map(String::as_str).filter(|id| !id.is_empty()) {
        Some(id) => format!("{text} ({id})"),
        None => text.to_string(),
    };
    // And the account it is about, where the venue named one. A login holding
    // several is told about each of them on this channel, and a margin notice
    // or a corporate-action notice that does not say which account it concerns
    // is close to useless to the person it was written for. The venue said
    // which one; this dropped it.
    let told = match parsed.get(&1).map(String::as_str).filter(|a| !a.is_empty()) {
        Some(account) => format!("{account}: {told}"),
        None => told,
    };
    log::warn!("The venue reported: {told}");
    shared.market.push_venue_error(told);
}

/// The venue restating a working order's terms.
///
/// It names the order and states only what it has changed — the venue it is
/// working on, its limit, or both — so what it does not state is what the
/// order already held. The revised order goes back to the caller the way any
/// other change to it does.
impl CcpState {
    fn handle_attached_combo_answer(&self, message: &[u8], shared: &SharedState) {
        let parsed = crate::control::contracts::tag_sequence(message);
        let field = |tag| parsed.iter().find(|(key, _)| *key == tag).map(|(_, value)| value.as_str());
        match field(6040) {
            Some("7" | "36") => {
                if let Some(key) = field(320) {
                    shared.reference.answer_attached_combo_confirmation(key, message.to_vec());
                }
                shared.reference.push_market_rules(crate::control::contracts::parse_market_rules(message));
            }
            Some("154") => {
                for (exchange, rules) in crate::control::attached_combos::rules_response(message) {
                    shared.reference.set_attached_combo_rules(exchange, rules);
                }
            }
            _ => {},
        }
    }

    /// Send a user message (35=U) carrying `body`.
    pub(crate) fn send_user_message(
        &mut self,
        body: Vec<(u32, String)>,
        connection: &mut Option<Connection>,
        heartbeat: &mut HeartbeatState,
    ) -> std::io::Result<()> {
        let connection = connection.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotConnected, "the trading connection is not open")
        })?;
        let mut fields = vec![
            (fix::TAG_MSG_TYPE, "U".to_string()),
            (fix::TAG_SENDING_TIME, chrono_free_timestamp().to_string()),
        ];
        fields.extend(body);
        let fields: Vec<_> = fields.iter().map(|(tag, value)| (*tag, value.as_str())).collect();
        connection.send_fix(&fields)?;
        heartbeat.last_ccp_sent = Instant::now();
        Ok(())
    }

    fn handle_order_preset_answer(&self, message: &[u8], shared: &SharedState) {
        if let Some(values) = crate::control::order_presets::parse_values(message) {
            shared.reference.set_order_preset_values(values);
        } else if let Some(presets) = parse_order_presets(message) {
            shared.reference.set_order_presets(presets);
        }
    }

fn handle_order_revision(
    &self,
    parsed: &std::collections::HashMap<u32, String>,
    context: &Context,
    shared: &SharedState,
) {
    // Named the way every other report names an order: a cancel carries a
    // leading C and a liquidation a leading L, a revision chain carries a
    // suffix, and a recovered order is named by the venue's own permanent
    // name rather than by the number a caller addresses it under. Parsed as a
    // plain number instead, a revision for a recovered order — which is every
    // order the account already had — either resolved to nothing and was
    // dropped, or resolved to an unrelated live order that happened to be
    // numbered the venue's permanent name for this one, and wrote this order's
    // venue and limit onto that one.
    let Some(named) = parsed.get(&11) else { return };
    let stripped = named.strip_prefix('C').or_else(|| named.strip_prefix('L')).unwrap_or(named);
    let base = stripped.split('.').next().unwrap_or(stripped);
    let Some(named) = executions::stated_order_id(base) else {
        return;
    };
    // Read back through the names this session learned at recovery, and only
    // where nothing is working under the number itself: an order under that
    // number is the order that number means.
    let order_id = match context.order(named) {
        Some(_) => named,
        None => self.the_order_named(named).unwrap_or(named),
    };
    let Some(mut held) = shared.orders.get_order_info(order_id) else {
        // An order this session holds no account of. The venue states the
        // change against the order, and there is nothing here to state it on.
        log::debug!("the venue revised order {order_id}, which is not held here");
        return;
    };
    let mut changed = false;
    if let Some(exchange) = parsed.get(&30).filter(|at| !at.is_empty()) {
        // The venue an order works on is the contract's, which is what a
        // caller reads it off.
        held.contract.exchange = exchange.clone();
        changed = true;
    }
    if let Some(price) = parsed.get(&44).and_then(|p| p.parse::<f64>().ok()) {
        held.order.lmt_price = price;
        changed = true;
    }
    if !changed {
        return;
    }
    log::info!("the venue revised order {order_id}");
    shared.orders.push_order_info(order_id, held);
}
}

/// Why a message this client receives is deliberately not read.
///
/// Told apart from one nobody has looked at yet. Both are discarded, but only
/// one of them is a gap, and a diagnostic that cannot tell them apart is one
/// nobody keeps listening to.
///
/// Only what carries nothing this client needs belongs here. The order presets
/// were named here once as a user interface's defaults; they are not. The
/// venue fills the compete size and the offset of a pegged-best order from
/// them when the caller states neither, so an order sent from here differs
/// from the same order sent elsewhere. They are unread, and now counted as the
/// gap that is.
fn known_unread(subtype: &str) -> Option<&'static str> {
    match subtype {
        "93" => Some(
            "it answers the account and position subscription this client sends, and carries \
             the account, that request's own id and two flags: on this account, nothing the \
             subscription does not deliver itself. It is named as a dimension response",
        ),
        "102" => Some(
            "it is the exchange directory, each exchange and the name it goes by. Which \
             exchanges serve a book, and which book, is the market-data routing table's to \
             say, and a caller asking is answered from that",
        ),
        _ => None,
    }
}

/// The order presets a session is told about, as `(key, attributes, changed
/// at)`.
///
/// The venue states how many follow on 8167 and then repeats three fields for
/// each: the key it names the set by on 8168, the set's attributes on 8169 —
/// `&` between them, `v=` its variant and `a=1` where it is active — and the
/// moment it last changed on 8170. The values in a set are not here; asking
/// for those is a request of its own.
///
/// The moment is carried: it says *when* a set changed, which is what tells a
/// caller whether an order it sent at a given time was filled in from the old
/// defaults or the new — and these defaults fill in terms the caller left
/// unstated on orders already placed. Read past, the question could not be
/// asked. A set the venue states no moment for carries none rather than being
/// dropped for want of it.
///
/// Read by walking the tags in the order the message states them rather than
/// by looking each up, because three of them repeat and a keyed read answers
/// with whichever came last.
///
/// Nothing where the venue's own count and what arrived disagree, and nothing
/// where it stated no count. The count is what says the message is whole, so
/// its absence is not proof of anything: read without it, a truncated answer
/// published however many pairs happened to parse — as the account's defaults,
/// beside a number the venue itself said was larger — and one carrying neither
/// count nor pairs cleared what the account holds.
fn parse_order_presets(msg: &[u8]) -> Option<Vec<(String, String, String)>> {
    if !crate::control::contracts::tag_sequence(msg).iter()
        .any(|(tag, value)| *tag == 8166 && value == "L")
    {
        return None;
    }
    let mut out = Vec::new();
    let mut key: Option<String> = None;
    let mut stated: Option<usize> = None;
    for (tag, value) in crate::control::contracts::tag_sequence(msg) {
        match tag {
            8167 => stated = value.parse().ok(),
            8168 => key = Some(value),
            8169 => {
                if let Some(k) = key.take() {
                    out.push((k, value, String::new()));
                }
            }
            // Written onto the set the attributes just opened, because it
            // follows them. A set the venue states no moment for keeps the
            // empty one it was pushed with rather than taking the previous
            // set's.
            8170 => {
                if let Some(held) = out.last_mut() {
                    held.2 = value;
                }
            }
            _ => {}
        }
    }
    // The count is what says the message is whole, so a message that does not
    // state one is not whole either. Accepted without it, a truncated answer
    // published however many pairs happened to parse — and one carrying
    // neither count nor pairs cleared the sets this account holds as though
    // the venue had said it holds none.
    match stated {
        Some(n) if n == out.len() => Some(out),
        _ => None,
    }
}

/// The algorithms the venue offers, keyed `PROVIDER/SECTYPE`.
///
/// Stated once, unasked, after logon. Nothing here read it, so a caller had no
/// way to know which algorithms this account may use and would find out by
/// having an order refused.
///
/// `FOXRIVER/STK:FOXRIVER-AE,FOXRIVER-AL-COMMON;IBALGO/BAG:IBALGO-AE`
fn parse_algorithms(raw: &str) -> std::collections::HashMap<String, Vec<String>> {
    raw.split(';')
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let (key, names) = entry.split_once(':')?;
            let names: Vec<String> = names
                .split(',')
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .collect();
            Some((key.to_string(), names))
        })
        .collect()
}

fn handle_algorithms(parsed: &std::collections::HashMap<u32, String>, shared: &SharedState) {
    let Some(raw) = parsed.get(&6597) else { return };
    let offered = parse_algorithms(raw);
    if offered.is_empty() {
        return;
    }
    log::info!("Algorithms offered on {} provider and security type pairs", offered.len());
    shared.reference.set_algorithms(offered);
}

/// The account's own configuration, which states further feature tokens on the
/// same tag the logon used. Read only from the logon, the list is short by
/// whatever this adds.
fn handle_account_config(parsed: &std::collections::HashMap<u32, String>, shared: &SharedState) {
    let Some(raw) = parsed.get(&6542) else { return };
    let more: Vec<String> = raw.split(',').filter(|t| !t.is_empty()).map(str::to_string).collect();
    if more.is_empty() {
        return;
    }
    log::info!("Account configuration states {} further features: {raw}", more.len());
    shared.reference.add_enabled_features(more);
}

/// An advisor request the venue has not answered yet.
///
/// The reply states nothing about what was asked — not which partition, not
/// whether it was a question or a replacement — beyond the number the request
/// went out under, so what the caller is owed is remembered here.
#[derive(Debug, Clone)]
pub(crate) struct PendingAdvisor {
    /// The caller's number for a replacement, carried back on its end.
    pub(crate) req_id: i64,
    /// Which partition, as the reference client numbers it.
    pub(crate) fa_data_type: i32,
    /// Whether the caller was writing rather than reading. A question is
    /// answered with the configuration; a replacement with its end.
    pub(crate) replacing: bool,
    /// When this client stops waiting for the venue to answer it.
    pub(crate) deadline: Instant,
}

impl PendingAdvisor {
    /// What an error about this request is about.
    pub(crate) fn origin(&self) -> crate::types::model::ErrorOrigin {
        Self::origin_of(self.req_id, self.replacing)
    }

    /// A replacement is a numbered request; a question about a partition has
    /// no number of its own.
    pub(crate) fn origin_of(req_id: i64, replacing: bool) -> crate::types::model::ErrorOrigin {
        if replacing {
            crate::types::model::ErrorOrigin::Request { id: req_id, ends: true }
        } else {
            crate::types::model::ErrorOrigin::Question {
                q: crate::types::model::Question::Fa,
                ends: true,
            }
        }
    }
}

/// What a lookup naming an order's contract came back with.
#[derive(Debug, Clone)]
pub(crate) enum OrderNamed {
    /// The one contract the description names.
    Contract(Box<crate::control::contracts::ContractDefinition>),
    /// No contract, or several: how many the venue named.
    Unnamed(usize),
    /// Not answered, and why: the code and the words.
    Refused(i32, String),
}

/// An option chain request held behind one on the wire for the same
/// underlying: what it will be sent with.
#[derive(Debug, Clone)]
pub(crate) struct QueuedChain {
    pub(crate) req_id: u32,
    pub(crate) symbol: String,
    pub(crate) fut_fop_exchange: String,
    pub(crate) underlying_sec_type: String,
    pub(crate) underlying_con_id: i64,
}

/// Why a contract described by symbol is not served: the venue named none
/// for the description, or several, and what could not be done for it.
pub(crate) fn unnamed(sec_type: &str, symbol: &str, exchange: &str, listings: usize, so: &str) -> String {
    if listings > 1 {
        format!(
            "{sec_type} {symbol} on {exchange} matches {listings} contracts, so it names none and \
             {so}: state the currency or the exchange",
        )
    } else {
        format!("no security definition has been found for {sec_type} {symbol} on {exchange}, so {so}")
    }
}

/// The logout a session is owed as it is shut down rather than lost.
pub(crate) fn say_goodbye(conn: &mut Connection) -> std::io::Result<()> {
    let ts = chrono_free_timestamp();
    conn.send_fix(&[
        (fix::TAG_MSG_TYPE, fix::MSG_LOGOUT),
        (fix::TAG_SENDING_TIME, &ts),
        // The vendor states a reason here; "S" is what it sends when the
        // session is being shut down rather than lost.
        (8372, "S"),
    ])?;
    log::info!("Logout sent");
    Ok(())
}

/// The contract a request names, when it carries one.
///
/// Only the requests that must be sent under an id are listed: everything else
/// either carries no contract or is resolved by the venue from the symbol.
pub(crate) fn contract_named(cmd: &crate::types::ControlCommand) -> Option<&crate::types::ContractRef> {
    let contract = contract_of(cmd)?;
    // Only one the venue has not named yet: the rest already carry its id.
    (contract.con_id == 0).then_some(contract)
}

/// The contract a request names, whether or not the venue has numbered it.
pub(crate) fn contract_of(cmd: &crate::types::ControlCommand) -> Option<&crate::types::ContractRef> {
    use crate::types::ControlCommand as C;
    match cmd {
        C::Subscribe { contract, .. } => Some(contract),
        C::FetchHistorical { contract, .. }
        | C::FetchHeadTimestamp { contract, .. }
        | C::FetchHistoricalTicks { contract, .. }
        | C::FetchHistoricalSchedule { contract, .. }
        | C::SubscribeRealTimeBar { contract, .. }
        | C::SubscribeDepth { contract, .. }
        | C::SubscribeTbt { contract, .. } => Some(contract),
        _ => None,
    }
}

/// The venue's id for a contract a request gives by that id alone: no
/// security type or no exchange stated beside it.
///
/// A request states both and the venue routes on both, so both are the
/// venue's to say: asked for by id, it answers with them. Stamped with a guess
/// instead, a future or an option went out as a smart-routed US stock. The
/// requests a caller's call named this way before sending; a book and live
/// bars never were, and a book with no exchange is refused before any lookup.
pub(crate) fn named_by_id_alone(cmd: &crate::types::ControlCommand) -> Option<(i64, &str)> {
    use crate::types::ControlCommand as C;
    let (con_id, sec_type, exchange) = match cmd {
        C::Subscribe { contract, .. } =>
            (contract.con_id, &contract.sec_type, &contract.exchange),
        C::FetchHistogramData { con_id, sec_type, exchange, .. } => {
            (i64::from(*con_id), sec_type, exchange)
        }
        C::FetchHistorical { contract, .. }
        | C::FetchHeadTimestamp { contract, .. }
        | C::FetchHistoricalTicks { contract, .. }
        | C::FetchHistoricalSchedule { contract, .. }
        | C::SubscribeTbt { contract, .. } => (contract.con_id, &contract.sec_type, &contract.exchange),
        _ => return None,
    };
    (con_id != 0 && (sec_type.is_empty() || exchange.is_empty())).then_some((con_id, exchange.as_str()))
}

/// The filters that go with that contract.
fn filters_named(cmd: &crate::types::ControlCommand) -> crate::types::SecDefFilters {
    use crate::types::ControlCommand as C;
    match cmd {
        C::Subscribe { filters, .. } => filters.clone(),
        C::FetchHistorical { filters, .. }
        | C::FetchHeadTimestamp { filters, .. }
        | C::FetchHistoricalTicks { filters, .. }
        | C::FetchHistoricalSchedule { filters, .. }
        | C::SubscribeRealTimeBar { filters, .. }
        | C::SubscribeDepth { filters, .. }
        | C::SubscribeTbt { filters, .. } => filters.clone(),
        _ => crate::types::SecDefFilters::default(),
    }
}

/// What a request other than a details request has a gateway look its
/// contract up by.
///
/// Those requests carry no identifier and no issuer, so a gateway looks the
/// contract up by its description, whatever else the caller's contract held.
fn described(filters: crate::types::SecDefFilters) -> crate::types::SecDefFilters {
    crate::types::SecDefFilters {
        sec_id: String::new(),
        sec_id_type: String::new(),
        issuer_id: String::new(),
        ..filters
    }
}

/// Fill in the id the venue has given the contract a request named.
///
/// A request that gave the contract by id alone takes the contract whole as
/// the venue names it — its type, its exchange and what narrows a lookup of
/// it — as it did when the call named it before sending.
fn name_the_contract(cmd: &mut crate::types::ControlCommand, def: &crate::control::contracts::ContractDefinition) {
    use crate::types::ControlCommand as C;
    let by_id = named_by_id_alone(cmd).is_some();
    let named = crate::types::model::ContractDetails::from_definition(def).contract;
    match cmd {
        C::Subscribe { contract, filters, calculation, .. } => {
            if let Some(asked) = calculation {
                asked.contract = named.clone();
            }
            if by_id {
                *contract = (&named).into();
                *filters = named.lookup_filters();
            } else {
                contract.con_id = def.con_id as i64;
                if contract.sec_type.is_empty() { contract.sec_type = named.sec_type; }
                if contract.exchange.is_empty() { contract.exchange = named.exchange; }
            }
        }
        C::FetchHistorical { contract, filters, .. }
        | C::FetchHeadTimestamp { contract, filters, .. }
        | C::FetchHistoricalTicks { contract, filters, .. }
        | C::FetchHistoricalSchedule { contract, filters, .. }
        | C::SubscribeRealTimeBar { contract, filters, .. }
        | C::SubscribeDepth { contract, filters, .. }
        | C::SubscribeTbt { contract, filters, .. } if by_id => {
            *contract = (&named).into();
            *filters = named.lookup_filters();
        }
        C::FetchHistogramData { sec_type, exchange, .. } if by_id => {
            sec_type.clone_from(&named.sec_type);
            exchange.clone_from(&named.exchange);
        }
        C::FetchHistorical { contract, .. }
        | C::FetchHeadTimestamp { contract, .. }
        | C::FetchHistoricalTicks { contract, .. }
        | C::FetchHistoricalSchedule { contract, .. }
        | C::SubscribeRealTimeBar { contract, .. }
        | C::SubscribeDepth { contract, .. }
        | C::SubscribeTbt { contract, .. } => contract.con_id = def.con_id as i64,
        _ => {}
    }
}

/// Which request a caller is waiting on.
pub(crate) fn request_id(cmd: &crate::types::ControlCommand) -> Option<u32> {
    match cmd {
        crate::types::ControlCommand::Subscribe { req_id, .. } => u32::try_from(*req_id).ok(),
        crate::types::ControlCommand::FetchHistorical { req_id, .. }
        | crate::types::ControlCommand::FetchHeadTimestamp { req_id, .. }
        | crate::types::ControlCommand::FetchHistoricalTicks { req_id, .. }
        | crate::types::ControlCommand::FetchHistoricalSchedule { req_id, .. }
        | crate::types::ControlCommand::SubscribeRealTimeBar { req_id, .. }
        | crate::types::ControlCommand::SubscribeDepth { req_id, .. } => Some(*req_id),
        crate::types::ControlCommand::SubscribeTbt { req_id, .. } => u32::try_from(*req_id).ok(),
        crate::types::ControlCommand::FetchHistogramData { req_id, .. } => Some(*req_id),
        _ => None,
    }
}

/// Which tag carries a maturity.
///
/// A full expiry date is MaturityDate (541) and a contract month is
/// MaturityMonthYear (200); they are not interchangeable, and an option asked
/// for by date on tag 200 matches nothing at all. Anything too short to be
/// either is left off rather than sent on a guess.
pub(crate) fn maturity_tag(maturity: &str) -> Option<u32> {
    match maturity.len() {
        6 => Some(200),
        n if n >= 8 => Some(541),
        _ => None,
    }
}

/// How long a reconnect waits for the recovery push before judging the orders
/// it did not mention. Generous, because a push that says nothing at all is
/// indistinguishable from one that has not started.
const RECOVERY_PUSH_GRACE: Duration = Duration::from_secs(30);

/// The same wait once the push has sent its own terminator. What is coming has
/// come; this only covers a fill report arriving just behind it.
const RECOVERY_TERMINATOR_GRACE: Duration = Duration::from_secs(2);

pub(crate) struct CcpState {
    pub(crate) seen_exec_ids: HashSet<String>,
    /// Insertion order for `seen_exec_ids`, oldest at the front. Used to evict
    /// one entry at a time once the dedup window is full, instead of clearing
    /// the whole set — a wholesale clear would let a post-reconnect server
    /// replay of a recently-seen ExecID double-count a fill.
    pub(crate) exec_id_order: VecDeque<String>,
    pub(crate) disconnected: bool,
    /// When to account for orders the reconnect did not explain.
    ///
    /// An order that terminated while the connection was down leaves no
    /// message behind — its evidence is an absence, and absence only means
    /// something once the recovery push is known to be complete. Armed
    /// generously at reconnect so the sweep still runs when the push says
    /// nothing at all, and re-armed tightly when the push's own terminator
    /// arrives. Cleared on a disconnect so a second drop before the
    /// sweep cancels it rather than reaping against a dead session.
    pub(crate) recovery_sweep_at: Option<Instant>,
    /// Whether this connection has hydrated an order from the server's account
    /// of what is working. Separates the replay's terminator from the echo that
    /// looks like it.
    pub(crate) hydrated_any: bool,
    /// (req_id, is_single_shot). Single-shot = known-conId lookup whose
    /// first 35=d reply is also the last (server emits no 323=5/6 terminator
    /// for these). Multi-record by-symbol/matching-symbols requests push
    /// `false` and rely on the response-type sentinel for is_last.
    /// In-flight secdef requests: (req_id, single_shot, deadline). The
    /// deadline is swept by `sweep_contract_details` so a request the
    /// gateway never answers (SessionReject, dead socket, lost reply)
    /// surfaces error 200 + contract_details_end instead of hanging
    /// forever.
    pub(crate) pending_secdef: Vec<(u32, bool, Instant)>,
    /// Requests awaiting a matching-symbols reply, with the deadline after
    /// which one is given up on. Recorded only for a request that actually went
    /// out, and expired so a stale head cannot absorb a later reply.
    pub(crate) pending_matching_symbols: Vec<(u32, Instant)>,
    /// A search whose caller was told it went unanswered, still in flight.
    ///
    /// The venue may answer a search without naming it, so a reply names no
    /// request but the one on the wire. Given up on and forgotten, the late
    /// reply was read as the next search's answer. So the search stays in
    /// flight until the venue replies to it, refuses it, or the connection
    /// ends, and its reply is then dropped.
    pub(crate) matching_symbols_abandoned: Option<u32>,
    /// Searches waiting for the one on the wire, in the order they were asked.
    pub(crate) queued_matching_symbols: VecDeque<(u32, String)>,
    /// Whether the venue is in the middle of stating what it has finished.
    ///
    /// The answer to that question is a run of ordinary execution reports for
    /// orders this session never placed, ending with the sentinel that ends
    /// the opening replay. Inside the window they are filed as history; the
    /// live path never sees them, because through it a report that states a
    /// fill is a fill and a fill moves a position.
    pub(crate) completed_orders_open: bool,
    /// The answer being assembled, one entry per order the venue has finished.
    ///
    /// Each order's life arrives as several reports, each stating what changed
    /// and leaving the rest out, and they are not always adjacent. So the
    /// answer is built here and handed over whole when the venue says it has
    /// finished — rather than one record per report, which made a caller
    /// choose between the first report's fields and the last report's status
    /// and could not give them both.
    ///
    /// In the order the venue stated them, so that is the order a caller hears
    /// about them in.
    finished_orders: Vec<FinishedOrder>,
    /// The wire name an order is reported under, against the number this
    /// session knows it by.
    ///
    /// The venue names a recovered order two ways. Its recovery report states
    /// the permanent name on tag 11 and the number an API gave it beside
    /// that, and this session takes the second, because that is the number a
    /// caller withdraws it by. Every later report about the order states only
    /// the first. Looked up under that, the order this session is tracking was
    /// not found: a fill on it was booked against nothing, and while a
    /// finished-orders window was open it was filed as history instead.
    ///
    /// A rolling window, oldest out first, the way the execution ids beside it
    /// are held: a caller asking repeatedly for what the account has finished
    /// teaches this session a name per replayed order, and unbounded it grew
    /// for as long as the connection lasted.
    wire_name_to_order: std::collections::HashMap<u64, u64>,
    /// The order those names were learned in, so the oldest can go first.
    wire_names_learned: VecDeque<u64>,
    /// Whether the answer being assembled has taken all the orders it can.
    ///
    /// Said once rather than per report: past the bound every report the venue
    /// states about an order the answer does not already hold is left out of
    /// it, and that is one thing that happened, not thousands.
    the_answer_is_full: bool,
    /// Every order this answer has taken, handed over or still being
    /// assembled, by its number and the venue's own name for it.
    ///
    /// The bound is on the answer, not on what is waiting to be handed over: a
    /// handover empties the records and the next order was taken beside them,
    /// so an answer with no end in sight grew without limit however often it
    /// was handed over. Kept here, an order already taken is still merged as
    /// the venue states more about it, and only an order the answer has never
    /// seen is left out.
    orders_in_this_answer: std::collections::HashSet<(u64, String)>,
    /// A caller asked what the venue has finished before the session's own
    /// replay was over, so the question is held until it is.
    ///
    /// The two answers end with the same sentinel and the wire correlates
    /// neither, so a window opened across the replay is closed by the replay's
    /// own ending — the caller is told the answer is complete before it has
    /// begun, and the reports that follow take the live path, where a report
    /// that states a fill is a fill. Worse, the replayed orders themselves
    /// arrive inside that window and are filed as history instead of being
    /// recovered into the book a withdrawal walks.
    completed_orders_wanted: Option<Instant>,
    /// Which turn of the question the window that is open belongs to.
    ///
    /// The answer is a run of ordinary reports and says nothing about which
    /// question it answers, so the turn the caller asked on travels with the
    /// question and comes back on its end: a caller that gave up leaves its
    /// answer on its way, and the next caller must not be released by it.
    completed_orders_asked_on: u64,
    /// The turns of the questions held behind that window, or behind the
    /// replay, in the order they were asked. Each is sent once the one before
    /// it is over: the answer names no question, and two on the wire share one
    /// sentinel.
    completed_orders_queued: VecDeque<u64>,
    /// Which question each turn is, for the answer its end delivers: whether
    /// only the orders an API placed were asked for.
    completed_orders_api_only: HashMap<u64, bool>,
    /// The turn the next question is asked on.
    completed_orders_next_turn: u64,
    /// When the question stops waiting for the replay of the working orders.
    ///
    /// Held against the connection rather than against the question: the
    /// replay happens once per connection, so a question asked after it has
    /// had its time is not held at all. Armed against each question instead,
    /// every call paid the hold again.
    replay_hold_until: Option<Instant>,
    /// In-flight option chain requests: (req_id, symbol, underlying conId).
    /// The request states no id of its own, so the symbol is what ties a reply
    /// back to it, and the conId is held because the callback names the
    /// underlying the caller asked about.
    ///
    /// One per symbol and underlying, and none given up on at a deadline: a
    /// reply naming no request is the next request's for that underlying once
    /// the one it answers has been let go of. A chain is over at its reply, a
    /// refusal of it, or the end of the connection that carried it.
    pub(crate) pending_option_params: Vec<(u32, String, i64)>,
    /// Chain requests waiting for the one on the wire for the same symbol and
    /// underlying, in the order they were asked.
    pub(crate) queued_option_params: VecDeque<QueuedChain>,
    /// Dividend queries, as `(the id it went out under, the contract it is
    /// about, when to stop waiting)`.
    ///
    /// The query goes out as text and the answer echoes only the id, so this
    /// is what says which contract an answer is about. Two queries in flight
    /// on one contract cannot happen — a second is not sent while the first is
    /// outstanding — but two on different contracts can, and read by contract
    /// rather than by id the second schedule would be filed under the first.
    ///
    /// An entry is given up on at its deadline. Kept until the answer came, a
    /// query the venue never answered held that contract for the life of the
    /// session and one more entry here for every underlying it happened to —
    /// so the list grew without bound and the contract could never be asked
    /// about again.
    pending_dividends: Vec<(String, u32, Instant)>,
    /// The contracts already asked about and answered.
    ///
    /// A schedule is a fact about the contract rather than a subscription, so
    /// it is asked for once. Told apart from the list above because that one
    /// empties as answers arrive: with only that, the next option written on
    /// the same underlying asked the same question again and was answered with
    /// the schedule this session already held.
    dividends_answered: std::collections::HashSet<u32>,
    /// The ids of queries given up on, and the contract each was about.
    ///
    /// Given up on says this session may ask again. It does not say the venue
    /// will not answer, and the echoed id is the only thing that says which
    /// contract an answer belongs to — dropped with the entry, a schedule that
    /// arrived a moment late was thrown away with nothing else to attribute it
    /// by. Bounded, oldest first: this is a second chance, not a record.
    dividends_given_up_on: std::collections::VecDeque<(String, u32)>,
    /// The id the next query of this kind goes out under. This client's own
    /// number, distinct from every other request's because the venue echoes
    /// only this one tag back.
    next_xml_query_id: u64,
    /// The currencies whose rates were asked for on this connection, by the
    /// id each question went out under, and the currencies answered: a
    /// currency's rates are asked for once.
    rates_asked: Vec<(String, String)>,
    rates_answered: HashSet<String>,
    /// The keys whose sessions the engine uses, the keys asked for on this
    /// connection and not yet answered, and the day the venue's clock was
    /// last read on: each key is asked for once, and again when the day
    /// turns.
    schedule_keys: Vec<String>,
    schedules_asked: Vec<String>,
    schedules_day: Option<jiff::civil::Date>,
    /// The contracts whose definition was asked for on this connection
    /// because their sessions are wanted and the key they are joined on was
    /// not known: each is asked for once, and its sessions once it answers.
    definitions_asked: Vec<u32>,
    /// Secdef replies awaiting paired schedule reply (joined by tag 6256).
    pub(crate) pending_schedule_pair: Vec<PendingSchedulePair>,
    /// Profit-and-loss subscriptions standing, by request number and account.
    ///
    /// The venue serves one on the connection that asked for it, so a rebuilt
    /// connection is asked for each again, as it is for the account and the
    /// positions. A subscription the caller withdrew is taken out of here and
    /// not renewed.
    pub(crate) pnl_subscriptions: Vec<(i64, bool, String)>,
    /// Counter for internal schedule subscribe req IDs.
    pub(crate) next_schedule_sub_id: u32,
    /// Exchange-definition requests still needed to complete a lookup.
    pub(crate) pending_fanout: Vec<PendingFanout>,
    /// Contract and exchange pairs with a known market-rule scope.
    contract_rule_scopes: HashSet<(u32, String)>,
    /// Contracts already handed to each caller's request. A lookup on a
    /// smart-routed symbol is answered once by the request itself and again by
    /// every venue it fans out to, and every one of those answers describes the
    /// same contract with a different `exchange` — the full venue list already
    /// rides inside each. Delivering them all reported one contract as
    /// twenty-seven listings of itself. Cleared when the request ends.
    pub(crate) details_delivered: std::collections::HashMap<u32, HashSet<i64>>,
    /// A caller's lookup of a continuous future, by the caller's number, while
    /// its answer is put together.
    pub(crate) continuous_lookups: std::collections::HashMap<u32, ContinuousLookup>,
    /// Counter for internal fan-out req IDs (tag 320 on per-exchange `35=c`).
    pub(crate) next_fanout_id: u32,
    /// Counter for internal secdef req IDs (auto-fetch on cold-cache positions).
    pub(crate) next_internal_secdef_id: u32,
    /// The number the next advisor-configuration request states as its own.
    ///
    /// These are counted from one for the session, and the request sends the
    /// count as a string, so a reply can be matched to the question that asked
    /// it.
    pub(crate) next_advisor_request: u32,
    /// The advisor requests waiting on an answer, by the number they went out
    /// under. The venue carries that number back on its reply and nothing
    /// else identifies which question was asked, so a reply arriving with no
    /// entry here belongs to nobody.
    pub(crate) pending_advisor: std::collections::HashMap<String, PendingAdvisor>,
    /// The key the open account subscription was asked for under, so the
    /// withdrawal can name it. Tag 6036 carries whether the request opens the
    /// subscription or closes it; a session that only ever opens them holds one
    /// per loop for the life of the connection.
    pub(crate) account_request_key: Option<String>,
    account_requests: Vec<(String, String)>,
    /// User-message subtypes the venue has sent that nothing here reads, so
    /// each is named once rather than on every arrival.
    unread_subtypes: std::collections::HashSet<String>,
    /// Message types the venue has sent that nothing here reads.
    unread_types: std::collections::HashSet<String>,
    /// Requests that named a contract the venue has not given an id to yet,
    /// keyed by the lookup asking for that id.
    ///
    /// Held whole until naming ends, before registration takes a slot.
    pub(crate) pending_named: Vec<(u32, crate::types::ControlCommand, Instant)>,
    /// Those the venue has now named, ready to be handled as though the id had
    /// been there all along.
    pub(crate) resolved_named: Vec<crate::types::ControlCommand>,
    /// Lookups asked to name the contract an order described, and when each
    /// was asked. The order waits in the engine for the answer.
    pub(crate) order_naming: Vec<(u32, Instant)>,
    /// What those lookups came back with, for the orders waiting on them.
    pub(crate) orders_named: Vec<(u32, OrderNamed)>,
    /// conIds a secdef has been fetched for without a caller asking, and the
    /// request that fetched each. The request is kept so a fetch that is never
    /// answered can be forgotten; held indefinitely, one lost request leaves
    /// that contract unasked for the life of the session and every position on
    /// it unnamed.
    pub(crate) auto_fetched_conids: HashMap<i64, u32>,
    /// Scanner results awaiting per-conId contract-detail enrichment.
    /// Each entry parks a parsed `<ScanResponse>` until every con_id the
    /// cache missed has been resolved via the same 35=d path that user-initiated
    /// `reqContractDetails` uses.
    pub(crate) pending_scanner_enrichment: Vec<PendingScannerEnrichment>,
}

/// Scanner result parked for contract-detail fan-out.
pub(crate) struct PendingScannerEnrichment {
    pub api_req_id: u32,
    pub result: crate::control::scanner::ScannerResult,
    pub awaiting: HashSet<i64>,
    pub deadline: Instant,
}

/// State for a secdef reply awaiting its paired schedule reply.
pub(crate) struct PendingSchedulePair {
    pub api_req_id: u32,
    pub join_key: String,
    pub def: crate::control::contracts::ContractDefinition,
    pub is_last: bool,
    pub deadline: Instant,
}

/// The fields a lookup names its identifier on, or none where it names no
/// identifier a gateway knows.
///
/// `22`/`48` carry every kind under the character that names its source. The
/// kind is read by its exact name, as a gateway reads it: a name it does not
/// know, a lower-case one included, is no identifier at all, and the lookup
/// goes by description with the identifier left out.
fn identifier_fields(filters: &crate::types::SecDefFilters) -> Vec<(u32, &str)> {
    let sec_id = filters.sec_id.as_str();
    if sec_id.is_empty() {
        return Vec::new();
    }
    let source = match filters.sec_id_type.as_str() {
        "CUSIP" => "1",
        "SEDOL" => "2",
        "ISIN" => "4",
        "RIC" => "5",
        "FIGI" => "S",
        "BB_SYMBOL" => "A",
        _ => return Vec::new(),
    };
    vec![(22, source), (48, sec_id)]
}

/// What a caller is told when the venue names no contract for a lookup, in a
/// gateway's words.
const NO_DEFINITION_FOUND: &str = "No security definition has been found for the request";

/// A caller's lookup of a continuous future.
///
/// A gateway asks for the continuous contract first and holds what it names.
/// Where the caller asked for the listed months as well, it asks for those
/// once the continuous answer is in, answer or refusal, and hands them over
/// first, the continuous contract after them. Where the venue names no listed
/// month, the lookup is answered as a contract not found, whatever the
/// continuous lookup named.
pub(crate) struct ContinuousLookup {
    /// Whether the listed months are asked for as well.
    listed_too: bool,
    /// Whether that second lookup is out, so what arrives now is a listed month.
    asking_listed: bool,
    /// Whether a listed month has been handed over.
    listed_found: bool,
    /// The continuous contract as each venue states it, one per venue and
    /// multiplier, the first stated kept.
    held: Vec<crate::control::contracts::ContractDefinition>,
    /// What the second lookup states: the caller's own description, as a future.
    symbol: String,
    exchange: String,
    currency: String,
    filters: crate::types::SecDefFilters,
    include_expired: bool,
}

/// In-flight by-symbol fan-out: per-exchange `35=c` requests sent after
/// the master `35=d` reply. Each per-exchange `35=d` reply (matched by tag
/// 320 string) is forwarded to `api_req_id` as one `contract_details`.
pub(crate) struct PendingFanout {
    pub api_req_id: u32,
    pub fanout_req_ids: Vec<String>,
    /// Which legs have answered.
    ///
    /// The exchanges that have answered. A fan-out ends when every exchange it
    /// asked has answered. Counting frames instead completes it twice over for
    /// a leg answered with more than one row, and drops the legs still
    /// outstanding.
    pub answered: Vec<String>,
    /// Idle deadline, refreshed on every per-exchange reply.
    ///
    /// A fan-out asks each exchange the contract lists on and ends when every
    /// one has answered. One reply lost or unreadable would leave the count
    /// short for good, so this bounds the wait — but reaching it is a failed
    /// request, reported as one, not the ordinary way a fan-out finishes.
    pub deadline: Instant,
}

impl CcpState {
    pub(crate) fn new() -> Self {
        Self {
            seen_exec_ids: HashSet::with_capacity(256),
            exec_id_order: VecDeque::with_capacity(256),
            disconnected: false,
            recovery_sweep_at: None,
            hydrated_any: false,
            pending_secdef: Vec::new(),
            pending_matching_symbols: Vec::new(),
            matching_symbols_abandoned: None,
            queued_matching_symbols: VecDeque::new(),
            completed_orders_open: false,
            finished_orders: Vec::new(),
            wire_name_to_order: std::collections::HashMap::new(),
            wire_names_learned: VecDeque::new(),
            the_answer_is_full: false,
            orders_in_this_answer: std::collections::HashSet::new(),
            completed_orders_wanted: None,
            replay_hold_until: None,
            completed_orders_asked_on: 0,
            completed_orders_queued: VecDeque::new(),
            completed_orders_api_only: HashMap::new(),
            completed_orders_next_turn: 0,
            pending_option_params: Vec::new(),
            queued_option_params: VecDeque::new(),
            pending_dividends: Vec::new(),
            dividends_answered: std::collections::HashSet::new(),
            dividends_given_up_on: std::collections::VecDeque::new(),
            next_xml_query_id: 1,
            rates_asked: Vec::new(),
            rates_answered: HashSet::new(),
            schedule_keys: Vec::new(),
            schedules_asked: Vec::new(),
            schedules_day: None,
            definitions_asked: Vec::new(),
            pending_schedule_pair: Vec::new(),
            pnl_subscriptions: Vec::new(),
            next_schedule_sub_id: 1,
            pending_fanout: Vec::new(),
            contract_rule_scopes: HashSet::new(),
            details_delivered: std::collections::HashMap::new(),
            continuous_lookups: std::collections::HashMap::new(),
            next_fanout_id: 1,
            next_internal_secdef_id: 0xF000_0000,
            next_advisor_request: 1,
            pending_advisor: std::collections::HashMap::new(),
            account_request_key: None,
            account_requests: Vec::new(),
            unread_subtypes: std::collections::HashSet::new(),
            unread_types: std::collections::HashSet::new(),
            pending_named: Vec::new(),
            resolved_named: Vec::new(),
            order_naming: Vec::new(),
            orders_named: Vec::new(),
            auto_fetched_conids: HashMap::new(),
            pending_scanner_enrichment: Vec::new(),
        }
    }

    /// Record `exec_id` in the fill-dedup window. Returns `true` if it is new
    /// (the fill should be processed) and `false` if it was already seen (a
    /// duplicate to skip).
    ///
    /// Backed by a bounded rolling window: once `EXEC_ID_WINDOW` IDs are held,
    /// the oldest is evicted one at a time. This replaces a previous wholesale
    /// `clear()` that dropped the entire history at the cap, which let a
    /// post-reconnect server replay of a recently-seen ExecID double-count the
    /// fill and corrupt the position.
    pub(crate) fn record_exec_id(&mut self, exec_id: &str) -> bool {
        if !self.seen_exec_ids.insert(exec_id.to_string()) {
            return false;
        }
        self.exec_id_order.push_back(exec_id.to_string());
        while self.exec_id_order.len() > EXEC_ID_WINDOW {
            if let Some(old) = self.exec_id_order.pop_front() {
                self.seen_exec_ids.remove(&old);
            }
        }
        true
    }

    /// Learn the venue's own name for an order this session numbers itself.
    ///
    /// Held for as long as the order is working, because that is what the name
    /// is for: every later report about it states only this name, and a
    /// revision, a fill or a withdrawal that cannot be resolved through it
    /// reaches the wrong order or none. One name is learned per order the
    /// venue replays, though, and a caller may ask what the account has
    /// finished as often as it likes — so the oldest name of an order that is
    /// no longer working goes when a new one arrives, and a name is dropped
    /// only when its order is not in the book.
    pub(crate) fn remember_the_venues_name_for(
        &mut self, wire_name: u64, order_id: u64, context: &Context,
    ) {
        if self.wire_name_to_order.insert(wire_name, order_id).is_none() {
            self.wire_names_learned.push_back(wire_name);
        }
        while self.wire_names_learned.len() > WIRE_NAME_WINDOW {
            // The name just learned is never the one forgotten. The order it
            // belongs to is recovered into the book behind this, so at this
            // moment nothing is working under it — which made it the first
            // name the sweep below found to forget, and the order went live
            // with no name at all.
            let forgettable = self.wire_names_learned.iter().position(|name| {
                *name != wire_name
                    && self.wire_name_to_order
                        .get(name)
                        .is_none_or(|order| context.order(*order).is_none())
            });
            // Every name held belongs to an order still working, so there is
            // nothing to forget: the window is what bounds the names of
            // orders that have finished, not what bounds the account.
            let Some(at) = forgettable else { break };
            if let Some(oldest) = self.wire_names_learned.remove(at) {
                self.wire_name_to_order.remove(&oldest);
            }
        }
    }

    /// Which order the venue means by one of its own names, where this session
    /// has been told.
    pub(crate) fn the_order_named(&self, wire_name: u64) -> Option<u64> {
        self.wire_name_to_order.get(&wire_name).copied()
    }

    /// How many of those names are held.
    #[cfg(test)]
    pub(crate) fn how_many_venue_names_are_held(&self) -> usize {
        self.wire_name_to_order.len()
    }

    /// Assemble one finished order, as the reports about it would.
    #[cfg(test)]
    pub(crate) fn hold_a_finished_order_for_test(
        &mut self, order_id: u64, status: crate::types::OrderStatus,
    ) {
        self.orders_in_this_answer.insert((order_id, String::new()));
        self.finished_orders.push(FinishedOrder {
            order_id,
            venue_order: String::new(),
            contract: Default::default(),
            order: crate::types::model::Order { order_id: order_id as i64, ..Default::default() },
            status,
            filled: 0,
            state: Default::default(),
            ord_type: String::new(),
            exec_inst: String::new(),
        });
    }

    /// Whether the window has already seen this execution, asked without
    /// spending its key: the booking that spends it runs further along the
    /// same report, and the readers before it need the same answer.
    pub(crate) fn already_recorded_exec_id(&self, exec_id: &str) -> bool {
        self.seen_exec_ids.contains(exec_id)
    }

    pub(crate) fn process_ccp_message(
        &mut self,
        msg: &[u8],
        ccp_conn: &mut Option<Connection>,
        context: &mut Context,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
        hb: &mut HeartbeatState,
        account_id: &str,
    ) {
        let parsed = fix::fix_parse(msg);
        let msg_type = match parsed.get(&fix::TAG_MSG_TYPE) {
            Some(t) => t.as_str(),
            None => return,
        };
        if *crate::engine::hot_loop::CAPTURE_WIRE {
            let hex: String = msg.iter().map(|b| format!("{b:02x}")).collect();
            shared.market.note_unread_wire("trading-msg", hex);
        }
        if let Some(sent_at) = parsed.get(&fix::TAG_SENDING_TIME) {
            self.ask_schedules_again_on_a_new_day(sent_at, ccp_conn, hb);
        }
        match msg_type {
            fix::MSG_EXEC_REPORT => self.handle_exec_report(&parsed, msg, context, shared, event_tx, account_id),
            fix::MSG_CANCEL_REJECT => self.handle_cancel_reject(&parsed, context, shared, event_tx),
            fix::MSG_NEWS => self.handle_news_bulletin(&parsed, shared),
            fix::MSG_HEARTBEAT => {}
            fix::MSG_TEST_REQUEST => {
                let test_id = parsed.get(&fix::TAG_TEST_REQ_ID).cloned().unwrap_or_default();
                if let Some(conn) = ccp_conn.as_mut() {
                    let ts = chrono_free_timestamp();
                    let _ = conn.send_fix(&[
                        (fix::TAG_MSG_TYPE, fix::MSG_HEARTBEAT),
                        (fix::TAG_SENDING_TIME, &ts),
                        (fix::TAG_TEST_REQ_ID, &test_id),
                    ]);
                    hb.last_ccp_sent = Instant::now();
                }
            }
            "3" => {
                let reason = parsed.get(&58).map(|s| s.as_str()).unwrap_or("unknown");
                let ref_tag = parsed.get(&371).map(|s| s.as_str()).unwrap_or("?");
                let stated = parsed.get(&320).and_then(|s| s.parse::<u32>().ok());
                // Fan-out and schedule requests carry names rather than
                // numbers. An unreadable number still names a request.
                let named_nothing = !parsed.contains_key(&320);
                log::warn!("SessionReject: reason='{reason}' refTag={ref_tag} request={stated:?}");
                // A refusal of an option-chain request. The venue rejects one
                // asked for an underlying it cannot number with "Unknown
                // contract", naming the request; attributed to nothing, the
                // caller waited out the chain's deadline for an answer that
                // had arrived fifteen milliseconds after the request.
                let refused_chain = stated
                    .and_then(|rid| self.pending_option_params.iter().position(|(pid, ..)| *pid == rid))
                    .or_else(|| {
                        (named_nothing
                            && self.pending_option_params.len() == 1
                            && self.pending_secdef.is_empty()
                            && self.pending_matching_symbols.is_empty())
                        .then_some(0)
                    });
                if let Some(at) = refused_chain {
                    let (req_id, symbol, _) = self.pending_option_params.remove(at);
                    log::warn!("Option chain request req_id={req_id} symbol={symbol} rejected: {reason}");
                    shared.reference.push_historical_error(
                        req_id, crate::error_codes::Refusal::NO_DEFINITION,
                        format!("option chain request rejected: {reason}"),
                    );
                    return;
                }
                // A refusal of a symbol search, which states its own number on
                // this tag as the two beside it do. Matched against nothing,
                // the caller waited out the sweep's timeout and was told the
                // venue had never replied — when it had replied at once, and
                // said why.
                if let Some(at) = stated.and_then(|rid| {
                    self.pending_matching_symbols.iter().position(|(pid, _)| *pid == rid)
                }) {
                    let (req_id, _) = self.pending_matching_symbols.remove(at);
                    log::warn!("Matching symbols request req_id={req_id} rejected: {reason}");
                    // Refused rather than answered empty: an empty answer is a
                    // search the venue ran and nothing matched, which is not
                    // what this is.
                    shared.reference.push_historical_error(
                        req_id, crate::error_codes::Refusal::NO_DEFINITION,
                        format!("matching symbols request rejected: {reason}"),
                    );
                    return;
                }
                // The refusal of a search its caller was already told went
                // unanswered: nothing more is owed, and the search it held
                // back goes now.
                if stated.is_some() && stated == self.matching_symbols_abandoned {
                    log::info!("Matching symbols request req_id={stated:?} rejected after it was given up on: {reason}");
                    self.matching_symbols_abandoned = None;
                    return;
                }
                // The venue names the request it is refusing, on tag 320 —
                // the same tag its answers are matched on below. Attributed
                // by count instead, only a lone request was ever told: five
                // asked together drew five rejects inside a tenth of a
                // second, and all five callers waited out the sweep and were
                // told the request had timed out with no reply, when the
                // reply had arrived at once and said why.
                //
                // Falling back on the count where the venue names nothing,
                // which is the case this stood on.
                let named = stated
                    .and_then(|rid| {
                        self.pending_secdef.iter().position(|(pid, _, _)| *pid == rid)
                    })
                    .or_else(|| {
                        // Only where it named nothing, as the chain above reads
                        // it. This tag is not this request's alone — a symbol
                        // search states its own number on it — so a refusal
                        // that named a request of another kind matched no
                        // definition lookup here and fell through to the count,
                        // which answered a lookup that had not been refused at
                        // all: that caller was handed somebody else's refusal
                        // and its own definitions later had nothing waiting.
                        (named_nothing
                            && self.pending_secdef.len() == 1
                            && self.pending_fanout.is_empty()
                            && self.pending_matching_symbols.is_empty()
                            // And the chains, which the branch above defers to
                            // this one for. Left out, the two were not
                            // symmetric: a chain waiting with a lone lookup
                            // deferred there and was taken here, so the lookup
                            // was told the chain's refusal and the chain waited
                            // out its own deadline to be told nothing came.
                            && self.pending_option_params.is_empty())
                        .then_some(0)
                    });
                if let Some(at) = named {
                    let (req_id, _, _) = self.pending_secdef.remove(at);
                    if req_id < 0xF000_0000 {
                        self.refuse_lookup(
                            req_id, 200, format!("contract details request rejected: {reason}"),
                            ccp_conn, shared, event_tx, hb,
                        );
                    } else {
                        // A lookup the engine made for itself: forgotten, so
                        // the next report naming that contract asks again, as
                        // the deadline sweep already arranges.
                        self.auto_fetched_conids.retain(|_, rid| *rid != req_id);
                    }
                }
            }
            "U" => {
                if let Some(comm) = parsed.get(&6040) {
                    match comm.as_str() {
                        "7" | "36" | "154" => self.handle_attached_combo_answer(msg, shared),
                        "75" => {
                            // Position + market price feed (init burst + after each
                            // fill). Not the end of the batch: the account's own
                            // figures follow it, and calling the download
                            // complete here answered a caller before they
                            // arrived. The venue ends the batch itself, below.
                            self.handle_position_feed(msg, ccp_conn, context, shared, event_tx, hb);
                        }
                        "77" => self.handle_account_summary(&parsed, shared),
                        "143" => {
                            // P&L midnight seed — store for client-side daily P&L
                            // computation
                            positions::handle_pnl_response(msg, shared);
                        }
                        "152" => handle_pnl_prices(msg, shared),
                        "186" => {
                            if let Some(matches) = crate::control::contracts::parse_matching_symbols_response(msg) {
                                // A 186 frame is the real answer only when it
                                // carries the match-count tag 146 — present
                                // even when the count is zero. Frames without
                                // it are not-ready acks: popping on one would
                                // deliver a bogus empty answer and orphan the
                                // data frame that follows (observed live; the
                                // same ack-then-data shape as the what-if
                                // path).
                                if extract_tag_value(msg, b"146=").is_none() {
                                    log::debug!("matching-symbols ack frame (no tag 146) — awaiting data frame");
                                } else {
                                // Match the reply to its request by the req_id
                                // the server echoes in tag 320, NOT by queue
                                // order: FIFO cross-attributes out-of-order
                                // replies (, same fix as pending_secdef).
                                let echoed = extract_tag_value(msg, b"320=")
                                    .and_then(|v| v.parse::<u32>().ok());
                                let pos = match echoed {
                                    Some(rid) => self.pending_matching_symbols.iter().position(|(p, _)| *p == rid),
                                    // No echo on the wire: attribution is only
                                    // safe with a single request in flight.
                                    None if self.pending_matching_symbols.len() == 1 => Some(0),
                                    None => None,
                                };
                                if let Some(pos) = pos {
                                    let (req_id, _) = self.pending_matching_symbols.remove(pos);
                                    // An empty result is an answer in its own
                                    // right ("no such symbol") and is
                                    // delivered. Dropped, the caller waits
                                    // indefinitely and the stale queue head
                                    // misattributes every later reply.
                                    shared.reference.push_matching_symbols(req_id, matches);
                                                } else if self.matching_symbols_abandoned.is_some()
                                    && (echoed.is_none() || echoed == self.matching_symbols_abandoned)
                                {
                                    // The reply to a search its caller was told
                                    // went unanswered. Nobody is owed it; the
                                    // search held back behind it goes now.
                                    log::info!(
                                        "matching-symbols reply for req_id={:?}, which was given up on: dropped",
                                        self.matching_symbols_abandoned,
                                    );
                                    self.matching_symbols_abandoned = None;
                                                } else {
                                    log::warn!(
                                        "matching-symbols reply not attributable: echoed={:?} pending={:?}",
                                        echoed, self.pending_matching_symbols,
                                    );
                                }
                                }
                            }
                        }
                        // The venue's error channel. Two subtypes, one
                        // channel: which number it arrives under depends only
                        // on a capability the session negotiated at logon, not
                        // on the error.
                        "60" => handle_trade_charge(&parsed, shared),
                        "192" | "278" => handle_venue_error(&parsed, shared),
                        // The venue speaking to the account holder rather than
                        // about a request: it states the matter as text and
                        // names the account it concerns, and it belongs to no
                        // request anyone made — which is what the channel
                        // above already reports, so it is reported there.
                        // Read by nothing, it reached the account holder
                        // nowhere at all.
                        "42" => handle_venue_error(&parsed, shared),
                        "81" => handle_algorithms(&parsed, shared),
                        // The venue moving a working order: it names the order
                        // and states the terms it has changed — where it is
                        // now working, what its limit now is, or both. Read by
                        // nothing, a caller's own account of the order stayed
                        // at what it was placed with while the venue worked a
                        // different one.
                        "110" => self.handle_order_revision(&parsed, context, shared),
                        "210" => handle_account_config(&parsed, shared),
                        "117" => self.handle_advisor_config(&parsed, shared),
                        "139" => self.handle_option_chain(msg, ccp_conn, hb, shared),
                        "107" => self.handle_schedule_reply(msg, ccp_conn, shared, event_tx, hb),
                        "18" => {
                            // The venue restating its own clock, unasked. It is
                            // never asked for it — this wire carries no such
                            // request — so a caller wanting it is answered from
                            // this machine's clock and the difference the venue
                            // has stated, of which this is the second and last
                            // statement, after the one on the logon.
                            if let Some(seconds) = parsed.get(&6114).and_then(|v| v.parse::<i64>().ok()) {
                                // Saturating, as the reference conversion is:
                                // a second count near the end of what the type
                                // holds carries past it in milliseconds, and
                                // the product wrapped to a clock a thousand
                                // years behind the one the venue stated.
                                shared.market.note_venue_millis(seconds.saturating_mul(1_000));
                            }
                        }
                        // What a contract pays out, answered under the id
                        // the query went out with. An option model needs the
                        // dividend schedule this states.
                        "20" => self.handle_dividends_answer(&parsed, shared),
                        // The operation distinguishes the preset list from
                        // the values of one preset. Values have repeated price
                        // fields and no list count.
                        "194" => self.handle_order_preset_answer(msg, shared),
                        // Something the venue said that nothing here reads.
                        // Dropped in silence it is indistinguishable from the
                        // venue saying nothing, which is how an answer that had
                        // been arriving all along went unnoticed. Named once,
                        // the first time each is seen, so a session that meets
                        // one leaves a record without repeating itself.
                        other => {
                            if self.unread_subtypes.insert(other.to_string()) {
                                match known_unread(other) {
                                    Some(why) => log::debug!("Subtype {other} is not read: {why}"),
                                    None => {
                                        shared.market.note_unread_wire(
                                            "trading", format!("user message {other}"),
                                        );
                                        log::info!(
                                        "Unread user message: subtype {other}. Nothing here reads \
                                         it, so whatever it carries is being discarded"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // The end of a batch the venue was sending. An account request is
            // otherwise only known to be finished when its rows arrive, and an
            // account holding nothing sends no rows — so a caller waiting on
            // the download would wait for something that was already over.
            "EB" => {
                let ends = parsed.get(&6529).map(String::as_str).unwrap_or("");
                if ends.starts_with("AR") {
                    let Some(portfolio) = shared.portfolio_for_request(ends) else { return };
                    log::info!("Account request {ends} is complete");
                    // Only where this end is the outstanding request's. Any
                    // other request's end squares nothing, and declaring the
                    // download over on it lets a caller through to the answer
                    // the squaring exists to prevent.
                    if let Some(unstated) = portfolio.set_account_download_complete(ends) {
                        for con_id in unstated {
                            let avg_cost = portfolio.position_info(con_id)
                                .map(|i| i.avg_cost).unwrap_or_default();
                            if !std::sync::Arc::ptr_eq(&portfolio, &shared.portfolio) { continue; }
                            let Some(instrument) = context.market.instrument_by_con_id(con_id)
                            else { continue };
                            let standing = context.position(instrument);
                            if standing != 0.0 { context.update_position(instrument, -standing); }
                            portfolio.set_position(instrument, 0.0);
                            emit(event_tx, Event::PositionUpdate {
                                instrument, con_id, position: 0.0, avg_cost,
                            });
                        }
                        // Last, so a caller waiting on the download is let
                        // through to an account that has been squared with
                        // what the venue just said rather than to what it held
                        // before.
                        portfolio.account_download_is_settled();
                    }
                }
            }
            "UT" | "UM" => positions::handle_account_update(msg, context, shared),
            // The per-currency figures, which are not a name-and-value stream
            // the way the two above are.
            "RL" => positions::handle_ledger_update(msg, shared),
            // The same figures, for the sets of holdings the account does not
            // hold itself. Applied to the account's own they would overstate
            // what it is worth, so they are kept where the holdings they
            // describe are kept.
            "AL" => handle_account_update_elsewhere(msg, shared, crate::types::HeldElsewhere::Away),
            "UL" => handle_account_update_elsewhere(msg, shared, crate::types::HeldElsewhere::Aside),
            // One frame, every holding it names — see `split_position_entries`.
            "UP" => {
                for one in positions::split_position_entries(msg) {
                    positions::handle_position_update(&one, context, shared, event_tx);
                }
            }
            // The venue keeps three sets of holdings and this client read one.
            // The others carry the same fields in the same tags — they differ
            // only in which set they belong to — and were discarded, so a
            // caller could not learn the account held anything away at all.
            //
            // One frame, every holding it names, the same way the account's own
            // set is read. Handed the flat map instead, a frame naming several
            // holdings arrived as one: the generic parser keeps the last value
            // of a repeated tag, so every holding but the last was gone before
            // anything could see it.
            "AP" | "DO" | "DP" => {
                let held = match msg_type {
                    "AP" => crate::types::HeldElsewhere::Away,
                    "DO" => crate::types::HeldElsewhere::DisplayOnly,
                    _ => crate::types::HeldElsewhere::Aside,
                };
                for one in positions::split_position_entries(msg) {
                    positions::handle_position_elsewhere(&one, shared, held);
                }
            }
            "d" => {
                let response_req_id = crate::control::contracts::secdef_response_req_id(msg);
                // A reply can describe several contracts: a symbol asked for
                // without a currency is answered with every listing that
                // carries it. Read as one contract it keeps whichever came
                // last, and the venue fan-out then follows that one, so the
                // rest are lost before anything can see them. Deliver them all
                // here; the row the path below delivers is deduplicated
                // against these by contract id.
                let listings = {
                    // What a definition carried that nothing here reads. The
                    // point of asking about a contract is to be told about it,
                    // and a field that arrives and is dropped is a fact about
                    // the contract nobody can Recorded rather than guessed
                    // at, so the gap is measurable from a real reply.
                    let unread = crate::control::contracts::unread_definition_tags(msg);
                    if !unread.is_empty() {
                        shared.market.note_unread_wire(
                            "definition",
                            unread.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(","),
                        );
                    }
                    let all = crate::control::contracts::parse_secdef_responses(msg, shared.island_for_nasdaq());
                    for def in &all {
                        shared.reference.cache_contract_definition(def.clone());
                        if def.con_id != 0 && def.sec_type != crate::control::contracts::SecurityType::News && !def.exchange.is_empty() {
                            self.contract_rule_scopes.insert((
                                def.con_id, crate::control::contracts::exchange_to_fix(&def.exchange).to_string(),
                            ));
                        }
                    }
                    let listings = all.len();
                    // Only while the number is still waiting, as a single
                    // listing's row goes out: the caller's own number is the
                    // key, and a reply arriving after its deadline ended the
                    // request is an answer nobody is owed — delivered, it
                    // reached whatever request reused the number since.
                    if listings > 1
                        && let Some(rid) = response_req_id.as_ref().and_then(|r| r.parse::<u32>().ok())
                        && rid < 0xF000_0000
                        && self.pending_secdef.iter().any(|(pid, ..)| *pid == rid)
                    {
                        // The listing the flat parse below produces is that
                        // path's to deliver: it is the one paired with the
                        // trading hours, and claimed here it reached the
                        // caller without them while the enriched row was
                        // dropped at the gate.
                        let flat = crate::control::contracts::parse_secdef_response(msg, shared.island_for_nasdaq())
                            .map(|d| d.con_id);
                        for def in all.into_iter().filter(|d| d.con_id != 0 && Some(d.con_id) != flat) {
                            self.note_what_it_is_written_on(&def, shared, ccp_conn, hb);
                            shared.reference.cache_definition(
                            def.con_id as i64,
                            // Mapped where every other reader of a
                            // definition maps it. Written out here, an
                            // option was cached without its strike, its
                            // right, its expiry or its multiplier — which
                            // is all that tells two options on one
                            // underlying apart.
                            crate::types::model::ContractDetails::from_definition(&def).contract,
                        );
                            self.hand_over(rid, def, shared, event_tx);
                        }
                    }
                    listings
                };

                let fanout_idx = response_req_id.as_ref().and_then(|rid| {
                    self.pending_fanout.iter().position(|p| {
                        p.fanout_req_ids.iter().any(|id| id == rid)
                    })
                });
                if let (Some(idx), Some(rid)) = (fanout_idx, response_req_id.as_ref()) {
                    let api_req_id = self.pending_fanout[idx].api_req_id;
                    let parsed = crate::control::contracts::parse_secdef_response(msg, shared.island_for_nasdaq());
                    if parsed.is_none() {
                        // Counted below all the same: a leg whose reply cannot
                        // be read is answered, and left uncounted the request
                        // waited out its deadline for a reply already in.
                        log::warn!("lookup {api_req_id}: a leg answered with a definition that cannot be read");
                    }
                    if let Some(def) = parsed {
                        // No con_id is "no definition for this
                        // exchange" — cache nothing and emit no row. The leg
                        // still counts toward the fan-out below, so the
                        // request completes.
                        if def.con_id != 0 {
                            shared.reference.cache_definition(
                            def.con_id as i64,
                            // Mapped where every other reader of a
                            // definition maps it. Written out here, an
                            // option was cached without its strike, its
                            // right, its expiry or its multiplier — which
                            // is all that tells two options on one
                            // underlying apart.
                            crate::types::model::ContractDetails::from_definition(&def).contract,
                        );
                            self.note_what_it_is_written_on(&def, shared, ccp_conn, hb);
                            identify_position(shared, &def);
                            self.try_release_scanner_enrichments(def.con_id as i64, shared);
                            // The master row for this same contract may be
                            // parked waiting for its trading hours. It is the
                            // richer of the two and claims the same dedup slot,
                            // so delivering this one first hands the caller a
                            // contract with no trading or liquid hours and
                            // drops the enriched row when it arrives.
                            let awaiting_schedule = self.pending_schedule_pair.iter().any(|p| {
                                p.api_req_id == api_req_id && p.def.con_id == def.con_id
                            });
                            if !awaiting_schedule {
                                self.hand_over(api_req_id, def, shared, event_tx);
                            }
                        }
                    }
                    // A leg the gateway cannot resolve carries no contract:
                    // it still completes the fan-out, but a zeroed row is
                    // not a listing.
                    if !self.pending_fanout[idx].answered.iter().any(|id| id == rid) {
                        self.pending_fanout[idx].answered.push(rid.clone());
                    }
                    self.pending_fanout[idx].deadline = Instant::now() + SECDEF_TIMEOUT;
                    if self.pending_fanout[idx].answered.len() >= self.pending_fanout[idx].fanout_req_ids.len() {
                        self.pending_fanout.swap_remove(idx);
                        // The master row may still be parked awaiting its
                        // schedule. Ending here would order the end before
                        // the row, so the pair carries it — the same way the
                        // single-exchange case above hands the end over.
                        match self.pending_schedule_pair.iter_mut()
                            .find(|p| p.api_req_id == api_req_id)
                        {
                            Some(pair) => pair.is_last = true,
                            None => self.end_lookup(api_req_id, ccp_conn, shared, event_tx, hb),
                        }
                    }
                    let rules = crate::control::contracts::parse_market_rules(msg);
                    if !rules.is_empty() {
                        shared.reference.push_market_rules(rules);
                    }
                    return;
                }

                if let Some(def) = crate::control::contracts::parse_secdef_response(msg, shared.island_for_nasdaq()) {
                    let is_last_wire = crate::control::contracts::secdef_response_is_last(msg);
                    if def.con_id != 0 {
                        self.note_what_it_is_written_on(&def, shared, ccp_conn, hb);
                        shared.reference.cache_definition(
                            def.con_id as i64,
                            // Mapped where every other reader of a
                            // definition maps it. Written out here, an
                            // option was cached without its strike, its
                            // right, its expiry or its multiplier — which
                            // is all that tells two options on one
                            // underlying apart.
                            crate::types::model::ContractDetails::from_definition(&def).contract,
                        );
                        identify_position(shared, &def);
                        self.try_release_scanner_enrichments(def.con_id as i64, shared);
                        // A contract whose sessions waited on this lookup has
                        // them asked for now, by the key the definition states.
                        if !def.join_key.is_empty()
                            && let Some(at) = self.definitions_asked.iter().position(|c| *c == def.con_id)
                        {
                            self.definitions_asked.swap_remove(at);
                            self.ask_schedule(def.con_id, "", shared, ccp_conn, hb);
                        }
                        // A request held until this lookup names its contract.
                        if let Some(rid) = response_req_id.as_ref().and_then(|r| r.parse::<u32>().ok())
                            && let Some(at) = self.pending_named.iter().position(|(pid, ..)| *pid == rid)
                        {
                            let (_, mut cmd, _) = self.pending_named.remove(at);
                            if listings > 1 {
                                Self::abandon_named(&cmd, listings, shared);
                            } else {
                                name_the_contract(&mut cmd, &def);
                                self.resolved_named.push(cmd);
                            }
                        }
                        // And an order, which is named whole: what the venue
                        // names is the contract the order is placed on.
                        if let Some(rid) = response_req_id.as_ref().and_then(|r| r.parse::<u32>().ok())
                            && let Some(at) = self.order_naming.iter().position(|(pid, _)| *pid == rid)
                        {
                            self.order_naming.remove(at);
                            let named = if listings > 1 {
                                OrderNamed::Unnamed(listings)
                            } else {
                                OrderNamed::Contract(Box::new(def.clone()))
                            };
                            self.orders_named.push((rid, named));
                        }
                    }
                    // Match the response to its originating pending_secdef entry
                    // by tag 320 (response_req_id). Without this, an internal
                    // auto-fetch reply (e.g. position-driven secdef for SPY)
                    // landing while a user request is in flight would be
                    // attributed to `pending_secdef.first()` and leak as a
                    // bogus contract_details callback on the user's req_id.
                    let matched_idx: Option<usize> = response_req_id.as_ref()
                        .and_then(|rid| rid.parse::<u32>().ok())
                        .and_then(|rid_u32| {
                            self.pending_secdef.iter().position(|(pid, _, _)| *pid == rid_u32)
                        });
                    let single_shot = matched_idx
                        .map(|i| self.pending_secdef[i].1).unwrap_or(false);
                    let is_by_symbol = matched_idx
                        .map(|i| !self.pending_secdef[i].1).unwrap_or(false);
                    let is_last = is_last_wire || single_shot;
                    // Ask for the rule scopes still missing from the definition
                    // cache. CORPACT does not name a trading venue.
                    let fanout_exchanges: Vec<String> = if is_by_symbol && !is_last_wire {
                        def.valid_exchanges.iter()
                            .filter(|e| !matches!(e.as_str(), "" | "CORPACT"))
                            .filter(|e| !self.contract_rule_scopes.contains(&(
                                def.con_id, crate::control::contracts::exchange_to_fix(e).to_string(),
                            )))
                            .cloned()
                            .collect()
                    } else {
                        Vec::new()
                    };
                    if let Some(idx) = matched_idx {
                        let req_id = self.pending_secdef[idx].0;
                        // Internal sentinel req_ids (auto-fetch for cold-cache
                        // positions, scanner enrichment) start at 0xF000_0000.
                        // Their replies must populate the contract cache but
                        // never surface as user-visible contract_details
                        // callbacks.
                        let is_internal = req_id >= 0xF000_0000;
                        let join_key = def.join_key.clone();
                        if is_last {
                            self.pending_secdef.remove(idx);
                        } else {
                            // The venue is mid-answer, so the bound is on how
                            // long it stays quiet, not on how long the answer
                            // runs: a class naming every expiry sends for
                            // longer than one contract does, and cut at a
                            // fixed total it reads as a class the venue does
                            // not carry. Same rule the fan-out leg keeps.
                            self.pending_secdef[idx].2 = Instant::now() + unanswered_after(req_id);
                        }
                        let con_id = def.con_id as i64;
                        if con_id == 0 {
                            // Con_id=0 is the gateway saying "no
                            // security definition", not a contract. Pushed as
                            // a row it is indistinguishable from a hit —
                            // empty symbol, and min_tick carrying its 0.01
                            // default. Report it the way the reject and
                            // timeout paths do. The by-symbol leg gets its
                            // end from the fan-out branch below.
                            // The same reply also answers a symbol the gateway
                            // cannot resolve, which arrives contract-less (live:
                            // "BRK.A" for the "BRK A" listing). Drop the pending
                            // entry so the single-shot leg is not left parked
                            // waiting for a definition that will not come.
                            self.pending_secdef.retain(|(rid, ss, _)| *rid != req_id || *ss);
                            if !is_internal {
                                // Ended unconditionally: the pending entry was
                                // dropped just above, so the fan-out branch
                                // below can no longer supply the end for the
                                // by-symbol leg and a caller blocked on it
                                // would wait forever.
                                self.refuse_lookup(
                                    req_id, crate::error_codes::Refusal::NO_DEFINITION, NO_DEFINITION_FOUND.to_string(),
                                    ccp_conn, shared, event_tx, hb,
                                );
                            } else {
                                self.abandon_holders_of(req_id, shared);
                            }
                        } else if join_key.is_empty() {
                            // No join key — emit immediately without schedule data.
                            if !is_internal {
                                self.hand_over(req_id, def, shared, event_tx);
                                // The end is the lookup's, not the row's: a
                                // number reused for a contract it was already
                                // handed got neither, and waited out its
                                // deadline.
                                if is_last {
                                    self.end_lookup(req_id, ccp_conn, shared, event_tx, hb);
                                }
                            }
                        } else if is_internal {
                            // Skip schedule pairing for internal sentinels — no
                            // user is awaiting the trading_hours enrichment.
                        } else {
                            self.pending_schedule_pair.push(PendingSchedulePair {
                                api_req_id: req_id,
                                join_key: join_key.clone(),
                                def,
                                is_last,
                                deadline: Instant::now() + std::time::Duration::from_secs(3),
                            });
                            self.send_schedule_subscribe(&join_key, ccp_conn, hb);
                        }
                        // Dispatch fan-out (or fire end immediately if the
                        // symbol resolves to a single exchange and there's
                        // nothing to fan out to).
                        if is_by_symbol && !is_last_wire && con_id != 0 {
                            self.pending_secdef.retain(|(rid, ss, _)| *rid != req_id || *ss);
                            if is_internal {
                                // A naming lookup of the engine's own needed
                                // the one definition that names the contract,
                                // released above, and nothing after it. Asked
                                // per venue as a caller's lookup is, its rows
                                // and its end reached the wrapper under a
                                // number nobody asked with.
                            } else if fanout_exchanges.is_empty() {
                                // The master row may be parked awaiting its
                                // schedule pair; firing end now would order
                                // end BEFORE the row. Defer it to
                                // the pair's resolution (or its 3s sweep).
                                if let Some(pair) = self.pending_schedule_pair.iter_mut()
                                    .find(|p| p.api_req_id == req_id)
                                {
                                    pair.is_last = true;
                                } else {
                                    self.end_lookup(req_id, ccp_conn, shared, event_tx, hb);
                                }
                            } else {
                                let mut fanout_req_ids = Vec::with_capacity(fanout_exchanges.len());
                                for exch in &fanout_exchanges {
                                    let fid = format!("ibxfan-{}-{}", req_id, self.next_fanout_id);
                                    self.next_fanout_id = self.next_fanout_id.wrapping_add(1);
                                    let fix_exch = if exch == "SMART" { "BEST" } else { exch.as_str() };
                                    self.send_fanout_secdef_request(&fid, con_id, fix_exch, ccp_conn, hb);
                                    fanout_req_ids.push(fid);
                                }
                                log::info!(
                                    "Secdef by-symbol fan-out: api_req_id={} con_id={} exchanges={}",
                                    req_id, con_id, fanout_req_ids.len(),
                                );
                                self.pending_fanout.push(PendingFanout {
                                    api_req_id: req_id,
                                    fanout_req_ids,
                                    answered: Vec::new(),
                                    deadline: Instant::now() + SECDEF_TIMEOUT,
                                });
                            }
                        }
                    }
                }
                let rules = crate::control::contracts::parse_market_rules(msg);
                if !rules.is_empty() {
                    shared.reference.push_market_rules(rules);
                }
            }
            // A message type nothing here reads. Named once, like an unread
            // user message: the out-of-band types carry position and account
            // data, and an unrecognised one is worth a line rather than
            // nothing.
            other => {
                if self.unread_types.insert(other.to_string()) {
                    shared.market.note_unread_wire("trading", format!("type {other}"));
                    log::info!(
                        "Unread message: type {other}, {} bytes. Nothing here reads it, so \
                         whatever it carries is being discarded",
                        msg.len(),
                    );
                }
            }
        }
        // Every contract the messages above registered is counted where the
        // API reads the count. A recovery record registers the contract it
        // names, and only a subscription refreshed this mirror — so an account
        // whose working orders were on contracts this session never subscribed
        // to counted none of them, and a global cancel, composed by instrument
        // from the count, withdrew nothing and returned without an error.
        shared.market.set_instrument_count(context.market.count());
    }

    fn handle_news_bulletin(&mut self, parsed: &std::collections::HashMap<u32, String>, shared: &SharedState) {
        // The urgency the venue states, and the kind a caller is told about.
        // They are not the same numbering and they are not in the same order:
        // the venue's second kind is an exchange that has stopped trading and
        // its third is one that has started, while a caller reads those the
        // other way round. Passed straight through, a caller halting on an
        // exchange going down acted on one coming up. The last three are
        // kinds of their own — plain text, a message meant to be shown, and
        // one written as markup — and were all reported as ordinary news.
        static BULLETIN_TYPE_MAP: &[(i32, i32)] = &[
            (1, 1), (2, 3), (3, 2), (8, 4), (9, 5), (10, 6),
        ];
        /// The kind that claims least about what a bulletin is: text, meant to
        /// be read. What an urgency this does not name goes out under.
        const BULLETIN_PLAIN_TEXT: i32 = 4;
        let fix_type: i32 = parsed.get(&fix::TAG_URGENCY)
            .and_then(|s| s.parse().ok()).unwrap_or(0);
        let api_type = BULLETIN_TYPE_MAP.iter()
            .find(|(k, _)| *k == fix_type)
            .map(|(_, v)| *v);
        let api_type = match api_type {
            Some(t) => t,
            None => {
                // Delivered as plain text, and the urgency recorded. A bulletin
                // whose urgency this does not name is still a bulletin the
                // venue sent, and dropping it here threw away the headline with
                // the number nobody could read — which is the only part a
                // person reads. The kind it goes out under is the one that
                // claims least: the venue said something, and this does not
                // know what sort of something.
                shared.market.note_unread_wire(
                    "trading",
                    format!("news bulletin urgency {fix_type}"),
                );
                log::warn!(
                    "news bulletin states urgency {fix_type}, which names no bulletin type \
                     here — delivered as plain text",
                );
                BULLETIN_PLAIN_TEXT
            }
        };
        let message = parsed.get(&fix::TAG_HEADLINE).cloned().unwrap_or_default();
        let exchange = parsed.get(&fix::TAG_SECURITY_EXCHANGE).cloned().unwrap_or_default();
        // The venue numbers its own bulletins and states the number here.
        // Counted locally instead, the numbering started again at every
        // connect and named nothing the venue would recognise, so the same
        // bulletin arriving twice across a reconnect could not be told from
        // two. Absent, it stands at the widest number a bulletin id is
        // carried under, which is what says nothing was stated.
        let msg_id = parsed.get(&fix::TAG_BULLETIN_ID)
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(i32::MAX);
        let bulletin = NewsBulletin {
            msg_id,
            msg_type: api_type,
            message,
            exchange,
        };
        shared.market.push_news_bulletin(bulletin);
    }

    /// The account's cash, by currency.
    ///
    /// Tag 6566 counts the entries that follow and each is a currency on tag 15
    /// with its balance on 9806. Read as a single number under a selector, this
    /// took the last balance on the frame — the one belonging to whichever
    /// currency came last — and published it as net liquidation, so an account
    /// holding the better part of a million in cash reported a net liquidation
    /// of minus fourteen hundred.
    ///
    /// The account states these balances under names of their own, so there is
    /// nothing here a caller does not already have. That is what this session
    /// was sent and read back against the account; it is not a claim that no
    /// account, in no state, ever states more on this frame.
    fn handle_account_summary(&mut self, parsed: &std::collections::HashMap<u32, String>, _shared: &SharedState) {
        // Tag 6566 counts the entries that follow, and each is a currency on
        // tag 15 with its cash balance on 9806. Captured whole from a live
        // session:
        //
        //     6040=77 1=DU… 6566=3 15=BASE 9806=1492751.1917
        //                          15=GBP  9806=1105714.0700
        //                          15=USD  9806=0.0000
        //
        // and the sterling figure is this account's `TotalCashBalance` in
        // sterling to the penny. So the frame states what the account already
        // states under names, and there is nothing here a caller does not have.
        //
        // Read as a kind rather than a count, it had this client publishing
        // "an account figure of kind 3" whose value was the last of the three —
        // a number belonging to whichever currency happened to be last on the
        // wire, under a label that named nothing.
        let _ = parsed;
        // Nothing is written from here, so nothing is published from here
        // either. Writing the unchanged snapshot back had one effect: it
        // raised the flag that says this connection has stated the account.
        // A frame this function's own doc records as unread then marked a
        // pre-drop snapshot current on a rebuilt connection.
    }

    /// Subscribe to the schedule paired with a secdef reply, joined on tag 6256.
    /// Internal subscription (no API-client req_id exposed); reply arrives as
    /// 35=U|6040=107 and is matched back to the secdef via 6256.
    fn send_schedule_subscribe(
        &mut self,
        join_key: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        if let Some(conn) = ccp_conn.as_mut() {
            let sub_id = self.next_schedule_sub_id;
            self.next_schedule_sub_id += 1;
            let sub_id_str = format!("SchedSub.{sub_id}");
            let ts = chrono_free_timestamp();
            let _ = conn.send_fix(&[
                (fix::TAG_MSG_TYPE, "U"),
                (fix::TAG_SENDING_TIME, &ts),
                (crate::control::contracts::TAG_SUB_PROTOCOL,
                    crate::control::contracts::SUB_PROTOCOL_SCHEDULE_SUBSCRIBE),
                (320, &sub_id_str),
                (crate::control::contracts::TAG_SCHEDULE_JOIN_KEY, join_key),
            ]);
            hb.last_ccp_sent = Instant::now();
        }
    }

    /// Drop pending schedule pairs past their deadline, emitting partial details.
    /// Fail contract-details requests whose deadline has passed:
    /// both plain/by-symbol secdef lookups the gateway never answered and
    /// by-symbol fan-outs missing one or more per-exchange replies. On
    /// expiry the caller gets error 200 plus contract_details_end, so a
    /// blocked wait unblocks with no API change. Internal sentinel req_ids
    /// (>= 0xF000_0000: cache auto-fetch, scanner enrichment) are dropped
    /// silently — no user is waiting on them.
    /// How long a subscription waits for the lookup that would name its
    /// contract.
    ///
    /// Between two deadlines, and it has to stay between them. Longer than the
    /// lookup's own, so a definition or a refusal from the venue is preferred
    /// to this. Shorter than the caller's, because for a held request this is
    /// the only report there is: the lookup behind it is asked under an
    /// internal id, and an internal id is dropped silently. Sitting past the
    /// caller's wait, as it did, meant the caller was told nothing arrived
    /// while the reason was still being held, and heard it never.
    pub(crate) const NAMING_TIMEOUT: Duration =
        Duration::from_secs(crate::config::ANSWER_TIMEOUT_SECS - 3);

    /// A held request that cannot be sent for the contract described, told to
    /// its caller under its own number. Nothing the venue holds matches the
    /// contract described, which is what the code says word for word.
    fn abandon_named(cmd: &crate::types::ControlCommand, listings: usize, shared: &SharedState) {
        // Given by id alone, the id is all it states to name it by.
        let reason = if let Some((con_id, _)) = named_by_id_alone(cmd) {
            format!("no security definition has been found for contract {con_id}, so the request could not be sent")
        } else {
            let Some(named) = contract_named(cmd) else { return };
            unnamed(
                &named.sec_type, &named.symbol, &named.exchange, listings,
                "the request could not be sent",
            )
        };
        log::warn!("Request abandoned: {reason}");
        // A book's number is free again, as a gateway frees it.
        if let crate::types::ControlCommand::SubscribeDepth { req_id, .. } = cmd {
            shared.market.note_book_let_go(*req_id);
        }
        if let Some(req_id) = request_id(cmd) {
            super::push_hmds_refusal(
                shared, req_id, crate::error_codes::Refusal::NO_DEFINITION, reason,
                matches!(cmd, crate::types::ControlCommand::FetchHistorical { .. }),
            );
        }
    }

    /// End what waited on a naming lookup of the engine's own that the venue
    /// answered with no definition. Dropped with the lookup alone, the
    /// subscription or request learnt of it only when its own wait ran out.
    fn abandon_holders_of(&mut self, req_id: u32, shared: &SharedState) {
        if let Some(at) = self.pending_named.iter().position(|(pid, ..)| *pid == req_id) {
            let (_, cmd, _) = self.pending_named.remove(at);
            Self::abandon_named(&cmd, 0, shared);
        }
        if let Some(at) = self.order_naming.iter().position(|(pid, _)| *pid == req_id) {
            self.order_naming.remove(at);
            self.orders_named.push((req_id, OrderNamed::Unnamed(0)));
        }
    }

    pub(crate) fn sweep_contract_details(
        &mut self,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
    ) {
        // A schedule pairing delivers after the lookup that asked for it has
        // been retired, so the record of what a request was already handed has
        // to outlive both. Dropping it at the lookup's end let the venue copy
        // through behind the one carrying the trading hours.
        let idle = self.pending_secdef.is_empty()
            && self.pending_fanout.is_empty()
            && self.pending_schedule_pair.is_empty();
        if idle {
            self.details_delivered.clear();
        }
        if self.pending_secdef.is_empty() && self.pending_fanout.is_empty() {
            return;
        }
        let now = Instant::now();
        // A session that has ended answers nothing, so every request waiting on
        // it is already finished — waiting out its deadline only delays the
        // caller learning that, once per request.
        let over = shared.reference.session_over();
        let mut expired: Vec<u32> = Vec::new();
        let mut lost_auto_fetch: Vec<u32> = Vec::new();
        self.pending_secdef.retain(|(req_id, _, deadline)| {
            if now >= *deadline || over.is_some() {
                if *req_id < 0xF000_0000 {
                    expired.push(*req_id);
                } else {
                    log::warn!("Internal secdef timeout: req_id={req_id:#x}");
                    // Forgotten, so the next report naming that contract asks
                    // again. Held, one lost request leaves the contract unnamed
                    // for the life of the session.
                    lost_auto_fetch.push(*req_id);
                }
                false
            } else {
                true
            }
        });
        if !lost_auto_fetch.is_empty() {
            self.auto_fetched_conids.retain(|_, rid| !lost_auto_fetch.contains(rid));
        }
        self.pending_fanout.retain(|p| {
            if now >= p.deadline || over.is_some() {
                log::warn!(
                    "Contract-details fan-out timeout: api_req_id={} received {} of {}",
                    p.api_req_id, p.answered.len(), p.fanout_req_ids.len(),
                );
                expired.push(p.api_req_id);
                false
            } else {
                true
            }
        });
        for req_id in expired {
            let (code, why) = match over {
                Some(reason) => (
                    crate::error_codes::Refusal::NOT_CONNECTED,
                    format!("the session is over: {reason}"),
                ),
                None => (
                    crate::error_codes::Refusal::NO_ANSWER,
                    "contract details request timed out — no reply from the gateway".to_string(),
                ),
            };
            log::warn!("Contract-details unanswered: req_id={req_id} ({why})");
            self.fail_lookup(req_id, code, why, shared, event_tx);
        }
    }

    pub(crate) fn sweep_pending_schedule_pairs(
        &mut self,
        ccp_conn: &mut Option<Connection>,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
        hb: &mut HeartbeatState,
    ) {
        let now = Instant::now();
        let mut emit_now: Vec<PendingSchedulePair> = Vec::new();
        self.pending_schedule_pair.retain(|p| {
            if now >= p.deadline {
                let mut def = p.def.clone();
                def.trading_hours = None;
                def.liquid_hours = None;
                def.time_zone_id = None;
                emit_now.push(PendingSchedulePair {
                    api_req_id: p.api_req_id,
                    join_key: p.join_key.clone(),
                    def,
                    is_last: p.is_last,
                    deadline: p.deadline,
                });
                log::warn!("Schedule pair timeout: api_req_id={} join_key={}",
                    p.api_req_id, p.join_key);
                false
            } else {
                true
            }
        });
        for p in emit_now {
            // Same gate as every other way a contract reaches the caller: the
            // venue fan-out describes one contract many times over.
            self.hand_over(p.api_req_id, p.def, shared, event_tx);
            if p.is_last {
                self.end_lookup(p.api_req_id, ccp_conn, shared, event_tx, hb);
            }
        }
    }

    /// Match a 6040=107 schedule reply to a pending secdef pair and emit merged
    /// details.
    fn handle_schedule_reply(
        &mut self,
        msg: &[u8],
        ccp_conn: &mut Option<Connection>,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
        hb: &mut HeartbeatState,
    ) {
        // Extract 6256 from the reply to locate the matching pair.
        let join_key = match extract_tag_value(msg, b"6256=") {
            Some(v) => v,
            None => return,
        };
        // Kept for the key, whoever asked: every contract on it has these
        // sessions.
        let schedule = crate::control::contracts::parse_schedule_response(msg);
        if let Some(schedule) = &schedule {
            self.schedules_asked.retain(|key| *key != join_key);
            shared.reference.set_contract_schedule(&join_key, schedule.clone());
        }
        let pos = match self.pending_schedule_pair.iter().position(|p| p.join_key == join_key) {
            Some(p) => p,
            None => return,
        };
        let mut pair = self.pending_schedule_pair.swap_remove(pos);
        if let Some(sched) = schedule {
            if pair.def.con_id != 0 {
                shared.reference.note_schedule_key(pair.def.con_id, &join_key);
            }
            pair.def.time_zone_id = if sched.timezone.is_empty() {
                None
            } else {
                Some(sched.timezone.clone())
            };
            // On the clock the venue names beside them. Where that zone is
            // one no database here answers to, the hours stay as the wire
            // carried them and the zone is reported as the UTC they are on,
            // so a caller is never given a name the times do not match.
            let named = sched.timezone.as_str();
            let stated_on_the_named_clock =
                crate::control::contracts::sessions_are_stated_on(named);
            if !stated_on_the_named_clock {
                pair.def.time_zone_id = Some("UTC".to_string());
            }
            pair.def.trading_hours = Some(
                crate::control::contracts::format_sessions_string(&sched.trading_hours, named)
            );
            pair.def.liquid_hours = Some(
                crate::control::contracts::format_sessions_string(&sched.liquid_hours, named)
            );
        }
        // The schedule reply completes the pairing, and this is where the row
        // that carries the trading hours reaches the caller. Same gate as every
        // other path: one contract, delivered once.
        self.hand_over(pair.api_req_id, pair.def, shared, event_tx);
        if pair.is_last {
            self.end_lookup(pair.api_req_id, ccp_conn, shared, event_tx, hb);
        }
    }

    /// What the venue answers an advisor request with.
    ///
    /// The reply states the number the request went out under and nothing else
    /// about what was asked, so what the caller is owed is read from what was
    /// remembered when the question left. A question is answered with the
    /// configuration itself; a replacement with its end, or, where the venue
    /// states trouble, with that trouble under the code the reference client
    /// reports a failed replacement on.
    ///
    /// A reply carrying no number at all is the venue volunteering a change to
    /// a model nobody asked about. Nothing here asked for it and no caller is
    /// waiting on it, so it is logged rather than delivered to whichever
    /// request happens to be open.
    fn handle_advisor_config(
        &mut self,
        parsed: &std::collections::HashMap<u32, String>,
        shared: &SharedState,
    ) {
        let Some(key) = parsed.get(&6158).filter(|k| !k.is_empty()) else {
            log::info!("The venue stated an advisor configuration nobody asked for");
            return;
        };
        let Some(asked) = self.pending_advisor.remove(key.as_str()) else {
            log::warn!("The venue answered advisor request {key}, which nothing here asked");
            return;
        };
        // Tag 58 is the venue's own account of what went wrong. Empty is the
        // only shape that means the request stands.
        let trouble = parsed.get(&58).map(String::as_str).unwrap_or("");
        if !trouble.is_empty() {
            log::warn!("The venue refused advisor request {key}: {trouble}");
            shared.reference.push_advisor_refused(
                asked.origin(), ADVISOR_SAVE_REFUSED, trouble.to_string(),
            );
            return;
        }
        if asked.replacing {
            shared.reference.push_advisor_replaced(asked.req_id, ADVISOR_SAVED.to_string());
            return;
        }
        // A partition the venue holds nothing for is stated as nothing rather
        // than left out, and an advisor with no groups is an answer: delivered
        // empty, the caller learns there are none; dropped, they wait.
        let document = parsed.get(&6118).cloned().unwrap_or_default();
        shared.reference.push_advisor_config(asked.fa_data_type, document);
    }

    /// Ask for, or replace, the advisor's own configuration.
    ///
    /// An advisor's groups, allocation profiles and models are held by the
    /// venue, not by this client, and are asked for one partition at a time.
    /// The command says which of asking, replacing or removing is meant; a
    /// replacement carries the configuration as its own document.
    ///
    /// An account that is not an advisor's holds none of this, and the venue
    /// says so rather than answering with an empty one.
    pub(crate) fn send_advisor_config(
        &mut self,
        req_id: i64,
        command: i32,
        partition: &str,
        fa_data_type: i32,
        document: Option<&str>,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) {
        // Refused where it cannot be sent, as every sibling request on this
        // connection is. Nothing is recorded for a request that never went
        // out, and the connection that replaces this one is asked nothing
        // this one was — so there is no later moment at which an answer
        // arrives. Silent, a caller reading a partition waited for ever, and
        // one replacing a partition held a document it believed the venue had
        // taken.
        let Some(conn) = ccp_conn.as_mut() else {
            log::warn!("Advisor configuration request req_id={req_id} not sent: no CCP transport");
            shared.reference.push_advisor_refused(
                PendingAdvisor::origin_of(req_id, document.is_some()), ADVISOR_SAVE_REFUSED,
                "the advisor configuration request could not be sent: no connection to the \
                 venue".to_string(),
            );
            return;
        };
        let ts = chrono_free_timestamp();
        let command = command.to_string();
        // Which partition, and which request. The partition rides 6906 and
        // 6158 is the request's own number, counted from one for the
        // session and sent as a string, which the reply carries back so an
        // answer can be matched to its question. Writing the partition
        // into 6158 and omitting 6906 leaves every advisor request naming
        // no partition, so a replacement carries a document for a partition
        // none of them named. The two are written in this order.
        let key = self.next_advisor_request.to_string();
        self.next_advisor_request = self.next_advisor_request.wrapping_add(1);
        let mut fields: Vec<(u32, &str)> = vec![
            (fix::TAG_MSG_TYPE, "U"),
            (fix::TAG_SENDING_TIME, &ts),
            (6040, "116"),
            (6905, &command),
            (6158, &key),
            (6906, partition),
        ];
        // Only a replacement carries a document; asking for one that states
        // a document would be asking and telling at once.
        if let Some(xml) = document {
            fields.push((6118, xml));
        }
        let _ = conn.send_fix(&fields);
        // Held before the frame is called sent: the reply states only this
        // number, so a question nobody remembers asking is a reply nobody
        // can be given.
        self.pending_advisor.insert(
            key.clone(),
            PendingAdvisor {
                req_id,
                fa_data_type,
                replacing: document.is_some(),
                deadline: Instant::now() + ADVISOR_TIMEOUT,
            },
        );
        hb.last_ccp_sent = Instant::now();
        log::info!("Sent advisor configuration request: command={command} partition={partition}");
    }

    /// Ask the venue to state the account's figures now.
    ///
    /// The same pair the connection sends when it re-establishes itself: the
    /// keyed account request on 6040=6, and the display request on 6040=91 that
    /// carries the positions beside it. Subscribing alone does not produce
    /// them — the venue restates them on its own schedule, and a session that
    /// has just opened waits tens of seconds for its first set.
    pub(crate) fn send_account_refresh(
        &mut self,
        account: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) {
        let Some(conn) = ccp_conn.as_mut() else { return };
        let ts = chrono_free_timestamp();
        let _ = conn.send_fix(&[
            (fix::TAG_MSG_TYPE, "U"),
            (fix::TAG_SENDING_TIME, &ts),
            (6040, "91"),
            (1, account),
            (6556, "DR.1"),
            (6712, "1"),
        ]);
        // The key is unique for the life of the process, not of this state.
        // The venue keys the subscription on 6529 and answers a key it is
        // already serving with nothing; a connection outlives the loops that
        // use it, so a counter reset with each loop asks under a key the
        // connection has already seen and is not answered at all. The opening
        // sequence has used AR.1.
        let key = self.next_account_request_key();
        self.account_requests.push((key.clone(), account.to_string()));
        shared.name_account_request(&key, account);
        shared.portfolio_for(account).holdings_restated_under(&key);
        let _ = conn.send_fix(&[
            (fix::TAG_MSG_TYPE, "U"),
            (fix::TAG_SENDING_TIME, &ts),
            (6040, "6"),
            (6036, "1"),
            (6095, account),
            (6529, &key),
        ]);
        hb.last_ccp_sent = Instant::now();
    }

    /// The next key to ask for account and position data under, recorded as
    /// the one this state now holds.
    ///
    /// Unique for the life of the process, not of this state. The venue keys
    /// the subscription on 6529 and answers a key it is already serving with
    /// nothing, and a connection outlives the loops that use it — so every
    /// place that asks draws from here rather than naming a key of its own.
    /// A reconnect that named one directly asked under a key a refresh had
    /// already spent, was answered with nothing, and the position pushes did
    /// not resume.
    fn next_account_request_key(&mut self) -> String {
        static NEXT_ACCOUNT_REQUEST: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(2);
        let n = NEXT_ACCOUNT_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = format!("AR.{n}");
        self.account_request_key = Some(key.clone());
        key
    }

    /// life of the connection and the venue stops answering new ones.
    /// Send P&L subscribe: 6040=142, 6529=PLR.{N}, 1={account}.
    /// Close the account subscription this state opened.
    ///
    /// Tag 6036 states whether the request opens the subscription or closes it,
    /// and the key on 6529 names which. Left open, each loop holds one for the
    /// life of the connection and the venue stops answering new ones.
    pub(crate) fn send_account_unsubscribe(
        &mut self,
        account: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        let last = self.account_request_key.take();
        let mut requests = std::mem::take(&mut self.account_requests);
        if let Some(key) = last && !requests.iter().any(|(k, _)| k == &key) {
            requests.push((key, account.to_string()));
        }
        let Some(conn) = ccp_conn.as_mut() else { return };
        let ts = chrono_free_timestamp();
        for (key, account) in requests {
            let _ = conn.send_fix(&[
                (fix::TAG_MSG_TYPE, "U"),
                (fix::TAG_SENDING_TIME, &ts),
                (6040, "6"), (6036, "0"), (6095, &account), (6529, &key),
            ]);
        }
        hb.last_ccp_sent = Instant::now();
    }

    pub(crate) fn send_pnl_subscribe(
        &mut self,
        req_id: i64,
        single: bool,
        account: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) {
        // Recorded whether or not the transport is up: a rebuilt connection
        // asks for every standing subscription again.
        self.pnl_subscriptions.retain(|(id, one, _)| (*id, *one) != (req_id, single));
        self.pnl_subscriptions.push((req_id, single, account.to_string()));
        if let Some(conn) = ccp_conn.as_mut() {
            // The key names the request; the account rides tag 1 beside it, as
            // the protocol defines it and as the opening sequence in
            // `logon::send_post_burst_grace` already wrote it. Written into the
            // key instead, the account rides as literal bytes inside a field
            // that names a request, and tag 1 is not sent at all.
            let pnl_key = Self::next_pnl_key();
            shared.name_account_request(&pnl_key, account);
            let ts = chrono_free_timestamp();
            let _ = conn.send_fix(&[
                (fix::TAG_MSG_TYPE, "U"),
                (fix::TAG_SENDING_TIME, &ts),
                (6040, "142"),
                (6529, &pnl_key),
                (1, account),
            ]);
            hb.last_ccp_sent = Instant::now();
            log::info!("Sent P&L subscribe: req_id={req_id} account={account}");
        }
    }

    fn next_pnl_key() -> String {
        static NEXT_PNL_REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(2);
        format!("PLR.{}", NEXT_PNL_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    /// Forget a profit-and-loss subscription, so a reconnect does not renew it.
    ///
    /// Nothing withdraws one at the venue on this wire; what stops is the
    /// renewal, and the reporting once the session ends.
    pub(crate) fn withdraw_pnl_subscription(&mut self, req_id: i64, single: bool) {
        self.pnl_subscriptions.retain(|(id, one, _)| (*id, *one) != (req_id, single));
    }

    /// Say goodbye before going.
    ///
    /// A session dropped without this is one the venue has to time out, and
    /// this account permits only one at a time: the next connection then races
    /// a session the venue still believes is live. A gateway sends one too,
    /// stating why it is going.
    pub(crate) fn send_logout(
        &mut self,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) -> bool {
        let Some(conn) = ccp_conn.as_mut() else { return false };
        if say_goodbye(conn).is_ok() {
            hb.last_ccp_sent = Instant::now();
            true
        } else {
            false
        }
    }

    /// Ask the venue for the contract an id names.
    ///
    /// `event_tx` is where a caller's lookup that cannot be sent is told it
    /// ended; a lookup of the engine's own passes none.
    pub(crate) fn send_secdef_request(&mut self, req_id: u32, con_id: i64, exchange: &str, ccp_conn: &mut Option<Connection>, hb: &mut HeartbeatState, shared: &SharedState, event_tx: &Option<EventSink>) {
        // A lookup sent afresh under a number forgets what that number was
        // handed before, as the lookup by symbol does.
        self.details_delivered.remove(&req_id);
        self.continuous_lookups.remove(&req_id);
        let sent = match ccp_conn.as_mut() {
            Some(conn) => {
                let con_id_str = con_id.to_string();
                let req_id_str = req_id.to_string();
                let ts = chrono_free_timestamp();
                conn.send_fix(&[
                    (fix::TAG_MSG_TYPE, "c"),
                    (fix::TAG_SENDING_TIME, &ts),
                    (crate::control::contracts::TAG_SECURITY_REQ_ID, &req_id_str),
                    (crate::control::contracts::TAG_SECURITY_REQ_TYPE, "2"),
                    (crate::control::contracts::TAG_IB_SOURCE, "Socket"),
                    (146, "1"),
                    (crate::control::contracts::TAG_IB_CON_ID, &con_id_str),
                    (6004, if exchange.is_empty() { "ANYEXCH" } else { crate::control::contracts::exchange_to_fix(exchange) }),
                ])
                .map_err(|e| e.to_string())
            }
            None => Err("no connection to the venue".to_string()),
        };
        match sent {
            Ok(()) => {
                log::info!("Sent secdef request: req_id={req_id} con_id={con_id}");
                hb.last_ccp_sent = Instant::now();
            }
            Err(why) => {
                if self.refuse_unsent_lookup(req_id, &why, shared, event_tx) {
                    return;
                }
            }
        }
        // Known-conId lookup: single record, no paginated terminator.
        self.pending_secdef.push((req_id, true, Instant::now() + unanswered_after(req_id)));
    }

    /// A caller's lookup that did not reach the venue, refused now rather
    /// than queued. Queued as if sent, it was reported twenty seconds later as
    /// a request the venue did not answer, when the venue never received it,
    /// while a matching-symbols or option-chain request in the same state is
    /// refused at once. A lookup of the engine's own is queued all the same
    /// and answers `false`: what waits on it is told by its own sweep.
    ///
    /// Ended through the same helper as every other lookup, so a caller
    /// listening for events hears the end too — a continuous future's second
    /// lookup among them.
    fn refuse_unsent_lookup(&mut self, req_id: u32, why: &str, shared: &SharedState, event_tx: &Option<EventSink>) -> bool {
        if req_id >= crate::bridge::ENGINE_ID_BASE {
            log::warn!("secdef request req_id={req_id:#x} queued unsent: {why}");
            return false;
        }
        log::warn!("Contract details request req_id={req_id} not sent: {why}");
        self.fail_lookup(
            req_id, crate::error_codes::Refusal::NOT_CONNECTED,
            format!("contract details request could not be sent: {why}"), shared, event_tx,
        );
        true
    }

    /// Hand a caller one contract its lookup found.
    ///
    /// Every row reaches a caller through here, and once: a lookup on a
    /// smart-routed symbol describes one contract many times over. A continuous
    /// future's own rows are held until its lookup ends instead.
    fn hand_over(
        &mut self,
        req_id: u32,
        def: crate::control::contracts::ContractDefinition,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
    ) {
        if let Some(asked) = self.continuous_lookups.get_mut(&req_id)
            && !asked.asking_listed
        {
            // One per venue and multiplier, the first stated kept, as a
            // gateway keeps them; and one per contract, as every row here.
            let seen = asked.held.iter().any(|held| {
                held.con_id == def.con_id
                    || (held.exchange == def.exchange && held.multiplier == def.multiplier)
            });
            if !seen {
                asked.held.push(def);
            }
            return;
        }
        if !self.details_delivered.entry(req_id).or_default().insert(def.con_id as i64) {
            return;
        }
        if let Some(asked) = self.continuous_lookups.get_mut(&req_id) {
            asked.listed_found = true;
        }
        let for_event = clone_for_event(event_tx, &def);
        shared.reference.push_contract_details(req_id, def);
        if let Some(details) = for_event {
            emit(event_tx, Event::ContractDetails { req_id, details: Box::new(details) });
        }
    }

    /// End a caller's lookup.
    ///
    /// A continuous future's end is where the rest of its answer is settled:
    /// the listed months are asked for where the caller wanted them too, and
    /// otherwise the continuous contract is handed over, after any listed
    /// month, under the type a gateway reports it as. A lookup that found
    /// nothing a gateway would hand over is answered as a contract not found.
    fn end_lookup(
        &mut self,
        req_id: u32,
        ccp_conn: &mut Option<Connection>,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
        hb: &mut HeartbeatState,
    ) {
        if let Some(mut asked) = self.continuous_lookups.remove(&req_id) {
            if asked.listed_too && !asked.asking_listed {
                asked.asking_listed = true;
                let (symbol, exchange, currency) =
                    (asked.symbol.clone(), asked.exchange.clone(), asked.currency.clone());
                let (filters, include_expired) = (asked.filters.clone(), asked.include_expired);
                self.continuous_lookups.insert(req_id, asked);
                self.send_secdef_request_by_symbol(
                    req_id, &symbol, "FUT", &exchange, &currency, &filters, include_expired,
                    ccp_conn, hb, shared, event_tx,
                );
                return;
            }
            let found = if asked.listed_too { asked.listed_found } else { !asked.held.is_empty() };
            if !found {
                shared.reference.push_historical_error(
                    req_id, crate::error_codes::Refusal::NO_DEFINITION, NO_DEFINITION_FOUND.to_string(),
                );
            } else {
                for mut def in asked.held {
                    def.sec_type = crate::control::contracts::SecurityType::Other("CONTFUT".into());
                    let for_event = clone_for_event(event_tx, &def);
                    shared.reference.push_contract_details(req_id, def);
                    if let Some(details) = for_event {
                        emit(event_tx, Event::ContractDetails { req_id, details: Box::new(details) });
                    }
                }
            }
        }
        shared.reference.push_contract_details_end(req_id);
        emit(event_tx, Event::ContractDetailsEnd(req_id));
    }

    /// A caller's lookup the venue refused, or answered with no contract.
    ///
    /// Refused while a continuous future is asked for, the lookup goes on as
    /// though that part had ended, which is what a gateway does with it.
    #[allow(clippy::too_many_arguments)]
    fn refuse_lookup(
        &mut self,
        req_id: u32,
        code: i32,
        why: String,
        ccp_conn: &mut Option<Connection>,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
        hb: &mut HeartbeatState,
    ) {
        if self.continuous_lookups.get(&req_id).is_some_and(|asked| !asked.asking_listed) {
            // The caller is told nothing of it, so it is kept here: the only
            // word of why the continuous contract is missing from the answer.
            log::info!("continuous lookup req_id={req_id} refused ({code}): {why}");
            self.end_lookup(req_id, ccp_conn, shared, event_tx, hb);
            return;
        }
        self.fail_lookup(req_id, code, why, shared, event_tx);
    }

    /// A caller's lookup that ends without the venue's answer: unanswered, or
    /// cut off with the connection.
    fn fail_lookup(
        &mut self,
        req_id: u32,
        code: i32,
        why: String,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
    ) {
        self.continuous_lookups.remove(&req_id);
        shared.reference.push_historical_error(req_id, code, why);
        shared.reference.push_contract_details_end(req_id);
        emit(event_tx, Event::ContractDetailsEnd(req_id));
    }

    /// Ask the venue to name the contract a request wants, and hold the
    /// request until it does.
    ///
    /// Answers `false` when the request names nothing that can be looked up,
    /// so the caller sends it as it stands rather than holding it for ever.
    pub(crate) fn hold_until_named(
        &mut self,
        cmd: crate::types::ControlCommand,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) -> Option<crate::types::ControlCommand> {
        // Given by id alone, it is asked for by that id.
        if let Some((con_id, exchange)) = named_by_id_alone(&cmd) {
            let req_id = self.next_internal_secdef_id;
            self.next_internal_secdef_id = self.next_internal_secdef_id.wrapping_add(1);
            self.send_secdef_request(req_id, con_id, exchange, ccp_conn, hb, shared, &None);
            self.pending_named.push((req_id, cmd, Instant::now()));
            return None;
        }
        // Cloned rather than borrowed: the command is moved onto the pending
        // list below, and what it named has to outlive it.
        let named = match contract_named(&cmd) {
            Some(c) if !c.symbol.is_empty() => c.clone(),
            _ => return Some(cmd),
        };
        let filters = described(filters_named(&cmd));
        // Stated on the request, a gateway states it on the lookup that names
        // the request's contract too.
        let include_expired = matches!(
            cmd,
            crate::types::ControlCommand::FetchHistorical { include_expired: true, .. }
                | crate::types::ControlCommand::FetchHeadTimestamp { include_expired: true, .. }
                | crate::types::ControlCommand::FetchHistoricalTicks { include_expired: true, .. }
        );
        let req_id = self.next_internal_secdef_id;
        self.next_internal_secdef_id = self.next_internal_secdef_id.wrapping_add(1);
        self.send_secdef_request_by_symbol(
            req_id, &named.symbol, &named.sec_type, &named.exchange, &named.currency,
            &filters, include_expired, ccp_conn, hb, shared, &None,
        );
        self.pending_named.push((req_id, cmd, Instant::now()));
        None
    }

    /// Ask the venue to name the contract an order describes. Answered on
    /// [`orders_named`](Self::orders_named) under the number this returns;
    /// `None` where there is no connection to ask on.
    pub(crate) fn name_for_an_order(
        &mut self,
        contract: &crate::types::model::Contract,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) -> Option<u32> {
        ccp_conn.as_ref()?;
        let req_id = self.next_internal_secdef_id;
        self.next_internal_secdef_id = self.next_internal_secdef_id.wrapping_add(1);
        if contract.con_id != 0 {
            self.send_secdef_request(req_id, contract.con_id, &contract.exchange, ccp_conn, hb, shared, &None);
        } else {
            // The caller's description narrows the lookup, including an
            // identifier where one was stated.
            self.send_secdef_request_by_symbol(
                req_id, &contract.symbol, &contract.sec_type, &contract.exchange, &contract.currency,
                &contract.lookup_filters(), contract.include_expired, ccp_conn, hb, shared, &None,
            );
        }
        self.order_naming.push((req_id, Instant::now()));
        Some(req_id)
    }

    /// Withdraw a request that is still waiting to be named.
    ///
    /// A request naming its contract by symbol is parked whole while the
    /// venue is asked what that contract is, and at that moment it is in
    /// neither the in-flight record nor the held one -- so a cancel arriving
    /// in that window found nothing, sent nothing, and returned. The naming
    /// answer then arrived, the request was re-injected and sent, and the
    /// caller was served a full answer to a request it had withdrawn.
    ///
    /// Called from the cancel of every request that can be parked this way.
    /// The second list covers a naming answer re-injected in a pass before the
    /// cancel was read; once the request has been sent it is in the in-flight
    /// record and the ordinary withdrawal reaches it.
    pub(crate) fn withdraw_named(&mut self, req_id: u32, kind: impl Fn(&crate::types::ControlCommand) -> bool) -> bool {
        let before = self.pending_named.len() + self.resolved_named.len();
        self.pending_named.retain(|(_, cmd, _)| !(request_id(cmd) == Some(req_id) && kind(cmd)));
        self.resolved_named.retain(|cmd| !(request_id(cmd) == Some(req_id) && kind(cmd)));
        self.pending_named.len() + self.resolved_named.len() != before
    }

    /// A held request whose contract the venue never named. Told to the caller
    /// rather than left waiting.
    pub(crate) fn sweep_pending_named(&mut self, shared: &SharedState) {
        // An order's, which the engine refuses once the lookup is given up on.
        let now = Instant::now();
        let mut given_up = Vec::new();
        self.order_naming.retain(|(rid, asked_at)| {
            let waiting = now.duration_since(*asked_at) < Self::NAMING_TIMEOUT;
            if !waiting {
                given_up.push(*rid);
            }
            waiting
        });
        self.orders_named.extend(given_up.into_iter().map(|rid| (rid, OrderNamed::Unnamed(0))));
        if self.pending_named.is_empty() {
            return;
        }
        let mut gave_up = Vec::new();
        self.pending_named.retain(|(_, cmd, asked_at)| {
            if now.duration_since(*asked_at) < Self::NAMING_TIMEOUT {
                return true;
            }
            gave_up.push(cmd.clone());
            false
        });
        for cmd in gave_up {
            Self::abandon_named(&cmd, 0, shared);
        }
    }

    /// Ask the venue for the contracts a caller's details request describes.
    ///
    /// A continuous future is asked for first, and where the caller named the
    /// listed months as well they are asked for once it is answered, the way a
    /// gateway asks for them; what the two lookups find is put together as the
    /// lookup ends.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn send_contract_details_lookup(&mut self, req_id: u32, symbol: &str, sec_type: &str, exchange: &str, currency: &str, filters: &crate::types::SecDefFilters, include_expired: bool, ccp_conn: &mut Option<Connection>, hb: &mut HeartbeatState, shared: &SharedState, event_tx: &Option<EventSink>) {
        // By the type alone, as a gateway routes it: named by an identifier,
        // each of the two lookups asks by that identifier.
        if matches!(sec_type, "CONTFUT" | "FUT+CONTFUT" | "CONTFUT+FUT") {
            self.continuous_lookups.insert(req_id, ContinuousLookup {
                listed_too: sec_type != "CONTFUT",
                asking_listed: false,
                listed_found: false,
                held: Vec::new(),
                symbol: symbol.to_string(),
                exchange: exchange.to_string(),
                currency: currency.to_string(),
                filters: filters.clone(),
                include_expired,
            });
        } else {
            self.continuous_lookups.remove(&req_id);
        }
        self.send_secdef_request_by_symbol(
            req_id, symbol, sec_type, exchange, currency, filters, include_expired, ccp_conn, hb, shared, event_tx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn send_secdef_request_by_symbol(&mut self, req_id: u32, symbol: &str, sec_type: &str, exchange: &str, currency: &str, filters: &crate::types::SecDefFilters, include_expired: bool, ccp_conn: &mut Option<Connection>, hb: &mut HeartbeatState, shared: &SharedState, event_tx: &Option<EventSink>) {
        // A public identifier and the tags it rides on. When one is set the
        // lookup rides the identifier and drops the symbol/secType/filters.
        let identifier_fields = identifier_fields(filters);
        let identifier_lookup = !identifier_fields.is_empty();

        if let Some(conn) = ccp_conn.as_mut() {
            let req_id_str = req_id.to_string();
            let ts = chrono_free_timestamp();
            let fix_exchange = if exchange == "SMART" { "BEST" } else { exchange };
            // The protocol has no security type of its own for a continuous
            // future. The contract goes out as a future and a separate field
            // asks for the current lead month instead of a listed one; sent as
            // its own type the whole message is refused. The spelling that
            // asks for the expiring months as well asks for the continuous one
            // the same way — the listed months are a second lookup, made once
            // this one is answered.
            let continuous_future = matches!(sec_type, "CONTFUT" | "FUT+CONTFUT" | "CONTFUT+FUT");
            // A contract named only by its issuer is answered as fixed income,
            // whatever type the caller stated: the issuer rides a field of its
            // own and the type it is looked up under is not the caller's to
            // choose.
            let issuer_id = filters.issuer_id.as_str();
            let fix_sec_type = if continuous_future {
                "FUT"
            } else if !issuer_id.is_empty() {
                "FIXED"
            } else {
                match sec_type {
                    "STK" => "CS", "FUT" => "FUT", "OPT" => "OPT", "IND" => "IND", other => other,
                }
            };
            // A news feed states its provider where every other contract states
            // a venue, and the protocol carries the provider under a field of
            // its own: without it the message is refused outright. Where the
            // exchange names provider and feed together the feed is the half
            // that is wanted. The provider having moved, neither a venue nor a
            // currency rides with it, and a feed carries no trading class.
            let news_source = if sec_type == "NEWS" {
                match exchange.split(':').collect::<Vec<_>>()[..] {
                    [_, feed] => feed,
                    [provider, ..] => provider,
                    [] => "",
                }
            } else {
                ""
            };
            let strike_str = if filters.strike > 0.0 { format!("{}", filters.strike) } else { String::new() };
            // PutOrCall: Call = 1, Put = 0.
            let right_code = match filters.right.to_uppercase().as_str() {
                "C" | "CALL" => "1",
                "P" | "PUT" => "0",
                _ => "",
            };
            // Exchange rides tag 100; primaryExchange (when set) rides tag 207 —
            // the two were previously conflated onto 207. localSymbol replaces the
            // plain symbol; the derivative/disambiguation filters are added only
            // when set. Captured in.
            let mut fields: Vec<(u32, &str)> = vec![
                (fix::TAG_MSG_TYPE, "c"),
                (fix::TAG_SENDING_TIME, &ts),
                (320, &req_id_str),
                (321, "2"),
            ];
            if !news_source.is_empty() {
                fields.push((6825, news_source));
            }
            if identifier_lookup {
                // Identifier lookup: the identifier and its source replace the
                // symbol/secType/filters; exchange, primary exchange and
                // currency still ride.
                fields.extend_from_slice(&identifier_fields);
            } else {
                // Both, where the caller stated both. The protocol carries
                // the symbol and then the venue's local symbol from
                // separate fields of the request, and neither suppresses the
                // other. Sending only
                // the local symbol asked a narrower question than the caller
                // put, and a symbol that disagrees with it — which the venue
                // would refuse — matched whatever the local symbol named.
                if !symbol.is_empty() {
                    fields.push((55, symbol));
                }
                if !filters.local_symbol.is_empty() && !continuous_future {
                    fields.push((6035, &filters.local_symbol));
                }
                // A continuous future is not looked up by its listed months, so
                // its class rides a field of its own; a news feed is not looked
                // up by class at all.
                if !filters.trading_class.is_empty() && news_source.is_empty() {
                    fields.push((if continuous_future { 8362 } else { 6058 }, &filters.trading_class));
                }
                fields.push((167, fix_sec_type));
                if let Some(tag) = maturity_tag(&filters.last_trade_date_or_contract_month) {
                    fields.push((tag, &filters.last_trade_date_or_contract_month));
                }
                if continuous_future {
                    fields.push((6857, "2"));
                }
                if !right_code.is_empty() {
                    fields.push((201, right_code));
                }
                if !strike_str.is_empty() {
                    fields.push((202, &strike_str));
                }
                if !filters.multiplier.is_empty() {
                    fields.push((231, &filters.multiplier));
                }
            }
            if news_source.is_empty() {
                fields.push((100, fix_exchange));
            }
            if !filters.primary_exchange.is_empty() {
                fields.push((207, &filters.primary_exchange));
            }
            if news_source.is_empty() {
                fields.push((15, currency));
            }
            // An ISIN asked about anywhere but SMART is asked of any type:
            // the identifier alone does not say which listing is meant.
            if identifier_fields.first() == Some(&(22, "4")) && exchange != "SMART" {
                fields.push((167, "ANY"));
            }
            // The issuer rides a lookup by description; beside an identifier a
            // gateway states nothing of it.
            if !identifier_lookup {
                fields.push((6454, issuer_id));
            }
            fields.push((6088, "Socket"));
            // A contract that has already expired is in scope where the caller
            // said so, on a lookup by description. An identifier names one
            // contract and a gateway states nothing more beside it.
            if include_expired && !identifier_lookup {
                fields.push((6320, "1"));
            }
            // A gateway writes no field it has nothing for: an empty venue or
            // currency is left out, not stated as nothing.
            fields.retain(|(_, value)| !value.is_empty());
            if let Err(e) = conn.send_fix(&fields) {
                if self.refuse_unsent_lookup(req_id, &e.to_string(), shared, event_tx) {
                    return;
                }
            } else {
                log::info!("Sent secdef lookup: req_id={req_id} symbol={symbol} sec_type={sec_type} identifier={identifier_lookup}");
                hb.last_ccp_sent = Instant::now();
            }
        } else if self.refuse_unsent_lookup(req_id, "no connection to the venue", shared, event_tx) {
            return;
        }
        // By-symbol lookup: master reply carries `6046={exch_list}`. The
        // server never emits a 323=5/6 terminator; completion is detected
        // by counting per-exchange fan-out replies (see `pending_fanout`).
        self.details_delivered.remove(&req_id);
        self.pending_secdef.push((req_id, false, Instant::now() + unanswered_after(req_id)));
    }

    pub(crate) fn send_attached_quote_definition(
        &mut self, req_id: u32, con_id: u32, exchange: &str,
        connection: &mut Option<Connection>, heartbeat: &mut HeartbeatState, shared: &SharedState,
    ) {
        if exchange.is_empty() {
            self.send_secdef_request(req_id, i64::from(con_id), exchange, connection, heartbeat, shared, &None);
        } else {
            self.send_fanout_secdef_request(&req_id.to_string(), i64::from(con_id), exchange, connection, heartbeat);
            self.pending_secdef.push((req_id, true, Instant::now() + SECDEF_TIMEOUT));
        }
    }

    /// Send a per-exchange fan-out request after a by-symbol master reply.
    /// Wire: `35=c|320={fanout_id}|321=2|146=1|6008={conid}|6004={exch}|`
    pub(crate) fn send_fanout_secdef_request(
        &mut self,
        fanout_req_id: &str,
        con_id: i64,
        exchange: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        if let Some(conn) = ccp_conn.as_mut() {
            let con_id_str = con_id.to_string();
            let ts = chrono_free_timestamp();
            let _ = conn.send_fix(&[
                (fix::TAG_MSG_TYPE, "c"),
                (fix::TAG_SENDING_TIME, &ts),
                (crate::control::contracts::TAG_SECURITY_REQ_ID, fanout_req_id),
                (crate::control::contracts::TAG_SECURITY_REQ_TYPE, "2"),
                (146, "1"),
                (crate::control::contracts::TAG_IB_CON_ID, &con_id_str),
                (6004, exchange),
            ]);
            hb.last_ccp_sent = Instant::now();
        }
    }

    /// Ask the venue which contracts match a pattern, one search at a time.
    ///
    /// The venue may answer a search without naming it, and names none but
    /// the one it is answering, so two on the wire at once cannot be told
    /// apart. A search asked while another is on the wire waits for that one's
    /// reply, its refusal, or the end of the connection.
    pub(crate) fn ask_matching_symbols(&mut self, req_id: u32, pattern: &str, ccp_conn: &mut Option<Connection>, hb: &mut HeartbeatState, shared: &SharedState) {
        if self.matching_symbols_in_flight() || !self.queued_matching_symbols.is_empty() {
            self.queued_matching_symbols.push_back((req_id, pattern.to_string()));
            return;
        }
        self.send_matching_symbols_request(req_id, pattern, ccp_conn, hb, shared);
    }

    /// Whether a search is on the wire, answered or not to its caller.
    fn matching_symbols_in_flight(&self) -> bool {
        !self.pending_matching_symbols.is_empty() || self.matching_symbols_abandoned.is_some()
    }

    /// Send the searches held back, now that the wire is free: each until one
    /// is on it. One that cannot be sent is refused where it is sent, and the
    /// next goes.
    pub(crate) fn send_next_matching_symbols(&mut self, ccp_conn: &mut Option<Connection>, hb: &mut HeartbeatState, shared: &SharedState, left: &mut usize) {
        while *left > 0 && !self.matching_symbols_in_flight() {
            let Some((req_id, pattern)) = self.queued_matching_symbols.pop_front() else { return };
            *left -= 1;
            self.send_matching_symbols_request(req_id, &pattern, ccp_conn, hb, shared);
        }
    }

    pub(crate) fn send_matching_symbols_request(&mut self, req_id: u32, pattern: &str, ccp_conn: &mut Option<Connection>, hb: &mut HeartbeatState, shared: &SharedState) {
        // Recorded only where the request went out, so a request issued while
        // the transport is down is not queued as pending with nothing on the
        // wire to answer it.
        let Some(conn) = ccp_conn.as_mut() else {
            log::warn!("Matching symbols request req_id={req_id} pattern='{pattern}' not sent: no CCP transport");
            // Refused rather than answered empty: an empty answer is a search
            // the venue ran and nothing matched, which is not what happened.
            shared.reference.push_historical_error(
                req_id, crate::error_codes::Refusal::NOT_CONNECTED,
                "matching symbols request could not be sent: no connection to the venue".to_string(),
            );
            return;
        };
        let req_id_str = req_id.to_string();
        let ts = chrono_free_timestamp();
        if let Err(e) = conn.send_fix(&[
            (fix::TAG_MSG_TYPE, "U"),
            (fix::TAG_SENDING_TIME, &ts),
            (6040, "185"),
            (320, &req_id_str),
            (58, pattern),
        ]) {
            log::warn!("Matching symbols request req_id={req_id} pattern='{pattern}' not sent: {e}");
            shared.reference.push_historical_error(
                req_id, crate::error_codes::Refusal::NOT_CONNECTED,
                format!("matching symbols request could not be sent: {e}"),
            );
            return;
        }
        hb.last_ccp_sent = Instant::now();
        log::info!("Sent matching symbols request: req_id={req_id} pattern='{pattern}'");
        self.pending_matching_symbols.push((req_id, Instant::now() + MATCHING_SYMBOLS_TIMEOUT));
    }

    /// Give up on matching-symbols requests the gateway never answered.
    ///
    /// Nothing expired them, so an unanswered request stayed in the queue for
    /// the life of the process — and the reply matcher falls back to the head
    /// of that queue when a reply carries no echoed request id, so a stale entry
    /// could absorb a later request's answer.
    pub(crate) fn sweep_pending_matching_symbols(&mut self, shared: &SharedState) {
        if self.pending_matching_symbols.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut abandoned = None;
        self.pending_matching_symbols.retain(|(req_id, deadline)| {
            if now >= *deadline {
                log::warn!("Matching symbols request req_id={req_id} unanswered after {MATCHING_SYMBOLS_TIMEOUT:?} — giving up");
                // The timeout is answered, not merely recorded: a caller
                // told nothing waits on a request this session has abandoned.
                // Refused rather than answered empty: an empty answer is a
                // search the venue ran and nothing matched, and the venue
                // never answered this one at all.
                shared.reference.push_historical_error(
                    *req_id, crate::error_codes::Refusal::NO_ANSWER,
                    "matching symbols request timed out — no reply from the gateway".to_string(),
                );
                abandoned = Some(*req_id);
                false
            } else {
                true
            }
        });
        // Still on the wire. Its reply may name no request, and the next
        // search is not sent until that reply, a refusal of it, or the end of
        // the connection says the wire is free.
        if abandoned.is_some() {
            self.matching_symbols_abandoned = abandoned;
        }
    }

    /// Give up on an advisor request the venue never answered.
    ///
    /// The reply states only this client's own number for the question, so a
    /// question given up on is a reply nobody can be given — the entry goes
    /// with the refusal rather than staying behind to match a late one onto a
    /// caller who has already been told.
    pub(crate) fn sweep_pending_advisor(&mut self, shared: &SharedState) {
        if self.pending_advisor.is_empty() {
            return;
        }
        let now = Instant::now();
        self.pending_advisor.retain(|key, asked| {
            if now >= asked.deadline {
                log::warn!(
                    "Advisor configuration request {key} unanswered after {ADVISOR_TIMEOUT:?} \
                     — giving up",
                );
                shared.reference.push_advisor_refused(
                    asked.origin(), crate::error_codes::Refusal::NO_ANSWER,
                    "the advisor configuration request timed out — no reply from the venue"
                        .to_string(),
                );
                false
            } else {
                true
            }
        });
    }

    /// Note what a definition says its contract is written on, and ask what
    /// that pays out.
    ///
    /// Every definition, not the ones on one path: a contract is looked up by
    /// its own id, by symbol, for a subscription, for a scanner row and for
    /// the engine's own reasons, and the underlying is stated on all of them.
    /// Done on one of those paths only, an option asked for by id alone could
    /// never reach its schedule and was priced on the fallback for the life of
    /// the session.
    fn note_what_it_is_written_on(
        &mut self,
        def: &crate::control::contracts::ContractDefinition,
        shared: &SharedState,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        shared.reference.note_under_con_id(def.con_id, def.under_con_id);
        if def.under_con_id != 0 {
            self.send_dividends_query(def.under_con_id, ccp_conn, hb);
        }
    }

    /// Ask the venue what a contract pays out.
    ///
    /// Not a document like the other reference queries: the venue takes a line
    /// of text on tag 58 and answers with a small XML document on 6118, under
    /// the id this client asked with. That id is the only thing tying an answer
    /// to a question — the answer names neither the contract nor the query —
    /// so it is kept here against the contract until it comes back.
    ///
    /// Asked once per contract, and not again while one is outstanding: the
    /// answer is a fact about the contract rather than a subscription, and a
    /// second question would be answered with the same schedule under an id
    /// nothing was waiting on.
    pub(crate) fn send_dividends_query(
        &mut self,
        con_id: u32,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        if con_id == 0
            || self.dividends_answered.contains(&con_id)
            || self.pending_dividends.iter().any(|(_, held, _)| *held == con_id)
        {
            return;
        }
        let query = crate::control::dividends::query_for(con_id);
        // Registered as outstanding only if it went out. Recorded either way,
        // the contract would never be asked about again.
        if let Some(query_id) = self.send_text_query(&query, ccp_conn, hb) {
            log::info!("Asked what {con_id} pays out, under {query_id}");
            self.pending_dividends.push((
                query_id, con_id, Instant::now() + COMPLETED_ORDERS_TIMEOUT,
            ));
        }
    }

    /// Ask the venue the rates it states for a currency, once: the query a
    /// contract's schedule is asked with, naming the currency.
    pub(crate) fn ask_currency_rates(
        &mut self,
        currency: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        if currency.is_empty()
            || self.rates_answered.contains(currency)
            || self.rates_asked.iter().any(|(_, asked)| asked == currency)
        {
            return;
        }
        let query = crate::control::dividends::query_for_currency(currency);
        if let Some(query_id) = self.send_text_query(&query, ccp_conn, hb) {
            self.rates_asked.push((query_id, currency.to_string()));
        }
    }

    /// Send a text query under an id of this client's own, which is what its
    /// answer is filed by.
    fn send_text_query(
        &mut self,
        query: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) -> Option<String> {
        let Some(conn) = ccp_conn.as_mut() else {
            log::debug!("{query} could not be asked: no connection to the venue");
            return None;
        };
        let query_id = format!("div_{}", self.next_xml_query_id);
        self.next_xml_query_id += 1;
        let ts = chrono_free_timestamp();
        match conn.send_fix(&[
            (fix::TAG_MSG_TYPE, "U"),
            (fix::TAG_SENDING_TIME, &ts),
            (6040, "27"),
            (320, &query_id),
            (58, query),
        ]) {
            Ok(()) => {
                hb.last_ccp_sent = Instant::now();
                Some(query_id)
            }
            Err(e) => {
                log::warn!("{query} could not be asked: {e}");
                None
            }
        }
    }

    /// Ask the venue a contract's sessions for the engine's own use, by the
    /// key its definition is joined to them on, as a gateway asks: once for
    /// the key, whichever contracts on it wait, and not where the key's are
    /// in hand. A contract whose definition is not in hand has it looked up
    /// first, by its id on the exchange it was asked for on, as a gateway
    /// looks up the contract of every request; the sessions are asked for
    /// once the definition states the key.
    pub(crate) fn ask_schedule(
        &mut self,
        con_id: u32,
        exchange: &str,
        shared: &SharedState,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        let key = match shared.reference.schedule_key(con_id) {
            Some(key) => key,
            None => {
                let Some(key) = shared.reference.contract_definition(con_id, "")
                    .map(|definition| definition.join_key)
                    .filter(|key| !key.is_empty())
                else {
                    if ccp_conn.is_some() && con_id != 0 && !self.definitions_asked.contains(&con_id) {
                        self.definitions_asked.push(con_id);
                        let req_id = self.next_internal_secdef_id;
                        self.next_internal_secdef_id = self.next_internal_secdef_id.wrapping_add(1);
                        self.send_secdef_request(req_id, i64::from(con_id), exchange, ccp_conn, hb, shared, &None);
                    }
                    return;
                };
                shared.reference.note_schedule_key(con_id, &key);
                key
            }
        };
        if shared.reference.contract_schedule(con_id, |_| ()).is_none() {
            self.ask_schedule_by_key(&key, ccp_conn, hb);
        }
        if !self.schedule_keys.contains(&key) {
            self.schedule_keys.push(key);
        }
    }

    fn ask_schedule_by_key(
        &mut self,
        key: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        if ccp_conn.is_none() || self.schedules_asked.iter().any(|asked| asked == key) {
            return;
        }
        self.send_schedule_subscribe(key, ccp_conn, hb);
        self.schedules_asked.push(key.to_string());
    }

    /// Ask again for the sessions of every key the engine uses once the day
    /// the venue's clock reads on this machine's calendar turns, as a gateway
    /// does when its day turns: what it holds runs about a week ahead. Those
    /// held stand until the answer replaces them.
    fn ask_schedules_again_on_a_new_day(
        &mut self,
        sent_at: &str,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
    ) {
        let Some(day) = crate::protocol::datetime::ib_datetime_to_unix_millis(sent_at)
            .and_then(|at| jiff::Timestamp::from_millisecond(at).ok())
            .map(|at| at.to_zoned(jiff::tz::TimeZone::system()).date())
        else {
            return;
        };
        let held = self.schedules_day;
        if held.is_some_and(|held| day <= held) {
            return;
        }
        self.schedules_day = Some(day);
        if held.is_some() {
            for key in self.schedule_keys.clone() {
                self.ask_schedule_by_key(&key, ccp_conn, hb);
            }
        }
    }

    /// One answer to that question, filed against the contract it is about.
    ///
    /// The id is what says which contract, because the answer states no
    /// contract of its own. An id nothing is waiting on is an answer to a
    /// question this session did not ask, which is not a schedule to file.
    fn handle_dividends_answer(
        &mut self,
        parsed: &std::collections::HashMap<u32, String>,
        shared: &SharedState,
    ) {
        let Some(query_id) = parsed.get(&320) else { return };
        if let Some(at) = self.rates_asked.iter().position(|(id, _)| id == query_id) {
            let (_, currency) = self.rates_asked.remove(at);
            let rates = parsed.get(&6118).map(|body| crate::control::dividends::parse(body).term_rates);
            log::debug!("{currency} rates: {rates:?}");
            self.rates_answered.insert(currency.clone());
            shared.reference.set_currency_rates(&currency, rates.unwrap_or_default());
            return;
        }
        let con_id = match self.pending_dividends.iter().position(|(id, _, _)| id == query_id) {
            Some(at) => self.pending_dividends.remove(at).1,
            // Late, and still an answer. The id this session asked under is
            // the only thing that says which contract it is about, so a
            // schedule that arrived a moment after the wait ran out is filed
            // rather than thrown away.
            None => match self
                .dividends_given_up_on
                .iter()
                .position(|(id, _)| id == query_id)
            {
                Some(at) => {
                    let (_, con_id) = self.dividends_given_up_on.remove(at).expect("just found");
                    // Unless a later question about the same contract has
                    // already been answered. This one was asked first and
                    // arrived last, so filing it would put the older schedule
                    // over the newer one.
                    if self.dividends_answered.contains(&con_id) {
                        log::debug!("a later answer for {con_id} already stands");
                        return;
                    }
                    log::debug!("what {con_id} pays out arrived after the wait ran out");
                    con_id
                }
                None => {
                    log::debug!("an answer arrived under {query_id}, which nothing here asked");
                    return;
                }
            },
        };
        let Some(body) = parsed.get(&6118) else {
            // Not marked answered. The mark is what stops this being asked
            // again, and it is never lifted — so a reply carrying no schedule
            // at all, set down as an answer, left that underlying without one
            // for as long as the process ran and nothing asked again.
            log::debug!("the answer for {con_id} states no schedule");
            return;
        };
        self.dividends_answered.insert(con_id);
        let schedule = crate::control::dividends::parse(body);
        log::info!(
            "{con_id} pays out {} times over the venue's books",
            schedule.payments.len(),
        );
        shared.reference.set_dividend_schedule(con_id, schedule);
    }

    /// Ask for an underlying's option chain, one request per symbol and
    /// underlying on the wire at a time.
    ///
    /// A chain reply names its underlying and no request, so a second request
    /// for the same underlying sent while the first is out is answered by
    /// whichever reply comes first. It waits for the first's reply, a
    /// refusal of it, or the end of the connection.
    pub(crate) fn ask_option_params(
        &mut self,
        asked: QueuedChain,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) {
        if self.chain_on_the_wire(&asked.symbol, asked.underlying_con_id)
            || self.queued_option_params.iter().any(|q| {
                q.symbol.eq_ignore_ascii_case(&asked.symbol) && q.underlying_con_id == asked.underlying_con_id
            })
        {
            self.queued_option_params.push_back(asked);
            return;
        }
        self.send_option_params_request(
            asked.req_id, &asked.symbol, &asked.fut_fop_exchange, &asked.underlying_sec_type,
            asked.underlying_con_id, ccp_conn, hb, shared,
        );
    }

    /// Whether a chain for this symbol and underlying is on the wire.
    fn chain_on_the_wire(&self, symbol: &str, underlying_con_id: i64) -> bool {
        self.pending_option_params.iter().any(|(_, pending, con_id)| {
            pending.eq_ignore_ascii_case(symbol) && *con_id == underlying_con_id
        })
    }

    /// Send each chain request held back whose underlying the wire is now
    /// free for, in the order they were asked.
    pub(crate) fn send_next_option_params(
        &mut self,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
        left: &mut usize,
    ) {
        let mut at = 0;
        while *left > 0 && at < self.queued_option_params.len() {
            let q = &self.queued_option_params[at];
            let behind_an_earlier = self.queued_option_params.iter().take(at).any(|e| {
                e.symbol.eq_ignore_ascii_case(&q.symbol) && e.underlying_con_id == q.underlying_con_id
            });
            if behind_an_earlier || self.chain_on_the_wire(&q.symbol, q.underlying_con_id) {
                at += 1;
                continue;
            }
            let Some(q) = self.queued_option_params.remove(at) else { return };
            *left -= 1;
            self.send_option_params_request(
                q.req_id, &q.symbol, &q.fut_fop_exchange, &q.underlying_sec_type,
                q.underlying_con_id, ccp_conn, hb, shared,
            );
        }
    }

    /// Ask for the option chain of an underlying.
    ///
    /// A request that cannot go out is refused rather than left unanswered,
    /// because the caller is waiting on the end of a request nothing on the
    /// wire will ever end.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn send_option_params_request(
        &mut self,
        req_id: u32,
        symbol: &str,
        fut_fop_exchange: &str,
        underlying_sec_type: &str,
        underlying_con_id: i64,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) {
        let symbol = symbol.to_uppercase();
        // The request names the UNDERLYING's own type, not the derivative being
        // enumerated. Naming the derivative is answered "Unknown contract":
        // there is no option contract by that symbol, only a stock that has
        // options on it.
        //
        // A caller who states nothing claims nothing. An unstated type sent as
        // STK asks about a stock of that symbol where the caller meant an index
        // or a future. Tag 310 is omitted for an empty
        // security type rather than standing one in: its chain-request writer
        // states the type only when the caller gave one, and treats its own
        // empty-named type as absent.
        let underlying = underlying_sec_type;
        // A futures option whose underlying is not itself a future names that
        // underlying on a tag of its own.
        let futures_option = !fut_fop_exchange.is_empty() && underlying != "FUT";
        let con_id_tag = if futures_option { 6457 } else { 6346 };
        let Some(conn) = ccp_conn.as_mut() else {
            log::warn!("Option chain request req_id={req_id} symbol={symbol} not sent: no CCP transport");
            // Refused rather than answered empty: an empty answer is a chain
            // the venue enumerated and found nothing in, which is not what
            // happened.
            shared.reference.push_historical_error(
                req_id, crate::error_codes::Refusal::NOT_CONNECTED,
                "option chain request could not be sent: no connection to the venue".to_string(),
            );
            return;
        };
        let con_id_str = underlying_con_id.to_string();
        let ts = chrono_free_timestamp();
        let mut fields: Vec<(u32, &str)> = vec![
            (fix::TAG_MSG_TYPE, "U"),
            (fix::TAG_SENDING_TIME, &ts),
            (6040, "138"),
            (55, &symbol),
        ];
        if !underlying.is_empty() {
            fields.push((310, underlying));
        }
        fields.push((con_id_tag, &con_id_str));
        fields.push((6320, "1"));
        fields.push((6994, "1"));
        if underlying_sec_type == "FUT" {
            fields.push((6995, fut_fop_exchange));
        }
        if let Err(e) = conn.send_fix(&fields) {
            log::warn!("Option chain request req_id={req_id} symbol={symbol} not sent: {e}");
            shared.reference.push_historical_error(
                req_id, crate::error_codes::Refusal::NOT_CONNECTED,
                format!("option chain request could not be sent: {e}"),
            );
            return;
        }
        hb.last_ccp_sent = Instant::now();
        log::info!("Sent option chain request: req_id={req_id} symbol={symbol} con_id={underlying_con_id}");
        self.pending_option_params.push((req_id, symbol, underlying_con_id));
    }

    /// A chain reply names its underlying by symbol and echoes no request id,
    /// so it answers the oldest request outstanding for that symbol.
    fn handle_option_chain(
        &mut self,
        msg: &[u8],
        _ccp_conn: &mut Option<Connection>,
        _hb: &mut HeartbeatState,
        shared: &SharedState,
    ) {
        let Some(scopes) = crate::control::contracts::parse_option_chain_response(msg) else { return };

        // One reply can carry the venues of more than one underlying. Handing
        // the lot to the first one's request gives that caller another
        // underlying's strikes, and leaves the other request to time out with
        // an empty chain — so each underlying is answered to the request that
        // asked for it.
        let mut by_underlying: Vec<(String, Vec<_>)> = Vec::new();
        for scope in scopes {
            match by_underlying.iter_mut().find(|(sym, _)| sym.eq_ignore_ascii_case(&scope.symbol)) {
                Some((_, group)) => group.push(scope),
                None => by_underlying.push((scope.symbol.clone(), vec![scope])),
            }
        }
        // An underlying the venue lists nothing for still answers the request,
        // and then the symbol tag is all there is to attribute it by.
        if by_underlying.is_empty() {
            let symbol = extract_tag_value(msg, b"55=").unwrap_or_default();
            by_underlying.push((symbol, Vec::new()));
        }

        for (symbol, scopes) in by_underlying {
            // Matched on the underlying the reply names, not on its ticker
            // alone: the reply echoes no request id and does carry the
            // underlying's own id, so two chains for one ticker — a share and
            // the future on it, asked for at once — are told apart by the only
            // thing that distinguishes them. Where the reply states none, the
            // ticker is all there is.
            let stated = scopes.iter().map(|s| s.underlying_con_id).find(|id| *id != 0);
            let Some(pos) = self.pending_option_params.iter()
                .position(|(_, pending, con_id)| {
                    pending.eq_ignore_ascii_case(&symbol)
                        && stated.is_none_or(|named| *con_id == 0 || named == *con_id)
                })
            else {
                log::warn!("Option chain reply for '{symbol}' matches no request");
                continue;
            };
            let (req_id, _, asked_under) = self.pending_option_params.remove(pos);
            // The underlying as the venue names it, where it names one; a
            // caller who asked without the id is answered with it.
            let con_id = stated.unwrap_or(asked_under);
            log::info!("Option chain reply: req_id={req_id} symbol={symbol} scopes={}", scopes.len());
            shared.reference.push_option_params(req_id, con_id, scopes);
        }
    }

    /// A caller's question of what the venue has finished: numbered with a
    /// turn of its own, and sent in its turn.
    pub(crate) fn ask_completed_orders(
        &mut self,
        api_only: bool,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) {
        self.completed_orders_next_turn += 1;
        let turn = self.completed_orders_next_turn;
        self.completed_orders_api_only.insert(turn, api_only);
        self.send_completed_orders_request(turn, ccp_conn, hb, shared);
    }

    /// The question asked on this turn is over: the venue said it had
    /// finished, or the connection it was asked on went. Its answer is
    /// pushed here, behind every finished order the window delivered.
    fn end_completed_orders(&mut self, turn: u64, shared: &SharedState) {
        shared.orders.note_completed_orders_end_on(turn);
        let api_only = self.completed_orders_api_only.remove(&turn).unwrap_or(false);
        shared.push_call_record(crate::bridge::Record::Answer(
            crate::bridge::Answer::CompletedOrders { api_only },
        ));
    }

    /// How many questions of what the venue has finished are held, unsent.
    pub(crate) fn completed_orders_questions_held(&self) -> usize {
        self.completed_orders_queued.len()
    }

    /// Ask the venue for the orders it has finished.
    ///
    /// The same message the session opens with — the mass status request,
    /// which asks for everything still working — with the mode that asks for
    /// what is done instead, and the window it covers. There is no request of
    /// its own for this: one tag turns the one already being sent into it.
    ///
    /// What comes back is not a document. It is a run of ordinary execution
    /// reports, one per event in each order's life, ending with the sentinel
    /// that ends the opening replay. So the window between the ask and that
    /// sentinel is the whole mechanism, and it is held here.
    pub(crate) fn send_completed_orders_request(
        &mut self,
        turn: u64,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
    ) {
        // Said first, whatever becomes of the question below: what a caller
        // waits on is that the engine has taken its question off the queue.
        shared.orders.note_completed_orders_asked();
        // Not while the session's own replay is still running. Both answers
        // end with the same sentinel and nothing on the wire says which
        // question a sentinel answers, so a window opened across the replay is
        // shut by the replay's ending and the history that follows is read as
        // live. Held instead, and sent the moment the replay is over.
        //
        // Nor while an earlier question is still being answered. Two of them
        // at once share one window and one sentinel: the first sentinel to
        // arrive shuts the window on both, so the second answer took the live
        // path and only one of the two callers was ever released. Asked one at
        // a time, each has a sentinel of its own.
        //
        // The replay half of that wait has an end: an account with nothing
        // working ends its replay without naming an order, and naming one is
        // what says the replay has begun — so a question held for it would
        // wait for ever on exactly the accounts most likely to ask. Held for
        // as long as a replay could take, and then asked anyway. The window is
        // not on a clock here: it has one of its own, and the sweep shuts it
        // before it sends anything behind it.
        let hold_until = *self
            .replay_hold_until
            .get_or_insert_with(|| Instant::now() + COMPLETED_ORDERS_HOLD);
        let the_replay_could_still_be_running =
            !shared.orders.replay_done() && Instant::now() < hold_until;
        // And not ahead of a question asked before it that is still held.
        let behind_an_earlier = self.completed_orders_queued.front().is_some_and(|first| *first != turn);
        if the_replay_could_still_be_running || self.completed_orders_open || behind_an_earlier {
            self.completed_orders_wanted.get_or_insert(hold_until);
            // Held under its own turn, not the open window's: the window that
            // is open belongs to the question before this one, and its end is
            // that question's answer.
            if !self.completed_orders_queued.contains(&turn) {
                self.completed_orders_queued.push_back(turn);
            }
            log::debug!(
                "holding the question of what the venue has finished until the one before it \
                 is answered",
            );
            return;
        }
        // Past the hold, so this question is no longer waiting.
        self.completed_orders_queued.retain(|held| *held != turn);
        if self.completed_orders_queued.is_empty() {
            self.completed_orders_wanted = None;
        }
        // Whose question this is, carried through to the end that answers it.
        self.completed_orders_asked_on = turn;
        let Some(conn) = ccp_conn.as_mut() else {
            self.end_completed_orders(turn, shared);
            return;
        };
        let ts = chrono_free_timestamp();
        // The day the venue keeps, stated as it states every other time. Its
        // own cutoff decides what falls inside, so the window runs from the
        // start of yesterday to the start of tomorrow: that covers the day
        // whichever side of the cutoff this session is on.
        let from = crate::protocol::datetime::midnight_days_away(-1);
        let to = crate::protocol::datetime::midnight_days_away(1);
        let (from, to) = (from.to_string(), to.to_string());
        let sent = conn.send_fix(&[
            (fix::TAG_MSG_TYPE, "H"),
            (fix::TAG_SENDING_TIME, &ts),
            (11, "*"), (55, "*"), (54, "*"),
            (6533, "1"), (6536, &from), (6537, &to),
        ]);
        match sent {
            Ok(()) => {
                hb.last_ccp_sent = Instant::now();
                // A new question, so a new answer: this one takes its own
                // orders up to its own bound. The half-built records the answer
                // before it left stay, because a report states what changed and
                // leaves the rest out and those records are what a later report
                // about the same order is read against — but they are not this
                // answer's orders, so they are not counted against its bound
                // and are given up, oldest first, if it needs the room.
                self.orders_in_this_answer.clear();
                self.the_answer_is_full = false;
                // Open until the wire shuts it: the sentinel, or the end of
                // the connection. A clock here would hand the rest of the
                // answer to the next question, and to the live path.
                self.completed_orders_open = true;
                log::info!("Asked the venue for what it has finished, {from} to {to}");
            }
            Err(e) => {
                log::warn!("the request for finished orders could not be sent: {e}");
                self.end_completed_orders(turn, shared);
            }
        }
    }

    /// Give the trading connection up, and the socket it was carried on with
    /// it.
    ///
    /// The connection goes here rather than at the reconnect that replaces it.
    /// A liveness timeout says the venue has stopped answering, not that the
    /// socket is closed, and this account permits one session at a time: kept
    /// through the outage, the reconnect competes with a session this client
    /// is still holding open. On a hard error it is a descriptor held for as
    /// long as the outage lasts.
    ///
    /// Every lookup waiting on the connection is failed here, as the
    /// historical connection fails its own: the connection that replaces this
    /// one is asked nothing this one was asked.
    pub(crate) fn handle_disconnect(
        &mut self,
        ccp_conn: &mut Option<Connection>,
        context: &mut Context,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
    ) {
        self.disconnected = true;
        *ccp_conn = None;
        self.recovery_sweep_at = None;
        // The wait for the replay belongs to the connection that replays. Left
        // armed, the next connection inherited a hold that had already expired
        // and asked what the venue has finished while its own replay was still
        // arriving, which reads the replay as history.
        self.replay_hold_until = None;
        // A question about what the venue has finished dies with the
        // connection that carried it. The answer ends with a sentinel and no
        // sentinel is coming, so the caller was left waiting out its whole
        // deadline — and the window stayed open across the reconnect, where
        // the replay that follows is read as history rather than recovered.
        // The queries that went out on this connection will not be answered on
        // the next one, and an entry nobody will answer holds its contract.
        self.pending_dividends.clear();
        self.rates_asked.clear();
        self.schedules_asked.clear();
        self.definitions_asked.clear();
        // The names one connection's recovery taught this session mean nothing
        // on the next one, which recovers the account again and says them
        // afresh. Kept, they grew for the life of the engine and went on
        // redirecting reports to orders long finished.
        self.wire_name_to_order.clear();
        self.wire_names_learned.clear();
        // The question on the wire is over with the connection that carried
        // it, and answered with what the venue had stated: nothing after this
        // can be told apart from the next connection's replay. The questions
        // held behind it are not asked of a connection that has gone; each is
        // sent in its turn, once the next connection's replay is over.
        if self.completed_orders_open {
            self.completed_orders_open = false;
            self.deliver_finished_orders(shared);
            self.end_completed_orders(self.completed_orders_asked_on, shared);
        }
        // The next connection has a replay of its own to hold behind.
        self.replay_hold_until = None;
        // The engine stops believing these statuses here, and said so to
        // nobody — so the API layer went on reporting the pre-disconnect
        // status and `req_open_orders` kept asserting it.
        context.mark_orders_uncertain();
        // And what the account holds, for the same reason and at the same
        // moment. Marked only when the next connection arrived, the flag said
        // the download had finished for the whole of a backoff — or for ever,
        // where the retries ran out — so a caller asking what the account
        // holds was answered at once from the pre-drop book with nothing to
        // say the venue had not been heard from since.
        for (_, portfolio) in shared.account_portfolios() { portfolio.account_download_is_pending(); }
        for order in context.uncertain_orders() {
            let update = executions::uncertain_update(&order, shared.orders.get_order_info(order.order_id));
            shared.orders.push_order_update(update);
            emit(event_tx, Event::OrderUpdate(update));
        }
        self.fail_pending_lookups(shared, event_tx);
        // Don't emit Event::Disconnected — auto-reconnect handles CCP drops
        // transparently.
        // Python is only notified if reconnect exhausts retries.
    }

    /// Report every lookup still waiting on the connection as failed, and
    /// forget it.
    ///
    /// The connection that replaces this one is asked nothing this one was
    /// asked, so a lookup outstanding at the drop could only run its deadline
    /// out — and it was then reported as a request the venue never answered,
    /// or a contract the venue does not know, ten to twenty seconds after the
    /// connection went. The historical connection has failed its own at once
    /// all along.
    fn fail_pending_lookups(&mut self, shared: &SharedState, event_tx: &Option<EventSink>) {
        const WHY: &str = "the trading connection went away before the venue answered";
        // A caller's details request is failed and ended, as one that ran its
        // deadline out is. A fetch of the engine's own is forgotten, so the
        // next report naming that contract asks again.
        let mut ended: Vec<u32> = Vec::new();
        let mut lost_auto_fetch: Vec<u32> = Vec::new();
        for (req_id, _, _) in self.pending_secdef.drain(..) {
            if req_id < crate::bridge::ENGINE_ID_BASE { ended.push(req_id) } else { lost_auto_fetch.push(req_id) }
        }
        self.auto_fetched_conids.retain(|_, rid| !lost_auto_fetch.contains(rid));
        ended.extend(self.pending_fanout.drain(..).map(|p| p.api_req_id));
        // A continuous future's lookup is answered once every lookup it makes
        // is in, and the connection that would make the rest has gone: it is
        // failed whole, rather than handed over in part.
        for (req_id, _) in self.continuous_lookups.drain() {
            self.pending_schedule_pair.retain(|p| p.api_req_id != req_id);
            if !ended.contains(&req_id) {
                ended.push(req_id);
            }
        }
        // A contract the venue did name, whose trading hours it now will not
        // state: delivered without them, the way the pairing's own deadline
        // delivers it.
        for p in &mut self.pending_schedule_pair { p.deadline = Instant::now(); }
        self.sweep_pending_schedule_pairs(&mut None, shared, event_tx, &mut HeartbeatState::new());
        self.details_delivered.clear();
        // What the advisor was asked, which only the connection that was asked
        // can answer: the one that replaces it is asked nothing this one was.
        // Left standing, a caller reading a partition of the configuration
        // waited for ever, and one replacing a partition never learned whether
        // the venue had taken its document — no end, and no refusal either.
        for (_, asked) in self.pending_advisor.drain() {
            shared.reference.push_advisor_refused(
                asked.origin(), ADVISOR_SAVE_REFUSED, WHY.to_string(),
            );
        }
        // A scan parked behind the naming of its rows is released with the
        // rows it has, the way its own deadline releases it: the lookups it
        // waited on went with the connection.
        for pe in &mut self.pending_scanner_enrichment { pe.deadline = Instant::now(); }
        self.sweep_scanner_enrichments(shared);
        // A search given up on is over with the connection that carried it,
        // and the ones held behind it are refused with it: nothing is left to
        // send them on.
        self.matching_symbols_abandoned = None;
        let mut refused: Vec<u32> = self.pending_matching_symbols.drain(..).map(|(rid, _)| rid).collect();
        refused.extend(self.queued_matching_symbols.drain(..).map(|(rid, _)| rid));
        refused.extend(self.pending_option_params.drain(..).map(|(rid, ..)| rid));
        refused.extend(self.queued_option_params.drain(..).map(|q| q.req_id));
        // An order waiting on a lookup is refused with the connection it was
        // asked on.
        let orders: Vec<u32> = self.order_naming.drain(..).map(|(rid, _)| rid).collect();
        self.orders_named.extend(orders.into_iter().map(|rid| {
            (rid, OrderNamed::Refused(crate::error_codes::Refusal::NOT_CONNECTED, WHY.to_string()))
        }));
        let named: Vec<(u32, bool)> = self.pending_named.drain(..)
            .filter_map(|(_, cmd, _)| request_id(&cmd).map(|rid| {
                (rid, matches!(cmd, crate::types::ControlCommand::FetchHistorical { .. }))
            }))
            .collect();
        if ended.is_empty() && refused.is_empty() && named.is_empty() {
            return;
        }
        log::warn!(
            "{} lookup(s) were still unanswered when the trading connection went",
            ended.len() + refused.len() + named.len(),
        );
        let code = crate::error_codes::Refusal::NOT_CONNECTED;
        for req_id in ended {
            self.fail_lookup(req_id, code, WHY.to_string(), shared, event_tx);
        }
        for req_id in refused {
            shared.reference.push_historical_error(req_id, code, WHY.to_string());
        }
        for (req_id, bars) in named {
            super::push_hmds_refusal(shared, req_id, code, WHY.to_string(), bars);
        }
    }

    /// Bring both of the completed-order deadlines forward to now.
    ///
    /// Only a test calls these: the two waits are measured against a clock,
    /// and a test that slept them out would be a test that takes the wait.
    #[cfg(test)]
    pub(crate) fn give_up_waiting_for_the_replay(&mut self) {
        self.completed_orders_wanted = Some(Instant::now());
        self.replay_hold_until = Some(Instant::now());
    }

    /// And for every dividend query outstanding.
    ///
    /// Takes the query as already sent, because a test has no socket to send
    /// it down and what is under test is what becomes of the answer.
    #[cfg(test)]
    pub(crate) fn give_up_on_a_dividend_query(&mut self, query_id: &str, con_id: u32) {
        self.pending_dividends.push((query_id.to_string(), con_id, Instant::now()));
    }

    /// Send a held question about what the venue has finished, if the
    /// session's own replay is over by now.
    pub(crate) fn sweep_completed_orders_request(
        &mut self,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        shared: &SharedState,
        left: &mut usize,
    ) {
        // A question the venue never answered. Kept, it held its contract for
        // the life of the session and stayed on this list for ever; given up
        // on, the next option written on that underlying asks again.
        let mut given_up_on = Vec::new();
        self.pending_dividends.retain(|(query_id, con_id, until)| {
            let waiting = Instant::now() < *until;
            if !waiting {
                log::debug!("what {con_id} pays out went unanswered under {query_id}");
                given_up_on.push((query_id.clone(), *con_id));
            }
            waiting
        });
        for entry in given_up_on {
            self.dividends_given_up_on.push_back(entry);
            while self.dividends_given_up_on.len() > GIVEN_UP_ON_DIVIDEND_QUERIES {
                self.dividends_given_up_on.pop_front();
            }
        }

        // No clock ends a question the venue is answering: the answer names
        // no question, so what arrives after a deadline would be read as the
        // next question's. A question held behind it waits for the sentinel,
        // or for the end of the connection.
        let Some(waited_since) = self.completed_orders_wanted else { return };
        if self.completed_orders_open {
            return;
        }
        // The replay of an account with nothing working ends without naming an
        // order, and it is naming one that says the replay has begun — so a
        // question held for it would wait for ever on such an account. Held
        // for as long as the replay could take and then asked anyway.
        if !shared.orders.replay_done() && Instant::now() < waited_since {
            return;
        }
        // Left set, so the guard inside sees the wait it has already run out —
        // clearing it first makes that guard read "nothing has waited yet" and
        // hold the question all over again. It is cleared there, past the hold.
        // On the turn the question that was held was asked on, which is not
        // the open window's: this is that question going out at last.
        let Some(asked_on) = self.completed_orders_queued.front().copied() else {
            self.completed_orders_wanted = None;
            return;
        };
        if *left > 0 {
            *left -= 1;
            self.send_completed_orders_request(asked_on, ccp_conn, hb, shared);
        }
    }

    /// Report the orders the recovery push did not account for.
    ///
    /// Their status stays Uncertain: the engine watched the connection die
    /// with them working and has been told nothing since, so it does not know
    /// whether they filled, were pulled, or are still resting. What it does
    /// know — and what it had no way to say before — is that the recovery is
    /// over and they were not in it. A caller waiting on the reconciliation
    /// that `Uncertain` promises was otherwise waiting on nothing.
    pub(crate) fn sweep_recovery(
        &mut self,
        context: &mut Context,
        shared: &SharedState,
        event_tx: &Option<EventSink>,
    ) {
        match self.recovery_sweep_at {
            Some(at) if Instant::now() >= at => self.recovery_sweep_at = None,
            _ => return,
        }
        let stranded = context.uncertain_orders();
        if stranded.is_empty() {
            log::info!("Recovery complete — every order the drop left working is accounted for");
            return;
        }
        log::error!(
            "Recovery complete — {} order(s) it did not account for: {:?}. Their state is not known; \
             reconcile from executions before acting on them.",
            stranded.len(),
            stranded.iter().map(|o| o.order_id).collect::<Vec<_>>(),
        );
        for order in stranded {
            let update = executions::uncertain_update(&order, shared.orders.get_order_info(order.order_id));
            shared.orders.push_order_update(update);
            emit(event_tx, Event::OrderUpdate(update));
        }
    }

    pub(crate) fn reconnect(
        &mut self,
        conn: Connection,
        ccp_conn: &mut Option<Connection>,
        hb: &mut HeartbeatState,
        account_id: &str,
        shared: &SharedState,
    ) {
        shared.set_session_account(account_id);
        self.account_requests.clear();
        *ccp_conn = Some(conn);
        self.disconnected = false;
        // This connection has not yet named what it has working, and neither
        // "none" nor the last connection's answer is that. Both of these said
        // otherwise from the connection before it, so a caller asking what it
        // had on was answered at once from the pre-drop book — every order in
        // it Uncertain — while the venue's account was still on its way, which
        // is how the same order is placed twice. Cleared here, that caller
        // waits for the new push the way it waited for the first.
        self.hydrated_any = false;
        shared.orders.replay_is_pending();
        // And the account itself. The same flag, for the same reason: a
        // caller asking what the account holds was answered from the pre-drop
        // snapshot the moment the connection came back, before the venue had
        // restated a single holding.
        for (_, portfolio) in shared.account_portfolios() { portfolio.account_download_is_pending(); }
        self.recovery_sweep_at = Some(Instant::now() + RECOVERY_PUSH_GRACE);
        hb.last_ccp_sent = Instant::now();
        hb.last_ccp_recv = Instant::now();
        hb.pending_ccp_test = None;

        if let Some(conn) = ccp_conn.as_mut() {
            let ts = chrono_free_timestamp();

            // Re-subscribe to account/position data so server pushes fresh UP/UT/UM
            // messages.
            let _ = conn.send_fix(&[
                (fix::TAG_MSG_TYPE, "U"), (fix::TAG_SENDING_TIME, &ts),
                (6040, "91"), (1, account_id), (6556, "DR.1"), (6712, "1"),
            ]);
            // Drawn from the counter, not named here. The venue answers a key
            // it is already serving with nothing, and the refreshes on this
            // connection have been spending keys from that counter since the
            // session opened — so a fixed one is a key that has very likely
            // already been used, and the account and position pushes this asks
            // for simply do not resume. Recorded too, so the unsubscribe that
            // follows closes the key this connection is actually served under.
            // The download a rebuilt connection carries arrives under the key
            // its own opening asked with, which is the one a first logon uses
            // — the handshake sends it before this loop is handed the
            // connection. Recorded before the counter is drawn, so the end
            // that squares the account is the end of the request carrying it;
            // the counter's key below is a second subscribe on the same
            // account, and first-wins leaves it measuring nothing.
            shared.name_account_request(OPENING_ACCOUNT_REQUEST, account_id);
            shared.portfolio.holdings_restated_under(OPENING_ACCOUNT_REQUEST);
            let key = self.next_account_request_key();
            self.account_requests.push((key.clone(), account_id.to_string()));
            shared.name_account_request(&key, account_id);
            let _ = conn.send_fix(&[
                (fix::TAG_MSG_TYPE, "U"), (fix::TAG_SENDING_TIME, &ts),
                (6040, "6"), (6036, "1"), (6095, account_id), (6529, &key),
            ]);

            // Every profit-and-loss subscription standing, asked for again on
            // this connection: the venue serves one on the connection that
            // asked, and the marks a caller reads otherwise stop moving with
            // nothing said.
            for (_, _, account) in &self.pnl_subscriptions {
                let pnl_key = Self::next_pnl_key();
                shared.name_account_request(&pnl_key, account);
                let _ = conn.send_fix(&[
                    (fix::TAG_MSG_TYPE, "U"), (fix::TAG_SENDING_TIME, &ts),
                    (6040, "142"), (6529, &pnl_key), (1, account),
                ]);
            }

            // Resting open orders are pushed unsolicited by CCP as 35=8 with
            // 150=0/39=0 carrying originating clientId (6119) and orderId (6121),
            // terminated by 11='*' sentinel.
            hb.last_ccp_sent = Instant::now();
            log::info!(
                "CCP reconnected, sent account/position re-subscribe and {} P&L renewal(s)",
                self.pnl_subscriptions.len(),
            );
        }

        for (account, portfolio) in shared.account_portfolios() {
            if !std::sync::Arc::ptr_eq(&portfolio, &shared.portfolio) {
                self.send_account_refresh(&account, ccp_conn, hb, shared);
            }
        }

    }
}

/// Handle account update messages (cross-cutting, called from CCP message processing).
/// Account figures describing holdings the account does not hold itself.
///
/// Stated the same way as the account's own — a name and a value — and read
/// the same way. What differs is only which set of holdings they describe, and
/// mixing them into the account's own would overstate what it is worth.
fn handle_account_update_elsewhere(
    msg: &[u8],
    shared: &SharedState,
    held: crate::types::HeldElsewhere,
) {
    let Ok(text) = std::str::from_utf8(msg) else { return };
    // The name opens a group; its currency and its value follow inside it,
    // and the group is filed when the next name opens or the frame ends. On
    // the account's own figures the currency comes before the value on every
    // group; read as value-then-currency, the currency closed the group before
    // its value arrived and every figure was dropped. A figure stated in two
    // currencies is two figures.
    let mut name: Option<&str> = None;
    let mut value: Option<&str> = None;
    let mut currency: &str = "";
    let mut stated = 0usize;
    let mut file = |name: &mut Option<&str>, value: &mut Option<&str>, currency: &mut &str| {
        if let (Some(n), Some(v)) = (name.take(), value.take()) {
            shared.portfolio.set_value_elsewhere(held, n.to_string(), v.to_string(), currency.to_string());
            stated += 1;
        }
        *currency = "";
    };
    for part in text.split('\x01') {
        if let Some(v) = part.strip_prefix("8001=") {
            file(&mut name, &mut value, &mut currency);
            name = Some(v);
        } else if let Some(v) = part.strip_prefix("8004=") {
            value = Some(v);
        } else if let Some(c) = part.strip_prefix("15=") {
            currency = c;
        }
    }
    file(&mut name, &mut value, &mut currency);
    if stated > 0 {
        log::info!("{stated} account figures for holdings {held:?}");
    }
}

/// Handle 6040=152, the venue's price table: 146={count} with a list of
/// contract ids in 6008 paired positionally with a list of prices in 8057.
/// The price is stored as text and read where it is used, so one that does not
/// parse costs its own contract and not the table.
fn handle_pnl_prices(msg: &[u8], shared: &SharedState) {
    let text = match std::str::from_utf8(msg) {
        Ok(t) => t,
        Err(_) => return,
    };
    // Both lists are collected in wire order and paired by position, so an
    // unreadable contract id holds its place instead of shifting every price
    // after it onto the wrong contract.
    let mut con_ids: Vec<Option<i64>> = Vec::new();
    let mut prices: Vec<&str> = Vec::new();
    for part in text.split('\x01') {
        if let Some(v) = part.strip_prefix("6008=") {
            con_ids.push(v.parse::<i64>().ok().filter(|&id| id != 0));
        } else if let Some(v) = part.strip_prefix("8057=") {
            prices.push(v);
        }
    }
    let table = con_ids.into_iter().zip(prices)
        .filter_map(|(con_id, price)| Some((con_id?, price.to_string())))
        .collect();
    if let Some(portfolio) = shared.portfolio_for_message(msg) { portfolio.set_venue_prices(table); }
}

impl CcpState {

    /// Issue an internal secdef request for `con_id` where the reference cache is cold
    /// and
    /// none has been auto-fetched this session. The reply path populates the cache
    /// through
    /// the existing 35=d handler; the response is not tracked.
    fn auto_fetch_secdef_if_cold(
        &mut self,
        con_id: i64,
        ccp_conn: &mut Option<Connection>,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        if con_id == 0 { return; }
        if self.auto_fetched_conids.contains_key(&con_id) { return; }
        // Warm means defined. An entry a fill or an order seeded names the
        // contract and no more, and read as warm it kept the definition from
        // ever being asked for.
        if shared.reference.has_definition(con_id) { return; }
        let req_id = self.next_internal_secdef_id;
        self.next_internal_secdef_id = self.next_internal_secdef_id.wrapping_add(1);
        self.auto_fetched_conids.insert(con_id, req_id);
        self.send_secdef_request(req_id, con_id, "", ccp_conn, hb, shared, &None);
    }

    /// Park a scanner result and dispatch concurrent secdef requests for every cache-
    /// miss
    /// con_id. Once all replies arrive (via `try_release_scanner_enrichments`) the
    /// result
    /// is pushed to the dispatch queue with the now-warm cache. Mirrors what the
    /// gateway
    /// does internally for binary-API scanner clients.
    pub(crate) fn start_scanner_enrichment(
        &mut self,
        api_req_id: u32,
        result: crate::control::scanner::ScannerResult,
        ccp_conn: &mut Option<Connection>,
        shared: &SharedState,
        hb: &mut HeartbeatState,
    ) {
        let mut awaiting: HashSet<i64> = HashSet::new();
        for entry in &result.entries {
            let con_id = entry.con_id as i64;
            if con_id == 0 { continue; }
            if shared.reference.get_contract(con_id).is_some() { continue; }
            awaiting.insert(con_id);
        }
        // A batch needing nothing still waits behind an earlier batch of its
        // scan that does: handed over at once, it overtook the older list and
        // the caller ended on that one.
        if awaiting.is_empty()
            && !self.pending_scanner_enrichment.iter().any(|p| p.api_req_id == api_req_id)
        {
            shared.reference.push_scanner_data(api_req_id, result);
            return;
        }
        // Issue one secdef request per cold con_id. If another flow has already
        // requested the same con_id — auto_fetched_conids holds it — the send
        // is skipped and the wait stands: that reply populates the cache and
        // release this entry via try_release_scanner_enrichments.
        for &con_id in &awaiting {
            if !self.auto_fetched_conids.contains_key(&con_id) {
                let req_id = self.next_internal_secdef_id;
                self.next_internal_secdef_id = self.next_internal_secdef_id.wrapping_add(1);
                self.auto_fetched_conids.insert(con_id, req_id);
                self.send_secdef_request(req_id, con_id, "", ccp_conn, hb, shared, &None);
            }
        }
        self.pending_scanner_enrichment.push(PendingScannerEnrichment {
            api_req_id,
            result,
            awaiting,
            deadline: Instant::now() + Duration::from_secs(5),
        });
    }

    /// Called from the 35=d reply path after the contract cache has been
    /// populated for `con_id`. Removes `con_id` from any pending scanner
    /// enrichment's awaiting set; entries whose set becomes empty are handed
    /// over, each once every earlier batch of its scan has been.
    pub(crate) fn try_release_scanner_enrichments(&mut self, con_id: i64, shared: &SharedState) {
        if self.pending_scanner_enrichment.is_empty() { return; }
        for pe in &mut self.pending_scanner_enrichment {
            pe.awaiting.remove(&con_id);
        }
        self.release_scanner_batches(shared);
    }

    /// Hand over every batch that waits on nothing and has no earlier batch of
    /// its scan still waiting, in the order the batches arrived.
    fn release_scanner_batches(&mut self, shared: &SharedState) {
        let mut idx = 0;
        while idx < self.pending_scanner_enrichment.len() {
            let pe = &self.pending_scanner_enrichment[idx];
            let behind_its_own = self.pending_scanner_enrichment[..idx]
                .iter()
                .any(|earlier| earlier.api_req_id == pe.api_req_id);
            if pe.awaiting.is_empty() && !behind_its_own {
                let pe = self.pending_scanner_enrichment.remove(idx);
                shared.reference.push_scanner_data(pe.api_req_id, pe.result);
            } else {
                idx += 1;
            }
        }
    }

    /// Flush scanner enrichments past their deadline, dispatching whatever
    /// entries are held, blank fields included where the secdef reply never
    /// arrived. Prevents an indefinite hang on a missing reply.
    pub(crate) fn sweep_scanner_enrichments(&mut self, shared: &SharedState) {
        if self.pending_scanner_enrichment.is_empty() { return; }
        let now = Instant::now();
        let late = self.pending_scanner_enrichment.iter_mut()
            .filter(|pe| pe.deadline <= now && !pe.awaiting.is_empty());
        for pe in late {
            log::warn!(
                "scanner enrichment timeout: req_id={} missing={} con_ids; dispatching partial",
                pe.api_req_id,
                pe.awaiting.len(),
            );
            pe.awaiting.clear();
        }
        self.release_scanner_batches(shared);
    }
}

/// Fill in a holding's contract once its definition arrives.
///
/// The position feed states a contract id, a quantity and often a cost, and
/// little else — so a holding was reported unnamed, or named and priced by
/// the unit, until some richer message happened to arrive first. The
/// definition is already being fetched for exactly this reason; this is what
/// puts what it carries on the row.
fn identify_position(shared: &SharedState, def: &crate::control::contracts::ContractDefinition) {
    let con_id = def.con_id as i64;
    for (_, portfolio) in shared.account_portfolios() {
    let Some(existing) = portfolio.position_info(con_id) else { continue };
    let sec_type = def.sec_type.to_api_str();
    let multiplier = if def.multiplier != 1.0 { format!("{}", def.multiplier) } else { String::new() };
    // Only where the definition still fills something in. The write leaves
    // every field the row already carries standing, so the question is whether
    // any of them is missing. Asked of the symbol alone, a holding the
    // position feed had already named never took the multiplier — which the
    // definition is the only carrier of on that path — and a contract without
    // it is valued a unit at a time.
    let fills_a_gap = |on_row: &str, stated: &str| on_row.is_empty() && !stated.is_empty();
    if !(fills_a_gap(&existing.symbol, &def.symbol)
        || fills_a_gap(&existing.sec_type, sec_type)
        || fills_a_gap(&existing.currency, &def.currency)
        || fills_a_gap(&existing.multiplier, &multiplier))
    {
        continue;
    }
    portfolio.set_position_info(PositionInfo {
        con_id,
        position: existing.position,
        avg_cost: existing.avg_cost,
        symbol: def.symbol.clone(),
        sec_type: sec_type.to_string(),
        currency: def.currency.clone(),
        multiplier,
        ..Default::default()
    });
    }
}

/// Take the server's position as the engine's own.
///
/// The callback side reads `context.position`, and a snapshot that reached
/// only the portfolio left it deciding from a number the account had not held
/// since before the connection — flat, on a process that restarted holding
/// stock. The server is the authority here, so the difference is adopted
/// rather than accumulated.
fn adopt_position(context: &mut Context, instrument: InstrumentId, position: f64) {
    let delta = position - context.position(instrument);
    if delta != 0.0 {
        context.update_position(instrument, delta);
    }
}

#[cfg(test)]
mod venue_clock_tests {
    use super::*;

    fn local_seconds() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// The venue pushes its own clock, and that is what a caller asking for it
    /// is answered from.
    ///
    /// This is one of the two things the venue ever says about its clock, and
    /// the only one that arrives after the logon. Unread, a session running
    /// past a correction went on answering from the difference the logon
    /// stated, however long ago that was.
    #[test]
    fn a_pushed_clock_is_what_a_caller_is_answered_from() {
        let mut ccp = CcpState::new();
        let mut context = Context::new();
        let shared = SharedState::new();

        // A day ahead of this machine, so this machine's own clock could not
        // be mistaken for the answer.
        let stated = local_seconds() + 86_400;
        let pushed = fix::fix_build(&[
            (fix::TAG_MSG_TYPE, "U"), (6040, "18"), (6114, &stated.to_string()),
        ], 1);
        ccp.process_ccp_message(
            &pushed, &mut None, &mut context, &shared, &None,
            &mut HeartbeatState::new(), "DU1",
        );

        assert!(
            (shared.market.venue_time_millis() - stated * 1_000).abs() < 2_000,
            "the clock the venue pushed, not the one this machine keeps",
        );
    }

    /// A stamp on an ordinary message says nothing about the venue's clock.
    ///
    /// The venue states its clock twice — on the logon, and in the message
    /// above — and every other message merely carries the time it was sent.
    /// Learned from those as well, a caller's answer moved with whatever
    /// traffic happened to arrive, and stopped moving when it stopped.
    #[test]
    fn an_ordinary_message_does_not_move_the_clock() {
        let mut ccp = CcpState::new();
        let mut context = Context::new();
        let shared = SharedState::new();

        // What the venue has stated: this machine's clock, exactly.
        shared.market.note_venue_millis(local_seconds() * 1_000);
        let stamped = fix::fix_build(&[
            (fix::TAG_MSG_TYPE, fix::MSG_HEARTBEAT),
            (fix::TAG_SENDING_TIME, "20260815-12:00:00"),
        ], 1);
        ccp.process_ccp_message(
            &stamped, &mut None, &mut context, &shared, &None,
            &mut HeartbeatState::new(), "DU1",
        );

        assert!(
            (shared.market.venue_time_millis() - local_seconds() * 1_000).abs() < 2_000,
            "the difference the venue stated still stands",
        );
    }
}

pub(crate) mod executions;
pub(crate) mod positions;

#[cfg(test)]
mod tests;
