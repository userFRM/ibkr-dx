# Venue behaviour

What a program meets here that is how the venue answers, or how the TWS API is
defined, rather than something this client chose. They are written down because
a program meeting one for the first time reads it as a fault in the client, and
checking the wrong thing costs a session. Where a gateway is known to answer
the same way, the section says so.

What this client does differently from a gateway is on [Limits](./limits.md).

# Quotes and books

## A bid of -1 is the venue saying there is none

Some instruments carry no bid and no ask. The venue says so by sending `-1` on
both, and this client passes it on as sent, as a gateway does, rather than
turning it into a zero.

Which instruments those are is the venue's to say and not something this client
can work out. Indices are where it was met: most of the ones below carry no
quote, and one of them does, so the kind of instrument is not the rule.

Three answers look alike at first, and are worth telling apart before
concluding a subscription is missing:

* **No ticks at all, and no `market_data_type` callback either.** Nothing
  arrives to say what feed you are on. Asking for delayed data then answers
  normally.
* **`-1` on bid and ask**, no bid or ask *size*, and a `market_data_type` of 1
  saying the feed is real time, while the last, high, low, close and open
  arrive and move. The instrument has no quote.
* **A real bid and ask.** It is quoted.

Measured together with the market open: `SPX` on `CBOE` quotes; `VIX` on the
same exchange and the same feed does not, and reads `-1` while its volume ticks
up on a real-time feed. `RUT`, `INDU` and `TICK-NYSE` read `-1` the same way.
`NDX` real time is the first case — silence, no feed stated — and it answers as
soon as delayed data is asked for.

Delayed data carries no index quote at all: on `market_data_type` 3 every one
of them reads `-1`, `SPX` included. A quote that is on the real-time feed and
not the delayed one is a property of the feed rather than of the account.

## A book restarts after a reconnect

After a connection is rebuilt the venue restates a book from the top. This
client reports that on the error callback as 317, the TWS API's notice that a
book was reset, before the first new level arrives. Empty the book on it, or
the levels that follow land on top of the old ones.

## Which venue a quote's exchange mask names

The map from a quote's exchange-mask bits to venues is answered at regular
trading hours. Outside those hours the venue states none, to a gateway as to
this client, and a venue's letter is empty until it does.

## The fourth integer on a trading-status record

The trading-status record (generic tick 437) carries a status mask, a
timestamp and a status index, and then a fourth integer. This client reads the
first three and does not interpret the fourth, which has no stated meaning; a
gateway assigns it none either.

## Dividends arrive as tick 59

Ask for `456` in the generic tick list and what a contract pays out arrives on
`tick_string` 59, which is how the TWS API delivers dividends and how a gateway
delivers them.

## Callbacks that never fire on a gateway

The TWS API declares seven callbacks that never fire for a program on a
gateway: the four steps of the verification handshake (`verify_message_api`,
`verify_completed`, `verify_and_auth_message_api`,
`verify_and_auth_completed`), the exchange-for-physical quote (`tick_efp`), the
delta-neutral validation (`delta_neutral_validation`), which a gateway never
sends, and `win_error`, which no message on the wire carries; the TWS API's
Python client declares it and never raises it. Each is declared here, so a
program implementing one compiles and runs, and each fires here as often as it
does on a gateway: never. Trouble on a connection reaches the error callback.

# Requests

## Requests a gateway answers itself, or not at all

`verify_request` and `verify_and_auth_request` are answered by the reference
client itself, under 508 (*Bad message  Intent to authenticate needs to be
expressed during initial connect request.*), and never reach a gateway; they
are answered the same way here. `verify_message` and
`verify_and_auth_message` reach a gateway, which reads and discards them, so
here they send nothing and nothing answers. `cancel_contract_data` and
`cancel_historical_ticks` ask the venue nothing on a gateway either: they only
stop a gateway re-sending a request it held back while its connection to the
venue was down, which this client never does. A lookup or ticks already asked
for still arrive. Without a session — before one exists, or once it is over —
each is refused under 504.

## A broad lookup takes longer than one contract

A lookup naming a whole class is a different question from one naming a single
contract. `SPY` options across every expiry is 13,580 definitions over nineteen
exchanges, and the venue takes about ten seconds to send the first of them.

The wait measures silence, not the length of an answer: it is reset each time
the venue speaks, so an answer that runs for as long as that one does is not cut
off part-way through. A lookup that does run out says how many definitions
arrived before it did, since a partial answer and no answer are different facts
and only the first says to ask a narrower question — by naming an expiry, or a
single venue.

## An ambiguous description

`req_contract_details` answers a description that matches several listings
with every one of them, as a gateway does.

## Looking a contract up

- By identifier: `CUSIP`, `SEDOL`, `ISIN`, `RIC`, `FIGI` and `BB_SYMBOL`,
  spelled exactly so. A gateway reads any other name — a lower-case one
  included — as no identifier, and looks the contract up by its symbol with
  the identifier left out; so does this client. The venue, primary exchange
  and currency ride beside the identifier, and an ISIN asked about anywhere
  but SMART is asked of any type. Nothing of an issuer is stated beside an
  identifier.
- Only a details request looks a contract up by identifier or issuer. Market
  data, depth, bars, head timestamps, ticks and the rest carry neither to a
  gateway, which looks their contract up by its description; so does this
  client, whatever else the contract holds.
- A venue or currency left empty is not stated on the lookup.
- `includeExpired` is stated on every lookup by description a details request
  makes, and a bars, head-timestamp or ticks request carries it to the venue
  as the contract states it. A details lookup by identifier or by contract id
  does not state it.
- `FUT+CONTFUT` (or `CONTFUT+FUT`): the continuous future is asked for first
  and the listed months once it is answered, answer or refusal; the months
  come back first and the continuous contract after them under `CONTFUT`, with
  the lead month's id. Where the venue names no listed month the request is
  answered as a contract not found (200), whatever the continuous lookup
  named. `CONTFUT` alone comes back as `CONTFUT`, one per venue and
  multiplier. Named by an identifier, both lookups ask by that identifier.
- A continuous future asked for by contract id is looked up by that id and
  handed back as the venue names it. A gateway goes on to ask for the
  continuous contract as well, and this client does not.

## Size-only changes on historical ticks

`ignoreSize` on `req_historical_ticks` leaves out a bid/ask change that moves
only a size. A gateway asks for midpoint ticks that way whatever the flag
says, and so does this client; trades and aggregated trades are not filtered.

## Arguments that change nothing

Two arguments the TWS API defines have no effect on a gateway, and have none
here:

| Call | Argument | Why |
| --- | --- | --- |
| `req_ids` | `num_ids` | The next valid id is answered whatever number is asked for |
| `req_real_time_bars` | `bar_size` | A real-time bar is five seconds. The venue's request carries no bar size; a gateway reads the number and does not use it |

Two more act on a gateway and change nothing here, and are on
[Limits](./limits.md): `is_smart_depth` on `cancel_mkt_depth`, and
`b_auto_bind` on `req_auto_open_orders`.

## Stopping profit and loss

`cancel_pnl` stops the updates. The venue has no message withdrawing the
subscription itself, on a gateway as here, so the updates stopping is what the
call does.

## Implied volatility and option price

The venue computes its option model and publishes it per option, on a
subscription of its own. Volatility and greeks read off a quote are the venue's
own numbers.

What the protocol carries no request for is the inversion: an option price or a
volatility the caller supplies, for the venue to work back from. A gateway
solves that against the venue's published model for the same contract, and so
does this client. Where the venue has published no model, nothing is answered,
on a gateway as here. How long either waits for the model is on
[Limits](./limits.md).

# Orders

## What the account may trade

The venue states at logon which security types this account may trade, and
`order_permissions()` lists them. The venue answers an order for a type it has
not permitted by returning the order inactive, with no text. This client
refuses such an order before it is sent instead, with the reason on the error
callback.

## What a crypto order needs

A crypto is quoted around the clock and priced and sized differently from a
share. These are the venue's rules, and a gateway meets the same refusals.

| | |
| --- | --- |
| Time in force | Immediate-or-cancel, or the one measured in minutes. A day order is refused: *"The crypto buy order must be Minutes or IOC"* |
| Price | On the venue's grid. One that is not is refused as a price, *"Invalid Price"*, rather than rounded — this client sends prices as they were given |
| Quantity | A fraction, counted in hundred-millionths. A thousandth of a coin is an ordinary size |

# Account values

Account values and per-currency ledger values are separate rows, even when
their key and currency match. A full account subscription can receive both;
`req_account_updates_multi` with `ledger_and_nlv=True` receives only ledger
rows. An account-only change does not update a ledger-only subscription.
Unchanged rows are not repeated.

An `AccountCode` used only to identify an account delta does not overwrite the
account value. A frame that states `AccountType` before `AccountCode` supplies
the account value, including an explicitly empty one.

# Sessions

## What authenticates a farm connection

A market-data or trading connection is opened with a request carrying the
session's own token, and the venue may answer it in one of two ways: by asking
the session to authenticate in full, or by acknowledging the logon against the
token it was already given. Both are the venue accepting a credential.

Where it asks in full, the answer it sends back is checked: the venue states a
proof of the session key that only a party holding the account's verifier can
compute, and a logon whose proof does not match is refused. The group that
exchange runs in is this venue's own and no other, because a peer names it
before it has proved anything.

The logon runs beside the channel rather than inside it, so the protocol does
not bind the party holding the channel keys to the party that answered the
logon. That is a property of the venue's protocol, and a gateway's connections
are opened the same way.

## Contract expiry validation

Malformed `lastTradeDateOrContractMonth` is refused with code 10372 before
contract lookup or subscription state changes. Both surfaces check contract
details, market data, depth, tick-by-tick data, real-time bars, historical
bars and schedules, historical ticks, head timestamps, histograms, option
calculations and option exercise. Depth reports an empty exchange first.
Fundamental reports do not apply this check.

Accepted values are empty, case-insensitive `NOEXP`, `yyyyMM`, or a valid
calendar `yyyyMMdd`, with years 1978 through 3000 inclusive. Accepted text is
sent unchanged. Callback errors retain the caller's request id. The error
message is:

> lastTradeDateOrContractMonth: The date entered is invalid. The correct format is yyyyMM for a contract month or yyyyMMdd for a date. E.g.: 202607 or 20260724.
