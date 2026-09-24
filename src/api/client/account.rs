//! Account-related methods: positions, PnL, account summary/updates.

use crate::error_codes::Refusal;
use crate::types::model::{ErrorOrigin, Question};
use crate::types::*;

use super::{Contract, EClient};
use crate::client_core::ClientCore;

impl EClient {
    // ── Positions ──


    /// The contract a holding is in, named as fully as this client can.
    ///
    /// Preferred from the definition cache, which carries the exchange,
    /// local symbol and trading class; the feed's own fields answer while the
    /// cache is cold. Shared with the real-time path so a holding is named
    /// the same way whether it is read at the request or as it moves.
    pub(crate) fn position_contract(&self, pi: &crate::types::PositionInfo) -> Contract {
        self.core.get_contract(pi.con_id, &self.shared).unwrap_or_else(|| Contract {
            con_id: pi.con_id,
            symbol: pi.symbol.clone(),
            sec_type: pi.sec_type.clone(),
            currency: pi.currency.clone(),
            multiplier: pi.multiplier.clone(),
            ..Default::default()
        })
    }

    /// Request positions. Matches `reqPositions` in C++.
    ///
    /// Answered where the account has stated what it holds, which the venue
    /// does as a session opens: every holding and `position_end`, stated as
    /// the account stands where the answer is delivered, and each move after
    /// it on `position`. The engine holds the question until then, and nothing
    /// waits here. An account that says nothing within ten seconds is answered
    /// with what this session holds — which reads the same as an account
    /// holding nothing, so it is said on `error` ahead of the answer.
    pub fn req_positions(&self) {
        // Answered from a session that has ended, this hands back the last
        // book with nothing to say it is stale: the shutdown does not clear
        // the download flag. The other surface refuses, and this does too.
        if self.session_over() {
            return self.refuse_question(Question::Positions, &Refusal::not_connected("Not connected"));
        }
        if let Err(why) = self.send(ControlCommand::Ask(Ask::Positions)) {
            self.refuse_question(Question::Positions, &why);
        }
    }

    // ── PnL ──

    /// Subscribe to the named account's profit. The account is checked as a
    /// gateway checks it. Each request has its own subscription; a repeated
    /// active request number is refused under 102. A model is taken and not
    /// applied, with a log notice once per session.
    pub fn req_pnl(&self, req_id: i64, account: &str, model_code: &str) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        // The account is checked before the request number is subscribed.
        if let Err(why) = ClientCore::check_pnl_account(&self.shared, &self.accounts, &self.account_id, account) {
            log::warn!("{}", why.message);
            return self.report_reason(req_id, &why);
        }
        let account = if account.eq_ignore_ascii_case("All") || account == "AllNonProp" {
            ClientCore::note_account_selection(&self.shared, account);
            self.account_id.as_str()
        } else { account };
        if let Err(why) = self.core.subscribe_pnl(req_id, account) {
            return self.report_reason(req_id, &why);
        }
        ClientCore::note_account_selection(&self.shared, model_code);
        let account = account.to_string();
        if let Err(why) = self.send(ControlCommand::SubscribePnl { req_id, single: false, account }) {
            self.core.unsubscribe_pnl(req_id);
            self.report_reason(req_id, &why);
        }
    }

    /// Cancel PnL subscription. Matches `cancelPnL` in C++.
    ///
    /// The updates stop. The venue has no message withdrawing the subscription
    /// itself, on a gateway as here, so the updates stopping is what the call
    /// does.
    pub fn cancel_pnl(&self, req_id: i64) {
        self.core.unsubscribe_pnl(req_id);
        if let Err(why) = self.send(ControlCommand::CancelPnl { req_id, single: false }) {
            self.report_reason(req_id, &why);
        }
    }

    /// Subscribe to a position's profit in the named account.
    /// The account is checked as for the account-level profit. A model is
    /// taken and not applied, with a log notice once per session.
    pub fn req_pnl_single(&self, req_id: i64, account: &str, model_code: &str, con_id: i64) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        if let Err(why) = ClientCore::check_pnl_account(&self.shared, &self.accounts, &self.account_id, account) {
            log::warn!("{}", why.message);
            return self.report_reason(req_id, &why);
        }
        ClientCore::note_account_selection(&self.shared, model_code);
        let account = if account.eq_ignore_ascii_case("All") || account == "AllNonProp" {
            ClientCore::note_account_selection(&self.shared, account);
            self.account_id.as_str()
        } else { account };
        if let Err(why) = self.core.subscribe_pnl_single(req_id, con_id, account) {
            return self.report_reason(req_id, &why);
        }
        if let Err(why) = self.send(ControlCommand::SubscribePnl { req_id, single: true, account: account.to_string() }) {
            self.core.unsubscribe_pnl_single(req_id);
            self.report_reason(req_id, &why);
        }
    }

    /// Cancel single-position PnL subscription. Matches `cancelPnLSingle` in C++.
    pub fn cancel_pnl_single(&self, req_id: i64) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        self.core.unsubscribe_pnl_single(req_id);
        if let Err(why) = self.send(ControlCommand::CancelPnl { req_id, single: true }) { self.report_reason(req_id, &why); }
    }

    // ── Account Summary ──

    /// Request an account summary. `All` answers for every account the login
    /// holds. Account groups and `AllNonProp` are taken and not applied, with
    /// a log notice once per session. Validation and the limit of two standing
    /// summary requests follow a gateway.
    pub fn req_account_summary(&self, req_id: i64, group: &str, tags: &str) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        let accounts = if group == "All" { self.accounts.clone() } else {
            ClientCore::note_account_selection(&self.shared, group);
            vec![self.account_id.clone()]
        };
        let checked = ClientCore::check_account_summary(&self.shared, group, tags)
            .and_then(|()| self.core.subscribe_account_summary(req_id, tags, accounts.clone()));
        if let Err(why) = checked {
            return self.report_reason(req_id, &why);
        }
        for account in accounts {
            if let Err(why) = self.send(ControlCommand::RefreshAccount { account }) {
                self.report_reason(req_id, &why);
            }
        }
    }

    /// Cancel account summary. Matches `cancelAccountSummary` in C++.
    pub fn cancel_account_summary(&self, req_id: i64) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        self.core.unsubscribe_account_summary(req_id);
    }

    // ── Account Updates ──

    /// Subscribe to the named account's figures and holdings, or withdraw
    /// the subscription. A single-account login ignores the name as a gateway
    /// does. Subscribing asks the venue to restate that account now; the engine
    /// holds the answer until its download ends or the existing wait expires.
    pub fn req_account_updates(&self, subscribe: bool, acct_code: &str) {
        // The session before the account, as for every other request: an
        // ended one is told it has ended, not that it named a wrong account.
        // A subscription is the question of the account's figures; its
        // withdrawal, like `cancel_positions`, is the session's.
        let refused = if subscribe {
            ErrorOrigin::Question { q: Question::AccountUpdates, ends: true }
        } else {
            ErrorOrigin::Session
        };
        let refuse = |why: Refusal| self.refuse(refused, i64::from(why.code), &why.message);
        if self.session_over() { return refuse(Refusal::not_connected("Not connected")); }
        if let Err(why) = ClientCore::check_account_updates(&self.shared, &self.accounts, subscribe, acct_code) {
            return refuse(why);
        }
        let account = if ClientCore::login_holds_several_accounts(&self.shared)
            && !acct_code.eq_ignore_ascii_case("All") && acct_code != "AllNonProp" {
            acct_code.to_string()
        } else {
            if acct_code == "AllNonProp" { ClientCore::note_account_selection(&self.shared, acct_code); }
            self.account_id.clone()
        };
        // A withdrawal is the question's cancel: the engine withdraws a
        // subscription it still holds and confirms the withdrawal in its
        // place, after everything the subscription was answered with, so
        // nothing of the account follows it.
        let command = if subscribe {
            ControlCommand::Ask(Ask::AccountUpdates { account })
        } else {
            ControlCommand::Retire(Retirement::Question(Question::AccountUpdates))
        };
        // Subscribed where the answer stands in the session's order, once the
        // account has stated itself, which is where the first batch and its
        // end are stated from.
        if let Err(why) = self.send(command) {
            refuse(why);
        }
    }

    /// Cancel positions subscription. Matches `cancelPositions` in C++.
    ///
    /// Nothing is withdrawn from the venue: it pushes what the account holds
    /// as the session opens and keeps it current whether or not anyone is
    /// listening. What stops is the reporting — a holding that moves after
    /// this is no longer delivered on `position`. A `req_positions` the engine
    /// still holds is withdrawn, never answered. The cancel is confirmed on
    /// [`question_retired`](crate::api::wrapper::Wrapper::question_retired)
    /// where it stands, after everything the question was answered with.
    pub fn cancel_positions(&self) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        if let Err(why) = self.send(ControlCommand::Retire(Retirement::Question(Question::Positions))) {
            self.refuse_session(&why);
        }
    }

    /// Request managed accounts. Matches `reqManagedAccts` in C++.
    ///
    /// Answered with every account this login holds, comma separated, which is
    /// the shape the reference client answers in. A login with one account is
    /// answered with that one account and no comma.
    pub fn req_managed_accts(&self) {
        if self.session_over() {
            return self.refuse_question(Question::ManagedAccounts, &Refusal::not_connected("Not connected"));
        }
        self.reply(crate::bridge::Reply::ManagedAccounts(self.accounts.join(",")));
    }

    /// Subscribe to the named account's figures under this request number.
    /// `ledger_and_nlv` selects the per-currency ledger and net liquidation.
    /// A model is taken and not applied, with a log notice once per session.
    /// The initial batch ends with `account_update_multi_end`; changes keep
    /// arriving until the request is cancelled.
    pub fn req_account_updates_multi(
        &self, req_id: i64, account: &str, model_code: &str, ledger_and_nlv: bool,
    ) {
        if self.session_over() {
            return self.report_reason(req_id, &Refusal::not_connected("Not connected"));
        }
        ClientCore::note_account_selection(&self.shared, model_code);
        let account = if account.is_empty() || account.eq_ignore_ascii_case("All") || account == "AllNonProp" {
            ClientCore::note_account_selection(&self.shared, account);
            self.account_id.clone()
        } else { account.to_string() };

        // Held open from where the answer stands, and answered there with the
        // account whole: every batch after it is what has moved since. The
        // engine holds it for the account to state itself, as it holds the
        // holdings answer beside this: an account that has said nothing since
        // the connection dropped reads exactly like one holding nothing.
        if let Err(why) = self.send(ControlCommand::Ask(Ask::AccountUpdatesMulti { req_id, account, model_code: model_code.to_string(), ledger_and_nlv })) {
            self.refuse_request(req_id, &why);
        }
    }

    /// Cancel multi-account updates. Matches `cancelAccountUpdatesMulti` in C++.
    ///
    /// The request stops being reported to. The venue keeps the account
    /// current whether or not anyone is listening, as for
    /// `cancel_account_updates`; what stops is the reporting — a figure that
    /// moves after this is no longer delivered on `account_update_multi` for
    /// this request.
    ///
    /// A request the engine still holds is withdrawn, never answered; one it
    /// answered stops where the withdrawal stands, after its answer.
    pub fn cancel_account_updates_multi(&self, req_id: i64) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        if let Err(why) = self.send(ControlCommand::Retire(Retirement::AccountUpdatesMulti(req_id))) {
            self.report_reason(req_id, &why);
        }
    }

    /// Subscribe to holdings of the named account under this request number.
    /// A model is taken and not applied, with a log notice once per session.
    pub fn req_positions_multi(&self, req_id: i64, account: &str, model_code: &str) {
        if self.session_over() {
            return self.report_reason(req_id, &Refusal::not_connected("Not connected"));
        }
        ClientCore::note_account_selection(&self.shared, model_code);
        let account = if account.is_empty() || account.eq_ignore_ascii_case("All") || account == "AllNonProp" {
            ClientCore::note_account_selection(&self.shared, account);
            self.account_id.clone()
        } else { account.to_string() };

        // Watched from where the answer stands, and answered there with what
        // the account holds. The engine holds it for the account to state
        // itself, as it holds the plain answer: an account that has said
        // nothing since the connection dropped reads exactly like one holding
        // nothing.
        if let Err(why) = self.send(ControlCommand::Ask(Ask::PositionsMulti { req_id, account, model_code: model_code.to_string() })) {
            self.refuse_request(req_id, &why);
        }
    }

    /// Cancel multi-account positions. Matches `cancelPositionsMulti` in C++.
    /// Stop watching holdings under this request.
    ///
    // nothing to withdraw: the venue keeps the account current whether or not
    // anyone is listening, as for `cancel_positions`. What stops is the
    // reporting — a holding that moves after this is no longer delivered on
    // `position_multi` for this request.
    //
    // A request the engine still holds is withdrawn, never answered; one it
    // answered stops where the withdrawal stands, after its answer.
    pub fn cancel_positions_multi(&self, req_id: i64) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        if let Err(why) = self.send(ControlCommand::Retire(Retirement::PositionsMulti(req_id))) {
            self.report_reason(req_id, &why);
        }
    }

    /// Holdings the venue reports that this broker does not hold itself:
    /// positions held away at another broker, and rows it marks as shown but
    /// not held.
    ///
    /// Kept apart from `positions`, which answers what the account itself
    /// holds. The reference client has no call for these — its own front end
    /// shows them in a separate table — so this is the only way to reach them.
    pub fn positions_elsewhere(&self) -> Vec<crate::types::PositionElsewhere> {
        self.shared.portfolio.positions_elsewhere()
    }

    /// The account figures describing one of the sets of holdings the account
    /// does not hold itself, as name, value and the currency each is stated
    /// in. A figure stated in two currencies is two figures.
    ///
    /// The venue states these the same way it states the account's own, and
    /// mixing them in would overstate what the account is worth, so they are
    /// kept where the holdings they describe are kept.
    pub fn values_elsewhere(&self, held: crate::types::HeldElsewhere) -> Vec<(String, String, String)> {
        self.shared.portfolio.values_elsewhere(held)
    }

    /// The account as the venue last stated it whole, or nothing while a
    /// download is running. Answered from the struct alone, a caller read all
    /// zeros before the first download and the pre-drop figures after a drop,
    /// with nothing to say so; the other surface answers `None` for both.
    pub fn account(&self) -> Option<AccountState> {
        self.shared.portfolio.account_download_complete()
            .then(|| self.shared.portfolio.account())
    }
}
