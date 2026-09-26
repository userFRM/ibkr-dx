//! Questions the engine answers from what the session holds.
//!
//! The account's holdings and figures are stated by the venue as a session
//! opens, and what the account is working is named by it after the connect
//! returns. A question about either asked before then is held here, in the
//! loop's own laps, and answered where the statement completes — or where its
//! bound passes, said so ahead of the answer. Nothing waits on a caller's
//! thread, and a question's cancel withdraws one still held.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bridge::{Answer, Record, SharedState};
use crate::error_codes::Refusal;
use crate::types::model::{ErrorOrigin, Question};
use crate::types::{Ask, Retirement};

/// How long a question waits for the account to state itself before it is
/// answered with what this session holds: the ten seconds the call used to
/// wait.
const DOWNLOAD_WAIT: Duration = Duration::from_secs(10);

/// How long an answer about the holdings waits, once the account has stated
/// them, for their contracts to be named: a holding arrives as an id and a
/// quantity, and its definition is fetched separately.
const NAMING_WAIT: Duration = Duration::from_secs(2);

/// Said ahead of an answer from a book the account had not finished stating.
const NOT_WHOLE_HOLDINGS: &str = "the account had not finished stating its holdings within the \
    wait, so what follows is what this session already held rather than what the account holds";

/// The same, for the account's figures.
const NOT_WHOLE_FIGURES: &str = "the account had not finished stating its figures within the \
    wait, so what follows is what this session already held rather than what the account holds";

/// One question held until what it is answered from has been stated.
struct Held {
    ask: Ask,
    asked_at: Instant,
    /// When the account finished stating itself for this question, or its
    /// wait for that ran out, and whether it had finished.
    settled: Option<(Instant, bool)>,
    /// Already counted as an admission in this lap.
    new: bool,
}

/// The questions held, in the order they were asked.
#[derive(Default)]
pub(crate) struct Asks {
    held: Vec<Held>,
}

impl Asks {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// How many are held.
    pub(crate) fn len(&self) -> usize {
        self.held.len()
    }

    /// Hold a question until what it is answered from has been stated.
    pub(crate) fn take(&mut self, ask: Ask) {
        self.held.push(Held { ask, asked_at: Instant::now(), settled: None, new: true });
    }

    /// Withdraw what a cancel names that is still held, and confirm the
    /// withdrawal where it stands: after every record of the exchange before
    /// it, so nothing of that exchange follows it.
    pub(crate) fn retire(&mut self, what: Retirement, shared: &SharedState) {
        self.held.retain(|held| !held.withdrawn_by(what));
        shared.push_call_record(Record::Retired(what));
    }

    /// Forget every question held, answering none: what a stop does with a
    /// request that is not an order's. A gateway says nothing to a client whose
    /// socket has closed.
    pub(crate) fn withdraw_all(&mut self) {
        self.held.clear();
    }

    /// Answer each question whose statement is in, in the order they were
    /// asked; refuse each the session ended under.
    pub(crate) fn answer_what_is_ready(&mut self, shared: &Arc<SharedState>, left: &mut usize) {
        for held in &mut self.held {
            held.new = false;
        }
        self.answer(shared, left, false);
    }

    /// Finish the questions just admitted, before the next command is taken.
    /// Their admission already used this lap's turn.
    pub(crate) fn answer_new(&mut self, shared: &Arc<SharedState>) {
        self.answer(shared, &mut { super::COMMANDS_PER_LAP }, true);
    }

    fn answer(&mut self, shared: &Arc<SharedState>, left: &mut usize, only_new: bool) {
        if self.held.is_empty() {
            return;
        }
        let now = Instant::now();
        let over = shared.reference.session_over().is_some();
        let replay = shared.orders.replay_settled();
        let mut still = Vec::with_capacity(self.held.len());
        for mut held in std::mem::take(&mut self.held) {
            if *left == 0 || (only_new && !held.new) {
                still.push(held);
                continue;
            }
            let account = match &held.ask {
                Ask::PositionsMulti { account, .. }
                | Ask::AccountUpdatesMulti { account, .. }
                | Ask::AccountUpdates { account } => account.as_str(),
                _ => "",
            };
            let downloaded = shared.portfolio_for(account).account_download_complete();
            // Ended under it: refused, as a call made then is refused. Not the
            // next valid id: a connect asks it on no caller's behalf, a gateway
            // answers no connect with an error, and the session's end is
            // already said where the connection is lost.
            if over {
                if !matches!(held.ask, Ask::NextValidId) {
                    let why = Refusal::not_connected("Not connected");
                    shared.push_refused(held.origin(), i64::from(why.code), why.message);
                }
                *left -= 1;
                continue;
            }
            let answered = match &held.ask {
                Ask::Positions
                | Ask::PositionsMulti { .. }
                | Ask::AccountUpdates { .. }
                | Ask::AccountUpdatesMulti { .. } => {
                    if held.settled.is_none()
                        && (downloaded || now >= held.asked_at + DOWNLOAD_WAIT)
                    {
                        held.settled = Some((now, downloaded));
                    }
                    match held.settled {
                        None => false,
                        // The holdings' contracts named, as the answer names
                        // them, for a moment after the account has stated them.
                        Some((at, _))
                            if matches!(held.ask, Ask::Positions)
                                && now < at + NAMING_WAIT
                                && holdings_unnamed(shared) =>
                        {
                            false
                        }
                        Some((_, whole)) => {
                            held.answer(whole, shared);
                            true
                        }
                    }
                }
                // Held until the venue has named what is working to its end,
                // however long that takes, as a gateway holds it.
                Ask::OpenOrders(_) => {
                    let named = shared.orders.replay_done();
                    if named {
                        held.answer(named, shared);
                    }
                    named
                }
                Ask::NextValidId => match replay {
                    None => false,
                    Some(named) => {
                        held.answer(named || !shared.orders.naming_began(), shared);
                        true
                    }
                },
            };
            if answered {
                *left -= 1;
            } else {
                still.push(held);
            }
        }
        self.held = still;
    }
}

impl Held {
    /// Whether this cancel withdraws this question.
    fn withdrawn_by(&self, what: Retirement) -> bool {
        match (&self.ask, what) {
            (Ask::Positions, Retirement::Question(Question::Positions))
            | (Ask::AccountUpdates { .. }, Retirement::Question(Question::AccountUpdates)) => true,
            (Ask::PositionsMulti { req_id, .. }, Retirement::PositionsMulti(id))
            | (Ask::AccountUpdatesMulti { req_id, .. }, Retirement::AccountUpdatesMulti(id)) => {
                *req_id == id
            }
            _ => false,
        }
    }

    /// What a refusal of this question is about.
    fn origin(&self) -> ErrorOrigin {
        match &self.ask {
            Ask::Positions => ErrorOrigin::Question { q: Question::Positions, ends: true },
            Ask::AccountUpdates { .. } => {
                ErrorOrigin::Question { q: Question::AccountUpdates, ends: true }
            }
            Ask::PositionsMulti { req_id, .. } | Ask::AccountUpdatesMulti { req_id, .. } => {
                ErrorOrigin::Request { id: *req_id, ends: true }
            }
            Ask::OpenOrders(q) => ErrorOrigin::Question { q: *q, ends: true },
            Ask::NextValidId => ErrorOrigin::Session,
        }
    }

    /// Push the answer, and ahead of it what the caller is owed about an
    /// answer from a statement that had not finished.
    fn answer(&self, whole: bool, shared: &SharedState) {
        let notice = |origin: ErrorOrigin, why: &str| {
            log::warn!("{why}");
            shared.push_refused(origin, i64::from(Refusal::NO_ANSWER), why);
        };
        let answer = match &self.ask {
            Ask::Positions => {
                if !whole {
                    notice(
                        ErrorOrigin::Question { q: Question::Positions, ends: false },
                        NOT_WHOLE_HOLDINGS,
                    );
                }
                Answer::Positions
            }
            Ask::PositionsMulti { req_id, account, model_code } => {
                if !whole {
                    notice(ErrorOrigin::Request { id: *req_id, ends: false }, NOT_WHOLE_HOLDINGS);
                }
                Answer::PositionsMulti {
                    req_id: *req_id,
                    account: account.clone(),
                    model_code: model_code.clone(),
                }
            }
            Ask::AccountUpdates { account } => Answer::AccountUpdates { account: account.clone() },
            Ask::AccountUpdatesMulti { req_id, account, model_code, ledger_and_nlv } => {
                if !whole {
                    notice(ErrorOrigin::Request { id: *req_id, ends: false }, NOT_WHOLE_FIGURES);
                }
                Answer::AccountUpdatesMulti {
                    req_id: *req_id,
                    account: account.clone(),
                    model_code: model_code.clone(),
                    ledger_and_nlv: *ledger_and_nlv,
                }
            }
            Ask::OpenOrders(q) => Answer::OpenOrders(*q),
            Ask::NextValidId => {
                // No error travels with an id, so this is said where it can be
                // said. The floor is whatever had been named by then, which is
                // not the whole of what the account is working.
                if !whole {
                    log::warn!(
                        "the venue had not finished naming this account's working orders within \
                         the wait, so the next order id is counted from what it had named and the \
                         venue may refuse an order under it as one it is already working",
                    );
                }
                Answer::NextValidId
            }
        };
        shared.push_call_record(Record::Answer(answer));
    }
}

/// Whether a holding the answer would state names no contract yet.
fn holdings_unnamed(shared: &SharedState) -> bool {
    shared.portfolio.position_infos().iter().any(|pi| {
        pi.position != 0.0
            && pi.symbol.is_empty()
            && shared.reference.get_contract(pi.con_id).is_none()
    })
}
