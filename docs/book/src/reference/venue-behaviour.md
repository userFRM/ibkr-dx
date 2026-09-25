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

## Size-only changes

`ignoreSize` on `req_historical_ticks` and `ignore_size` on
`req_tick_by_tick_data` ask the venue to leave out a bid/ask change that moves
only a size. A gateway sends the filter and passes on what the venue answers
as it stands, and so does this client: nothing is filtered here. On historical
ticks a gateway asks for midpoint ticks that way whatever the flag says, and so
does this client; for trades and aggregated trades it is not asked. One
session saw the venue answer the same with the filter as without it, on
historical bid/ask ticks and on a bid/ask stream.

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
subscription of its own, beside the volatilities it states the model from. The
model tick, 13 or 83 on a delayed feed, is built from those as a gateway builds
it:

- The greeks and the option price are the venue's model's. The price of a
  warrant or a structured product (`WAR`, `IOPT`) is multiplied by its
  multiplier.
- The implied volatility is the first of the venue's model, mid and last
  volatilities that stands, carried from a trading day to a year of 252.
- `tickAttrib` is 1 where the mid's volatility was worked from prices.
- The underlying's price is the one the chain parameters on the underlying
  (687, asked for once per underlying on each connection) state for the
  option's trading class, multiplier and last trading day.
- The present value of dividends is that of the payments the underlying's
  schedule states over the option's life, discounted at the currency's rate
  for the option's term. The currency's rates are the venue's answer to the
  query a contract's payments are asked with, naming the currency (`div USD`):
  each entry is written as a payment is, its date the day the rate runs to and
  its amount the percentage, and a gateway reads it that way.

It is rebuilt at most once a second, on the clock's seconds, and a request is
sent it when any figure differs from the last one that request was sent. What
was stated stands through a reconnect until the venue states it again, so a
tick owed when the connection drops is built from what was stated before it.

The bid's, the ask's and the last's computations, 10, 11 and 12 (80, 81 and 82
on a delayed feed), are a gateway's own and not the venue's. This client works
them out the way a gateway does, with an option model of its own, from the
inputs below; the underlying's price it works them from is not always the one a
gateway takes, as [Limits](./limits.md) says.

- The implied volatility is the one the venue states for the side — the bid's
  and the ask's together (736), the last's on its own (737) — carried to a year
  of 252 trading days. `tickAttrib` is 0 on every one of them, as a gateway
  sends it: it sends each side with the attribute of the last of that side it
  sent, which starts unset.
- The option price is the side's price as the model takes it in, to single
  precision: a bid of 4.74 is 4.739999771118164. A warrant's or a structured
  product's is per contract. The model takes a bid or an ask in only with a
  size, and a last only where it or its size is above nought, so a side the
  quote has nothing on states no price: a frozen quote states one as -1 with
  no size, or a last of nought.
- The greeks are this client's option model's at that volatility, the model for
  volatilities worked from prices where any the venue states for the option,
  or its chain parameters, say so. They are worked from the underlying's price
  the chain parameters state — the first set for the option's class at its
  multiplier, or, where no set names the class, the only set or else the first
  for its last trading day at its multiplier — the time to the option's
  expiry, the currency's rate for its term and the dividends above. A side's
  greeks are replaced only by four that can be worked out. The time runs to the
  option's last trading day at the time of day its definition states (read as
  a gateway reads it, so 2400 is the next day's midnight), or at the end of
  that day's last liquid session, on the zone its sessions are stated on, or,
  where neither states it, to that date alone, which the model reads as four
  in the afternoon on that zone. The venue states the time of day as four
  digits (1615 on SPY, 1500 central time on SPXW), on SPY's and AAPL's options
  only for an expiry two to three weeks off or nearer, and the sessions about a
  week ahead, so a later SPY expiry runs to 16:00 and not to its 16:15 close,
  on a gateway as here.
  The model reads the clock once a minute. The currency's rates are counted
  from the day they were taken in.
- They are rebuilt at most once every two seconds, for the options something
  new was stated for. A side is put to every request watching the option when
  its volatility moves, and all three again on every change to the option's
  quote. Each figure a side does not state is the last one of that side the
  request was sent, and a request is sent a side when that differs from the
  last.

What is not carried yet is on [Limits](./limits.md).

What the protocol carries no request for is the inversion: an option price or a
volatility the caller supplies, for the venue to work back from. A gateway
solves that against the venue's published model for the same contract, and so
does this client. Where the venue has published no model, nothing is answered,
on a gateway as here. How long either waits for the model is on
[Limits](./limits.md).

# Orders

## Order IDs across sessions

The next order ID is retained per account and API client ID: the one stated at
connect, `EClientConfig::client_id` in Rust and `client_id` / `clientId` in
Python, zero on both unless stated. The saved counter raises `next_valid_id`
and Rust `order_id_floor`, which reads the same ID, at connect, and venue
replay can raise them further. Orders absent from that replay still leave their
saved counter behind.

Both surfaces state `next_valid_id` once a session has connected, after the
venue has named the orders the account is working: Python before `connect()`
returns, Rust on the first `process_msgs` read after that naming. `req_ids()`
states it again.

`next_order_id()` reserves under an exclusive file lock and writes the raised
counter before returning. Bracket helpers reserve their consecutive IDs in one
operation. A positive ID supplied to `place_order()` or `exercise_options()`
raises the saved counter before the call returns. IDs the engine assigns
itself, to attached children and to exercises stated without one, are counted
in memory and saved with the session's next placement or reservation.

As through a gateway, a new order under an ID at or below the counter saved
before the session opened, or at or below the ID this client gave a working
order the venue names for it, is refused with error 103, "Duplicate order id".
An order the session holds under that ID is modified as usual. Another client's
working orders raise nothing here. Previews are neither counted nor refused
against the saved counter.

`next_valid_id` states a floor; it does not reserve it. Programs sharing an
account, client ID and file should use `next_order_id()` for each allocation
rather than incrementing separate counters from that callback. The file does
not coordinate different client IDs, separate files or different machines.

The default file is `ibkr-dx/order-ids.json` under:

| Platform | Data directory |
| --- | --- |
| Linux | `$XDG_DATA_HOME`, or `~/.local/share` when unset |
| macOS | `~/Library/Application Support` |
| Windows | `%APPDATA%` |

Rust sets `EClientConfig.gateway.order_id_file = Some(path.into())`. Python sets
`connect(settings={"order_id_file": path})` for one session, or
`ibkr_dx.configure(order_id_file=path)` before connecting. Both read
`IBKR_DX_ORDER_ID_FILE` when the session leaves it unstated. An empty string
disables persistence; Rust `None` and Python `configure(order_id_file=None)`
restore the environment/default selection.

Entries are keyed by the SHA-256 digest of the account, in lowercase hex, so
the file holds no account number. A file keyed by the account itself is
rewritten under the digests when a session opens it, keeping every counter.
The digest is not a secret: anyone holding the file can find an account's entry
by digesting candidate account numbers.

To move the counter, stop all processes using it, move the JSON file and set
all of them to its new path. The client creates parent directories and keeps
`.lock` and `.tmp` siblings for locking and replacement. Writes are synced
before rename, and the containing directory is synced on Unix. A read, lock
or write failure warns once in the session's log and leaves it allocating from
memory and venue replay; a counter that could be read still floors the
session. The file then provides no reservation guarantee for that session.
Removing the file or disabling persistence also removes the floor for orders
the venue no longer replays.

## One number, more than one order

A number whose order the venue withdrew or refused is free again at the venue,
so a program that numbers its orders from the same point more than once sends a
later order under an earlier one's number, and the account then holds several
orders under one number. The venue names each with a name of its own, which
every report on the order repeats, and each is kept as the order it is:

- `reqCompletedOrders` answers with every order the venue states finished, each
  carrying the number as its permanent id, so two of them can share one. While
  that answer is open, every report the venue marks as restating the past is
  filed with it unless it is about an order this session holds, whatever this
  session once sent under its number.
- A report the venue marks as restating the past is about the order this
  session holds under its number where it states one of the names the venue
  has given that order on its reports of what is happening to it, and about
  another order where it states another name. One stating no name is read as
  though the venue had not named the order. Before the venue has named an
  order this session sent, as it does not name a market-on-close order in
  regular hours until it is withdrawn, the report is about another order where
  the venue counts that order at or below the highest count it had stated when
  this session sent its own, or where every time it states for the order and
  the event is more than two seconds before this session sent it, on the
  venue's clock. The venue counts the orders it creates from one count that
  only rises, the third segment of its name for an order under the first two,
  so an order counted there was created before this one went out. A count
  above that says nothing: an order that finished before the session opened
  is not restated then, so it can count above every order the venue had named
  by the send. Under a number this session holds no order under, such a report
  outside that answer is another order's where it names another order than the
  one this session saw finish under the number in the last five minutes.
- A report of what is happening now is about the order this session holds
  under its number unless it states none of the names the venue has given that
  order and the venue counts it at or below the highest count it had stated
  when this session sent it, which makes it an order created before.
- A report about another order is filed with the completed orders while that
  answer is open, or, outside it, kept as one of the day's executions where it
  states one. It reaches the order under the number in neither case.
- An order the venue names as working under a number an earlier order finished
  under is taken as that number's order.
- A new order under a number an earlier order used is refused with error 103,
  `Duplicate order id: N`, only as a gateway refuses one: a gateway refuses
  under 103 a new order whose number is not above a mark it keeps for each
  client from one session to the next, raised by every order that client
  places and every working order it states to it, and not by the executions
  the venue restates. An order the session holds under that number is
  modified as usual.
- `cancel_order_by_perm_id` withdraws the order held under the number, where
  more than one working order's record carries it; of the records carrying it,
  the one whose order the engine holds.

## Attached cancellation groups

A child placed with `parent_id` / `parentId` uses the known parent's decimal
wire order number as its OCA group. The default is reduce-on-fill without
block. This applies to separately transmitted children and to families
released by the last child's transmit flag. The bracket helper uses the same
group, so individually added children join it. The parent has no OCA group;
scale-generated children retain their scale group. Replacement retains the
original parent reference and group.

## A modification in flight

A replacement reads `PendingSubmit` once it is sent, as a gateway holds an
order it has sent and the venue has not acknowledged; nothing is said at the
send itself. The venue's pending report follows, as a status report, and a
gateway reads it as the order acknowledged: the caller is told `PreSubmitted`,
and the working status again once the venue accepts the change. The report
ahead of a change the venue makes itself (the exits of a bracket once its
parent fills) reads the same way. A pending report for a revision earlier than
the one last sent, or on an order being withdrawn, changes no status, and one
sent as an execution report rather than a status report changes none either:
the order is stated again as it stands. An order whose state this session lost
with a dropped connection takes its status from the report instead.

## Refused modifications

A rejection that names the original order reports error 201 under that order's
ID and leaves its last accepted terms and prior state in the open-orders view.
Fills and cancellation progress already received are retained. That refusal
reports a rejected modification; it does not declare the original cancelled
or inactive. After the error, the order is restated through `open_order` and
`order_status` with those terms and that state, as a gateway restates it.

Replacement carries `useAutoPriceForHedge` under the same order type, hedge,
opt-out and `HDGLMT` feature conditions as placement. Changing `hedgeMaxSize`
keeps the pricing instruction.

## Refused cancellations

A cancel the venue refuses on a report naming the cancel, as it refuses one
for an order another session is already withdrawing ("Order is already being
cancelled by another user"), reports error 201 under the order's ID with the
venue's words. The order stays in the state it held before the cancel went
out, in the open-orders view and under its own permId: the refusal is about the
cancel, not the order. After the error, the order is restated through
`open_order` and `order_status` in that state, as a gateway restates it.

## What the account may trade

The venue states at logon which security types this account may trade, and
`order_permissions()` lists them. The venue answers an order for a type it has
not permitted by returning the order inactive, with no text. This client
refuses such an order before it is sent instead, with the reason on the error
callback.

## Conditions in the overnight session

`conditionsIncludeOvernight` asks for an order's conditions to be measured in
the overnight session too. It travels with the conditions, beside
`conditionsIgnoreRth` and `conditionsCancelOrder`, each stated on or off as a
gateway writes them, and states nothing on an order that has none. As a
gateway does, this client refuses it under 10371 when the order is placed,
*"Conditions include overnight is not supported for this instrument or is not
enabled for this account."*, unless the logon enables `CONDINCOVN` and the
contract's valid exchanges include `OVERNIGHT` or `IBEOS`; a contract whose
definition is not yet held is looked up first. A replace is sent without that
check. A report that states the conditions is read for all three flags, one it
leaves out being off, as a gateway reads it, and `open_order` carries them.

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
