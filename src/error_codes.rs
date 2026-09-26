//! What a refusal is, and the number it carries.
//!
//! The reference client reports a refused request through `error(id, code,
//! message)` under a number the caller can branch on, rather than as a failure
//! of the call itself. A refusal raised here carries the same number, so a
//! program written against that client reads the same value whichever surface
//! it came through.

use std::fmt;

/// A request the client will not send, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The number the reference client reports this class of refusal under.
    pub code: i32,
    /// What was wrong, in words a caller can read.
    pub message: String,
}

impl Refusal {
    /// The request is malformed or contradicts itself.
    pub const VALIDATION: i32 = 321;
    /// Nothing the venue holds matches the contract described.
    pub const NO_DEFINITION: i32 = 200;
    /// The session is not up, so nothing can be sent on it.
    ///
    /// The number the reference client reports a request made before
    /// connecting, or after the connection went, under.
    pub const NOT_CONNECTED: i32 = 504;
    /// Whether orders entered elsewhere are bound was set by a client that is
    /// not the one they are bound to.
    ///
    /// The number a gateway answers a client other than 0 with when it asks
    /// not to bind them; asked to bind them, the request fails validation
    /// first and is answered with 321. It answers the request itself rather
    /// than sending it on, so this is the whole of what happens.
    pub const AUTO_BIND_NOT_THIS_CLIENT: i32 = 327;

    /// The venue said nothing at all before the wait ran out.
    ///
    /// This client's own number rather than the venue's: the reference client
    /// has none, because it does not wait — it hands a request over and leaves
    /// the caller to decide how long to care. A caller that wants to tell
    /// silence apart from a refusal branches on this; the venue never sends it.
    pub const NO_ANSWER: i32 = -1;

    /// A refusal a gateway gives no number of its own.
    ///
    /// A gateway states one under the largest number an integer holds.
    pub const UNNUMBERED: i32 = i32::MAX;

    /// The request is malformed or contradicts itself.
    pub fn validation(message: impl Into<String>) -> Self {
        Self { code: Self::VALIDATION, message: message.into() }
    }

    /// Nothing the venue holds matches the contract described.
    pub fn no_definition(message: impl Into<String>) -> Self {
        Self { code: Self::NO_DEFINITION, message: message.into() }
    }

    /// Nothing can be sent, because there is no session to send it on.
    pub fn not_connected(message: impl Into<String>) -> Self {
        Self { code: Self::NOT_CONNECTED, message: message.into() }
    }

    /// The venue said nothing before the wait ran out.
    pub fn no_answer(message: impl Into<String>) -> Self {
        Self { code: Self::NO_ANSWER, message: message.into() }
    }

    /// A refusal a gateway gives no number of its own, under the number it
    /// marks one with.
    pub fn unnumbered(message: impl Into<String>) -> Self {
        Self { code: Self::UNNUMBERED, message: message.into() }
    }

    /// A refusal exactly as the venue stated it, under its own number.
    ///
    /// The number is the point: a caller branches on it the way it would
    /// against the reference client, which reports the same one on its error
    /// callback. Flattened into text it can only be matched on prose.
    pub fn stated(code: i32, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// The code a request number that is already watching something is refused
/// under.
pub const DUPLICATE_TICKER_ID: i32 = 102;

/// The code the reference client answers a verification request under, which
/// it answers itself rather than sending: intent to authenticate is stated on
/// the initial connect, and it never states it.
pub const BAD_MESSAGE: i32 = 508;

/// The code a combination naming no legs is refused under.
pub const COMBINATION_NEEDS_LEGS: i32 = 314;

/// The code a combination leg this client cannot state is refused under.
pub const COMBINATION_LEG_INVALID: i32 = 313;

/// The code an order on a security type the account may not trade is refused
/// under.
pub const SECURITY_NOT_PERMITTED: i32 = 203;

/// The code an order stating a trigger method the venue does not carry is
/// refused under.
pub const TRIGGER_METHOD_INVALID: i32 = 146;

/// The code a condition on an order that does not describe its contract is
/// refused under.
pub const CONDITION_CONTRACT_INCOMPLETE: i32 = 147;

/// The code an unreadable good-till date is refused under.
pub const GOOD_TILL_DATE_INVALID: i32 = 334;

/// The code a change an order cannot take is refused under.
pub const CHANGE_CANNOT_CHANGE_TYPE: i32 = 329;

/// The code a request the venue's front end could not carry out is refused
/// under. The reason follows `Error processing request:`.
pub const REQUEST_NOT_PROCESSED: i32 = 322;

/// The code a request the venue's front end could not read is refused under.
/// The reason follows `Error reading request:`. A trailing order naming both
/// an amount and a percentage is refused under it, as a gateway refuses one
/// while it reads the order, and so is an entry in a free-form option list
/// that is not written `key=value`.
pub const REQUEST_NOT_READ: i32 = 320;

/// The code a configuration read or update is refused under.
pub const CONFIGURATION_ACCESS_UNAVAILABLE: i32 = 10357;

/// What a configuration read or update is refused with.
pub(crate) const CONFIGURATION_ACCESS_MESSAGE: &str = "Configuration access via API is not available. Please refer to the application interface to view or update your settings.";

/// The code an order stating a discretionary amount below nought is refused
/// under.
pub const DISCRETIONARY_AMOUNT_INVALID: i32 = 168;

/// The code a combination priced on its legs that states a price of its own as
/// well is refused under.
pub const COMBO_AND_LEG_PRICES: i32 = 10054;

/// The code a combination priced on every leg is refused under, where it is
/// not one a gateway prices by its legs.
pub const PER_LEG_PRICES_UNSUPPORTED: i32 = 10058;

/// The code a change naming a one-cancels-all group other than the order's
/// own is refused under.
pub const OCA_GROUP_REVISION: i32 = 10326;

/// The code a change naming a one-cancels-all type other than the order's
/// own is refused under.
pub const OCA_TYPE_REVISION: i32 = 10327;

/// The code an option in a request's free-form list, under a key the request
/// does not take, is refused under.
pub const MISC_OPTION_KEY_INVALID: i32 = 10337;

/// The code an option in a request's free-form list, with a value its key does
/// not take, is refused under.
pub const MISC_OPTION_VALUE_INVALID: i32 = 10338;

/// The code an order stating `e_trade_only` is refused under, where the
/// venue has withdrawn it.
pub const E_TRADE_ONLY_WITHDRAWN: i32 = 10268;
/// The code an order stating `firm_quote_only` is refused under, where the
/// venue has withdrawn it.
pub const FIRM_QUOTE_ONLY_WITHDRAWN: i32 = 10269;
/// The code an order stating `nbbo_price_cap` is refused under, where the
/// venue has withdrawn it.
pub const NBBO_PRICE_CAP_WITHDRAWN: i32 = 10270;
/// The code an order stating `e_trade_only` is warned under, where the
/// venue has not withdrawn it and the order goes without it.
pub const E_TRADE_ONLY_DROPPED: i32 = 2168;
/// The code an order stating `firm_quote_only` is warned under, where the
/// venue has not withdrawn it and the order goes without it.
pub const FIRM_QUOTE_ONLY_DROPPED: i32 = 2169;
/// The code an order stating `nbbo_price_cap` is warned under, where the
/// venue has not withdrawn it and the order goes without it.
pub const NBBO_PRICE_CAP_DROPPED: i32 = 2170;

/// The code an order declining smart routing is refused under, where the
/// venue has withdrawn that choice.
pub const OPT_OUT_SMART_ROUTING_WITHDRAWN: i32 = 10348;

/// The code an order declining smart routing is warned under, where the
/// choice is dropped and the order placed without it.
pub const OPT_OUT_SMART_ROUTING_DROPPED: i32 = 2181;

/// The code a withdrawal naming an order this client is not working is
/// answered under.
///
/// The same number the change path already uses for a number it holds no
/// record of: the two are the same fact under two keys, and answered
/// differently they read as different failures.
pub const NO_SUCH_ORDER: i32 = 135;

/// The code a withdrawal naming a historical query this client is not
/// answering is answered under.
pub const NO_SUCH_HISTORICAL_QUERY: i32 = 366;

/// The code an order type a gateway places and this client does not is
/// refused under, naming the type.
///
/// The number a gateway gives an order type the contract does not take on its
/// exchange.
pub const ORDER_TYPE_UNSUPPORTED: i32 = 387;

/// The code a name that is no order type is refused under, as a gateway
/// refuses it: *Invalid order type*.
pub const INVALID_ORDER_TYPE: i32 = 10051;

/// The code a replace naming a contract other than the order's is refused
/// under.
pub const ORDER_DOES_NOT_MATCH: i32 = 105;

/// The code a withdrawal of an order no longer in a cancellable state is
/// refused under.
pub const NOT_CANCELLABLE: i32 = 161;

/// The code a withdrawal is refused under, and not made, when a gateway
/// cannot read the manual time it states.
pub const MANUAL_CANCEL_TIME_INVALID: i32 = 10301;

/// The code a log level outside the range the client carries is refused under.
pub const LOG_LEVEL_INVALID: i32 = 319;

/// The code a request for every account is refused under on a login the venue
/// adds accounts to as it runs.
pub const ALL_NOT_FOR_DYNAMIC_ACCOUNTS: i32 = 10200;

/// The code a placement under a number the venue has already worked an order
/// under is refused under.
///
/// The venue refuses a repeated number only while it is still working one, so
/// after a fill it takes the placement as a new order -- which is how a caller
/// retrying what it thought had failed ends up holding two.
pub const DUPLICATE_ORDER_ID: i32 = 103;

/// The code a request number already running a scan is refused under.
///
/// Its own number rather than the one a quote subscription is refused under,
/// for the same reason a historical query has its own: a caller branches on
/// which request it made.
pub const DUPLICATE_SCANNER_SUBSCRIPTION: i32 = 385;

/// The code a withdrawal naming a scan this client is not running is answered
/// under.
pub const NO_SUCH_SCANNER_SUBSCRIPTION: i32 = 365;

/// The code a withdrawal naming a book this client does not hold is answered
/// under.
///
/// Its own number rather than the one a quote subscription is withdrawn under:
/// the catalogue names depth separately, and a caller branches on which of the
/// two it asked for.
pub const NO_SUCH_BOOK: i32 = 310;

/// The code a book is refused under where the market asked names no book
/// for the contract's type, before the venue is asked.
pub const DEEP_DATA_NOT_SUPPORTED: i32 = 10092;

/// The code the venue's restart of a book is reported under.
///
/// Not a refusal: it tells a caller holding a book to empty it before applying
/// what follows, which is the only way a book that shrank can shrink on the
/// caller's side.
pub const DEPTH_BOOK_RESET: i32 = 317;

/// The code a withdrawal naming a subscription this client does not hold is
/// answered under.
///
/// A caller branches on this to learn that its own record and this client's
/// disagree -- that what it believes it is withdrawing is not something this
/// client holds. Silence is indistinguishable from a withdrawal that worked.
pub const NO_SUCH_SUBSCRIPTION: i32 = 300;

/// The code a request number already answering a historical query is refused
/// under.
///
/// A separate number from the one a live quote subscription is refused under:
/// the two are different requests and a caller branches on which it made.
pub const DUPLICATE_HISTORICAL_QUERY: i32 = 386;

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// The validators shared with the Rust client state a reason and no number;
/// a request refused for a reason of that kind failed validation.
impl From<String> for Refusal {
    fn from(message: String) -> Self {
        Self::validation(message)
    }
}

impl From<&str> for Refusal {
    fn from(message: &str) -> Self {
        Self::validation(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_alone_is_a_validation_refusal() {
        let refusal: Refusal = "quantity must be positive".to_string().into();
        assert_eq!(refusal.code, Refusal::VALIDATION);
        assert_eq!(refusal.to_string(), "quantity must be positive");
    }

    /// A refusal the venue stated keeps the venue's number.
    ///
    /// Flattened into text — which is what happened to every answer the
    /// waiting calls returned — the number is still readable by a person and
    /// no longer branchable by a program. A caller against the reference
    /// client gets it on the error callback and switches on it.
    #[test]
    fn a_refusal_the_venue_stated_keeps_its_number() {
        let refused = Refusal::stated(10197, "no market data during competing session");
        assert_eq!(refused.code, 10197);
        assert_eq!(refused.to_string(), "no market data during competing session");

        // A request that never left has its own number, and it is the one the
        // reference client reports for a request made with no session. Left as
        // an untyped message it became a validation failure, which says the
        // venue refused something it never saw.
        let gone = Refusal::not_connected("Engine stopped: sending on a closed channel");
        assert_eq!(gone.code, 504);
        assert_ne!(gone.code, Refusal::VALIDATION);

        // Silence is this client's own answer, not the venue's, and says so
        // with a number the venue never sends.
        let quiet = Refusal::no_answer("no answer within 15s to head timestamp");
        assert_eq!(quiet.code, Refusal::NO_ANSWER);
        assert!(quiet.code < 0, "not a number the venue can state");
        assert_ne!(quiet.code, Refusal::VALIDATION, "silence is not a bad request");

        // And it still reads as prose wherever a caller wants prose -- through
        // Display, which is the only way. A refusal does not convert to a
        // string on its own: it did, and a refusal handed to a constructor
        // that takes prose was flattened onto the general number in silence,
        // discarding the one the validator had just stated. Two call sites
        // were doing exactly that. Without the conversion each is a compile
        // error instead.
        assert_eq!(refused.to_string(), "no market data during competing session");
    }

    #[test]
    fn an_unnamed_contract_is_reported_under_its_own_number() {
        assert_eq!(Refusal::no_definition("nothing matches").code, 200);
    }
}
