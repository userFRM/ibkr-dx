# Limits

What a caller has to know before writing against this client: what the account
decides, what is not settled here, and where this client answers differently
from a gateway. What a gateway answers the same way is on
[Venue behaviour](./venue-behaviour.md).

Nothing here is a call that returns as though it acted. A call this protocol
cannot carry reports why.

# What the account decides

## Market depth depends on the entitlement

A book is asked for at a named venue, and a level carries the maker the venue
names on it — empty where it names none, which is the ordinary shape on a venue
that quotes no makers. Which venues answer is the account's entitlement, not
this client's: a venue the account is not entitled to refuses by name, and the
refusal reaches the caller.

Measured on one account with the market open: a share on ARCA, a share on
ISLAND and a future on CME are each refused by name, while `EUR.USD`,
`GBP.USD` and `USD.JPY` on `IDEALPRO` all deliver, and a twenty-level book
delivers as readily as a five-level one.

A book of the size asked for stays that size. The venue sends every level it
holds and says nothing about the one that moves when a level inside the asked
size goes away, so this client keeps the book and states that move itself: a
withdrawal inside the book is followed by the level that came up into the place
it left, and a level arriving inside it is preceded by the withdrawal of the one
it pushed out. A caller applying the operations in the order they arrive holds
the size it asked for.

## Things an entitlement decides, not this client

* **News headlines** need a news subscription. Without one, the providers list
  is returned and every query comes back empty.
* **Corporate events content** needs a Wall Street Horizon subscription. The
  calendar's schema and event types are delivered either way; the events
  themselves come back empty without it.

# Not settled here

Something the protocol may well carry, which no session has established. Each
says what would settle it. None of them is a call that returns as though it
acted: a request this client will not send says so.

## What a replace states, and what it does not yet

A modify is the caller's order stated whole: its type, its prices and
everything it carries go out as its own placement would state them, which is
what a gateway's replace does. An order is replaced as itself whatever it is —
relative, pegged, trailing, midpoint, snap, limit-if-touched, an adjustable
stop, an algo, a conditional order, one with a minimum quantity — and a change
of type states the new type whole and nothing of the old one. A trail moved
between a percentage and an amount states the unit it is now in, and a trail
naming both is refused under 320, *Error reading request: Cannot specify
Trailing Amount and Trailing Percent at the same time*, as a gateway refuses it
while it reads the order. A ladder stated as a table is stated on its
placement alone, as a gateway states it, and a replace states its restart
instead.

What a gateway refuses in a replace is refused here in its words: a new
one-cancels-all group on an order that has one, under 10326 (*OCA group
revision is not allowed*), and a new way for the group to cancel, under 10327
(*OCA group type revision is not allowed*), unless the venue lifts both at
logon. The way is compared whether or not either names a group, and a way that
is not one of the four reads as the default, reduce on fill without block: an
order placed with the first way and replaced naming none is refused, and one
replaced from the default to a way outside the four is not. A parent or a group named on an order that had none is taken, and the
order goes on under the links it was placed with.

Four things are not settled:

- **The links on a replace.** A gateway's replace states neither the parent
  nor the group. This client's restates the ones the order was placed with,
  because a session once saw a bracket leg replaced without them leave its
  bracket — by a replace that also stated none of the order's other
  attributes. A leg replaced without them on a paper session, and then left to
  see whether its group still cancels it, would settle which is needed.
- **A relative order's replace.** One session saw a replace of one draw no
  answer, and a withdrawal after it none either. That replace stated a trigger
  a gateway does not state for this type; it states none now, and has not been
  measured since.
- **A change into a type the contract does not take.** A gateway refuses it
  before sending, under 329 and *Order modify failed. Cannot change to the new
  order type:* followed by the type's name, reading the order types the
  contract takes on its exchange. This client sends it and the venue answers:
  how a definition keys its lists of order types to each exchange is not
  established here, and a definition captured on a session would settle it.
- **A preview under the number of a working order.** A gateway prices it as a
  new order and leaves the working one alone. This client keys an order's
  record and its revisions on the number, so a preview there would stand in
  for the working order; it is refused under 329 until the two are kept apart.

A modify of an order this session did not place — one the venue named at
connect — is restated from the caller's own statement of it, links included:
there is no placement here to restate them from.

## Market-on-close acknowledgement

The observed MOC placement received no order report until cancellation about
31 seconds later, when a New report and then Cancelled arrived. The client
cannot report venue acceptance before the venue states it. Whether a gateway
receives an earlier report under the same account, order fields and session
features remains unsettled; no acknowledgement timing change is claimed.

## A trailing stop limit by percentage

A gateway takes a trailing stop limit whose trail is a percentage. What it
states as such an order's trigger is not established here, so this client
refuses one rather than put a price on it the caller did not ask for, and
takes the trail as an amount. A gateway placing one, read on the wire, would
settle it.

## The order types a contract takes on its exchange

A gateway refuses a midpoint peg on an exchange whose order types include
neither form of it, and a peg to best where they do not include the midpoint
form it rests on, under 387 and *Unsupported order type for this exchange and
security type.* This client sends both and the venue answers, for the reason
above: the lists are read here, but how each is keyed to an exchange is not
established.

## Alternate exercise transport

An exercise checks the selected account's positive position and waits for its
in-the-money figure, including when override is set. Override bypasses the
natural exercise/lapse check. The quantity is limited to the whole-contract
position captured before the wait. The alternate exercise transport named by
some logins is not implemented.

## Numbered ticks this client does not deliver

Ninety-six numbered market-data ticks reach a caller of the reference client.
Ninety of them reach one here. These six do not, for a reason that can be
checked rather than taken on trust.

| Ticks | What | Why not |
| --- | --- | --- |
| 10, 11, 12 and the delayed 80, 81, 82 | The option model struck against the bid, the ask and the last | Not sent by the venue. A gateway works each of them out with its own option model, at the volatility the venue states for that side (its bid, ask and last volatilities), with that side's price and the underlying's price it marks. How it marks the underlying is not carried here, so this client does not work them out. The venue's model is delivered on 13, and on 83 where the feed is delayed, as [Venue behaviour](./venue-behaviour.md) describes. This client's own model reads the venue's schedule of ex-dates and reproduces the venue's published price exactly, its delta within half a per cent and its gamma within one; what one day costs is within seven. What one point of volatility is worth is not, and the venue's own figures cannot settle how far out it is — at one strike its call and its put state vegas seven per cent apart, which is further than this client is from either |

Everything else the venue publishes and a caller can ask for is delivered, on
the callback the reference client delivers it on.

On the model tick itself, 13 or 83:

- The present value of dividends is not stated. A gateway works it out from the
  underlying's dividend schedule and the currency's rates, and the form of the
  rates the venue answers with is not established here.
- Before the venue states any model, mid or last volatility for an option, a
  gateway takes one from the option chain's volatility curve at the strike.
  That curve is not carried here, so the implied volatility is not stated until
  the venue states one.
- The underlying's price is not stated where the chain parameters state none
  for the option. A gateway takes the underlying's mark there.
- The underlying's price, and the per-contract price of a warrant or a
  structured product, are read from the option's definition as a
  contract-details request answers it. An option whose details a caller has not
  asked for is modelled without them: no underlying's price, and a warrant's
  price per unit.
- A gateway withholds the underlying's price on 13 from a caller without quote
  access to the underlying. That access is not checked here.
- A gateway sends a snapshot's model tick only once all eight figures are
  stated. With the dividend's present value unstated that would be never, so a
  snapshot is sent the tick as a stream is.
- On a frozen feed the tick is the live model's.

# Where this client behaves differently

Not a gap in what it can ask for. A program written against the other
behaviour will still be wrong, which is why they are here.

## Callbacks nothing fires

Two callbacks exist so a program written against the TWS API compiles and
runs, and nothing here fires them.

| Callback | Why |
| --- | --- |
| `reroute_mkt_data_req` | The contract a market-data request should be asked for under instead — a contract for difference standing for a share. A gateway sends it, and asks the venue nothing, for a contract for difference whose own definition asks for its underlying's data, where the account's logon permissions allow that. This client does not read a definition's flags for asking on the underlying before subscribing: the request goes to the venue as asked, and this never fires |
| `reroute_mkt_depth_req` | The same, for a request for the book rather than the quote |

Seven more never fire for a program on a gateway, and fire here as often: never.
They are on [Venue behaviour](./venue-behaviour.md).

Every other call and callback on the canonical list is served on both
languages. The call-by-call matrix is [generated from the source](./coverage.md).

## Smart depth is one request

A book is refused before anything is sent where a gateway refuses it, in its
words and in both languages: no exchange (*Please enter exchange.*), a
combination (*Market depth does not support combos.*) and no rows (*Market
depth rows requested must be greater than zero.*), each under 321. A book on a
market the routing table names for the top of the book only — a share on the
smart destination asked for without smart depth, as the table this account is
given reads — is refused before the venue is asked, under 10092, *Deep market
data is not supported for this combination of security type/exchange*, as a
gateway refuses it. Smart depth is not refused on a type a gateway gathers a
book for from each venue the contract trades on: shares, contracts for
difference, options, futures, currencies, bonds, crypto and the rest.

What differs is how smart depth is asked for. A gateway gathers one book from
each venue the contract trades on; this client asks for it as one request on
the smart destination, and on a share the smart destination serves no book, so
nothing arrives and no error follows. A gateway also matches a smart row by
the contract's listing exchange and aggregate group, so a share listed on PINK
in group 1 is served a book on the smart destination; this client does not know
those when it asks, and refuses every share on the smart destination without
smart depth.

## A regulatory snapshot is asked for whatever the permission

A regulatory snapshot (`regulatory_snapshot` on `req_mkt_data_ex`) is answered
by the venue in one message, which this client reads and hands to the
snapshot's own request, as a gateway does. Where the venue states the
contract's snapshot as chargeable, the moment the answer was read is stamped on
a clock corrected to the venue's and delivered as text on 85, in milliseconds;
a contract the account sees in real time is answered without it. Like a
gateway, this client waits up to two seconds for the contract's map of venues
before publishing, so the exchange letters on 32, 33, 84, 109 and 110 are the
contract's own, and the snapshot ends as soon as its answer is published.
Where the map never comes it publishes none of the answer, as a gateway
publishes none; the error a gateway reports then is not reproduced, as its
words are not settled here. On an account without the entitlement the venue
refuses the request, so no answer has been read here from a session.

What differs is what happens before and after. A gateway reads the contract's
snapshot permission first and refuses, without asking the venue, a snapshot
the permission does not allow — nothing stated, no top of book, or not
available through the API — and gives up on the data after seven seconds. This
client asks the venue whatever the permission, and waits up to eleven seconds.

## Arguments a gateway acts on and this client does not need

| Call | Argument | On a gateway | Here |
| --- | --- | --- | --- |
| `cancel_mkt_depth` | `is_smart_depth` | Says whether the book is found among the smart books or the exchange books, and one naming the wrong kind is answered with 310 and leaves the book running | The request id alone finds the book. Stated as the book was asked for, both withdraw the same one |
| `req_auto_open_orders` | `b_auto_bind` | Turns binding on or off for client 0 | What binding asks for is already the default: every session is told about every order on the account, so it changes nothing. A client other than 0 is refused, as a gateway refuses one |

## Configuration requests

`reqConfigProtoBuf(configRequestProto)` and
`updateConfigProtoBuf(updateConfigRequestProto)` report 10357 under the request
id on every connected session: *Configuration access via API is not available.
Please refer to the application interface to view or update your settings.*
Their snake spellings are `req_config_proto_buf(config_request_proto)` and
`update_config_proto_buf(update_config_request_proto)`. `None` makes no request;
configuration payloads are never applied. A request without a session reports
504.

Rust's `req_config(req_id)` and `update_config(req_id)` take only the request
id and deliver the same refusal through `process_msgs` and `Wrapper::error`.
They accept no configuration payload.

## Connect options

`set_connect_options` answers a caller that states options on `error`, under
321. The reference client hands these to its gateway on the greeting for the
gateway to read, and there is no gateway between this client and the venue.
Stating none is stating nothing.

## A withdrawal's time

A withdrawal states who is withdrawing the order and whether a person entered
it, and both travel on the cancel as a gateway writes them: from the
withdrawal, not the placement. A cancel that states neither carries neither.
An operator holding the byte that separates fields is refused under 321 and
nothing is withdrawn.

A manual cancel time is read as a gateway reads it — `yyyymmdd-hh:mm:ss` in
UTC, or `yyyymmdd hh:mm:ss` with the date and a zone optional — and one it
cannot read is refused under 10301 in a gateway's words, and the order keeps
working, as it does through a gateway. A time holding a character outside
ASCII is let through unread. A time that is read does not travel: a gateway
sends one only where the venue has turned that record on for the login, and
this client does not read whether it has. The withdrawal goes, and the caller
is told on `error` that the time did not. A gateway also refuses a time it can
read but not place — one in a zone that is neither UTC nor that of the machine
it runs on — and this client does not.

A withdrawal of every order carries no time: the reference client writes
none, and one stated here goes nowhere, as there. From Python, each field is
written as its text, as the reference client writes it, and an indicator that
does not read as a whole number is refused under 320 (*Error reading request:
Unable to parse field: 'Manual Order Indicator' for input string: '…'*).

## Request ids

This client keeps request ids at and above `0xC000_0000` for the questions it
asks itself, and refuses a request stated under one of them, or under a
negative id, rather than answering it. The interface this client mirrors
encourages one counter for orders and requests, and a program that keeps one
meets this once the account's order ids reach `0xC000_0000`: the next valid id
is then one no request can be stated under.

A preview asked with `what_if_order` spends no order id. The calls that answer
take their numbers from that band, and the venue's answer to a preview is not
counted as naming an order, so the ids handed out after a preview go on from
where they were.

## A calculation asked for before the model has arrived

`calculate_implied_volatility` and `calculate_option_price` are answered from
the venue's model for the contract, and asking opens the subscription that
carries it. A gateway gives up after five seconds. This client waits until the
model arrives or the call is cancelled.

## Order fields

An order carries 158 fields. 127 go out under a tag. 23 are taken and not
sent: a gateway reads each and sends nothing for it on the orders this client
places, and neither does this client. 1 is not carried by this client, and
it says so on itself rather than being quietly dropped. 6 more are what the
venue fills in on the way back, which an order being placed does not carry
out.

The 23 include the three retired instructions described below, a basis-point
offset and its kind, a bond's accrued interest, an auction strategy, a
shareholder, a parent's permanent id and the percentage constraints an order would set aside — which a gateway reads and sends nothing
for — together with the order options, the prices on a combination's legs,
the choice to decline smart routing and the kind of preview, which it checks
and sends nothing for, in the same words: an unknown option under 10337, a bad
value of `manual` under 10338, an entry not written `key=value` under 320
(*Error reading request:Please use 'Key=Value' format for Misc Options*), a
leg's price as *Prices on a combination's legs* below sets out, declining
smart routing refused under 10348 where the venue withdrew it and warned about
under 2181 otherwise — ahead of anything the venue says about the order, as a
gateway says it before the order goes out — and a preview kind other than the
ordinary one refused. A gateway reads the kind only from an
order in the protobuf encoding; the text encoding ib_async uses has no field
for it. The delta, the price randomisation and the hedging leg's
clearing, settling, short-sale and designated-location fields go out only on
order types this client does not place — a pegged-to-stock order, a
volatility order and the hedge a gateway builds for one — so on every order it
does place, a gateway sends nothing for them either. The hedging leg's short
sale is refused on an order that is itself a short sale naming a hedging
order type, where what a gateway makes of it is not established here.

A combination's routing parameters are not carried: a gateway checks them
against the combination in ways not all established here.

### Attached orders

`place_order` constructs stop-loss and profit-taking children from the selected
account preset. Rust uses `sl_order_id` / `sl_order_type` and `pt_order_id` /
`pt_order_type`; Python uses `slOrderId` / `slOrderType` and `ptOrderId` /
`ptOrderType`. State a child id with `PRESET`, compared without case. The default
integer leaves the id unstated. A child id is numbered as the order id is, so an
id from `next_order_id()` or `next_valid_id` serves a child as it serves a
parent; as with a parent, a new child under an id at or below one already used
is refused with 103. These are fields of `Order`; the call gains no argument.

The engine loads the contract and preset while unrelated requests continue.
`order_presets()` returns the list's key, attributes and last-change triples;
it does not return the separately requested values. Answers are correlated to
their request and selected key, and list changes invalidate older values.
Missing answers and venue errors are distinct from disabled attachment flags.
Smart-routing keys (`sr=`) and keys naming no selector are not selected as order
presets. The values request rebuilds its key in `sr, s, u, tc, m2, f` order and
escapes its values; the list retains the keys as the venue stated them.

The parent goes first, then stop loss, then profit taker. Children reverse the
parent's side and receive the applicable quantity, prices, parent link and
sibling OCA terms. TIF, trailing and adjusted prices, scale terms, RTH flags
and parent-trade-price offsets follow the preset and contract. A stop child
does not retain `OPG` unchanged. A contract without OCA support can omit a
collected multiple-child family while allowing the parent through.

A family with `transmit=false` stays in the engine until a parent or child
transmits it. It stays unsent at disconnect. A transmitting family finishes its
waits and goes out in order before logout. Children are built once: placing an
existing parent again changes that order, preserves changes already made to
its children and does not reload the preset. API order ids remain the ids for
callbacks, modification and cancellation even when venue ids differ. An API
number equal to another order's venue number still identifies its own order;
another client's API number does not address this client's orders. Parent
links carry the parent's current venue revision.

Initial child prices use the available order, quote and portfolio information
with the contract's price rules. There is no extra snapshot wait to choose
those prices. Transmission can subsequently wait for an ordinary quote or
mark data without recalculating the children. Changes and cancellation apply
to the held family; releasing its market-data observer leaves caller
subscriptions active. Price and trailing units accept `0` (amount), `1`
(ticks), `100` (percent), or the labels `amt`, `ticks`, `%`. Other numeric units
are unavailable.

Combination attachments use confirmed contracts, normalized legs and their
price rules. Distinct combinations retain distinct quote slots. Fresh
quantity scaling differs from replacement quantity handling: an explicit
group-allocation total becomes an integer on creation, while a replacement
preserves the quantity it states. Ratio factors, parent-price offsets and a
combination's multiplier are written to two through eight decimal places.

Refusals follow this order:

1. Attached fields are read first. `NOAPISLPTSGL` gives 320, `Error reading
   request: Attaching stop-loss or profit-taker is not allowed as part of a
   single placeOrder request, please submit such orders separately.` This
   includes an empty attached message. Otherwise a mismatched id and `PRESET`
   gives 320, `Error reading request: Invalid value for Stop Loss order-id or
   order-type`, with Profit Taker checked next. A non-`PRESET` type with no id
   is not refused merely for that type.
2. Order-field checks precede the shared contract-expiry check. An invalid
   expiry gives 10372. Empty, `NOEXP`, a valid `yyyyMM` month or `yyyyMMdd` day
   in years 1978–3000 are accepted.
3. Numbers are checked parent, profit taker, stop loss. An unassigned 0,
   `INT_MAX` or `INT_MIN` gives 10149, `Invalid order id: N`. A number no higher
   than the last placed gives 103, `Duplicate order id: N`, unless it names an
   existing order or an advanced rejection allowed reuse. Equal ids within
   the family give 103, `Duplicate order id`.
4. A requested attachment disabled in the loaded preset gives 10355,
   `Cannot auto-attach Profit Taker. Preset is not defined.`, checking Stop
   Loss next.

The advertised level remains **217**. Attached orders are constructed, but
level 218 also needs percentage-allocation sizing from positions by account
and model and the applicable allocation group. The selected account's holdings
cannot supply those quantities. Requests are not refused locally for this
limit; callers whose child quantities depend on group or model holdings must
supply explicitly sized parent and child orders. **226** is the highest level
a gateway announces; `conditionsIncludeOvernight` at that level is absent.

The complete preset-values answer and an entire attached family still need
venue confirmation. Offline tests cover the readers, construction, pricing,
identities and engine holds; they do not establish measured venue acceptance.


Two fields a gateway sends are refused by the venue by name, and this client
sends them the same way so the caller receives that answer: a caller's own
name for an algo — *"Invalid value in field # 8016"* — and how much of a
ladder's first component is already filled — *"Can not contain field #
6486"*. The second goes, as a gateway sends it, only on a ladder that steps
its price, as do the ladder's price adjustment, profit offset, restart, varied
sizes and starting position.

None is silently dropped, and that is checked rather than claimed:
`python scripts/gen_order_field_reach.py` recounts every figure from the
order builders and exits non-zero if any field becomes settable and unread.

## Retired order instructions

Both `Order` surfaces accept `e_trade_only`, `firm_quote_only` and
`nbbo_price_cap`; Python also accepts `eTradeOnly`, `firmQuoteOnly` and
`nbboPriceCap`. Their defaults are false, false and `f64::MAX` (`UNSET_DOUBLE`
in Python). An unset or non-finite cap states no instruction; zero and other
finite values do.

With `DEPRETFQNC` enabled at logon, the first stated instruction is refused
under 10268, 10269 or 10270, in that order. Otherwise each produces warning
2168, 2169 or 2170 and the accepted order goes without it. The caller's order
object keeps its values. Preview and algorithm orders follow the same rule.

The option list is read first, then the order fields are validated, then the
retired instructions are checked. A stop order with no trigger price and
`e_trade_only=true` therefore receives 403 before 10268. `NOAPIMISCVLD`
lifts option key and value checks while reading, but the later numeric check
on `manual` remains after preview and trailing-percent validation.

Declining smart routing is checked after these three instructions. If that
produces refusal 10348 after earlier warnings, Python delivers the warnings
before the refusal. Rust returns the refusal from `place_order` and queues
the earlier warnings under the order id for `Wrapper::error` on the next
`process_msgs`; a negative order id receives no queued warnings.

## A price of nought on an order

A gateway reads some prices of nought as no price and carries others as
stated, and so does this client:

- A trailing stop's starting trigger is carried wherever it is stated, nought
  and below included.
- A stock range of nought is no bound, and is not sent.
- A discretionary amount below nought is refused under 168, *Discretionary
  amount does not conform to the minimum price variation for this contract*.

A limit of nought, on a single contract or on a combination whose legs carry
no price, is sent as stated: what a gateway answers for one is not
established here.

## Prices on a combination's legs

A gateway prices a combination by its legs only where it is a non-guaranteed
combination of two legs routed by SMART. This client carries no routing
parameter that makes a combination non-guaranteed, so it places none priced
that way; the prices are checked as a gateway checks them and answered as it
answers them:

- A priced leg on any order but a limit is refused under 321, the leg named
  (*The combo details for leg '0' are invalid. - … Only LMT or REL+LMT order
  allows using per-leg prices.*).
- An unpriced leg after a priced first leg is refused the same way (*… All leg
  prices are needed when specifying per-leg prices.*).
- A limit of the combination's own beside a priced first leg is refused under
  10054, *Can't specify combo price when using per-leg prices.*
- A combination priced on every leg is refused under 10058, *Combo per-leg
  prices are only supported for non-guaranteed smart combo with two legs and
  feature "IECOMBOPERLEGPRICE" enabled.*
- Prices on later legs alone are read and not sent, as a gateway sends none;
  the combination goes at its own limit.

## A request's option list

Every request that carries a free-form option list has it checked the way an
order's is: market data, a book, historical bars, a scan, real-time bars, a
news article, historical news and historical ticks. `manual`, `0` or `1`, is
taken and changes nothing a gateway sends. Any other key is refused under
10337, another value under 10338, and an entry that is not `key=value` under
320. Two entries that could each be refused are checked in the order a
gateway's table holds them, and a key named twice keeps its last value. Where
the venue has lifted the checks at logon, a value of `manual` that is not the
number nought or one is refused under 321, after `Market data:` or
`Historical data:`; a value written in another script's digits is let
through, since a gateway reads those digits and this client does not.

The two option calculations, `calculate_implied_volatility` and
`calculate_option_price`, take no key at all: any key is refused under 10337
with no key named as valid (*Misc options key=foo is invalid in
ReqCalcImpliedVolatility(54) request. Valid keys are: *), and an entry that is
not `key=value` under 320. Where the venue has lifted the checks, any list
that reads is taken. Nothing in either list is sent: this client answers the
calculations itself, from the model the venue states for the contract. A
request for a fundamental report takes a list and a gateway reads none of it;
neither does this client, and nothing in it is checked or sent.

The Rust client takes a free-form list on an order and on `req_mkt_data_ex`,
whose final argument is `mkt_data_options: &[TagValue]`. Pass `&[]` for no
options. Python's `req_mkt_data_ex` appends `mkt_data_options=None` after
`mode_9887` and checks it as `req_mkt_data` does. From Python, `None` is no
list, and an entry that is not a tag and a value is written as its own text, as the reference client writes it — refused under 320 where that text is
not `key=value`.

The checks are the ones a gateway makes on a list written in the text
encoding ib_async uses. On the protobuf encoding the reference client moves
these requests to from level 203 on, a gateway checks only the value of
`manual`; this client makes every check whichever client a program was
written against.

## A refusal made while an order is checked names no request

Where a gateway refuses an order while it validates it — a login holding
several accounts naming none, a preview of a kind it does not take, a trailing
percentage outside what one can be, a value of `manual` that is not a number
nought or one where the venue has lifted the option checks — it answers under
321 and puts the name of the request it was validating in front of the reason.
This client answers under the same number with the same reason, without the
name. An option under an unknown key, and a value of `manual` other than `0`
or `1` where the checks stand, are answered under their own numbers, 10337 and
10338.

## The account an order is for

On a login holding one account, a gateway states that account on every order
whatever the order names, and so does this client; the open order reads back
the account the venue states. On a login holding several, the order goes out
on the account it names, and so do its replacements and its withdrawal; one
naming none is refused, *You must specify an account.* The withdrawal of an
order the venue named at connect goes out on the account the venue states it
is on. An exercise on such a login is refused naming none (*The account code
is required for this operation.*) or one the login does not hold (*Invalid
account code 'U9'.*).

A login holds several accounts, as a gateway decides it, where the venue lets
accounts be added to it as it runs, where the first account its logon names is
an introducing broker's master, or where its logon names more than one account
that is not a group. The accounts a family links to the login are not counted.
Which list a gateway counts is read from the logon, and this client counts the
accounts the logon names as the login's own; a login holding several accounts,
placing an order naming none, would settle that the two agree.

An advisor's login is not refused an order naming no account: the order is
allocated across accounts. Where an advisor's order does name one, a gateway
allocates it to that account rather than stating it on tag 1; this client
states it on tag 1, and what the venue makes of that is not established here.

An exercise naming another account on a login holding one is refused under
322, *No unlapsed position exists in this option in account* followed by the
account, which is what a gateway answers when it looks the position up there.
For the login's own account a gateway checks the position before sending, and
reduces a request for more than is held to what is held; this client sends it
and the venue answers, under 399, *You have not got the number of options
requested to be exercised*.

## An order states who it is for twice

A gateway states who originated an order on tag 6122 and nothing on 204. This
client states both: 6122 from the order's origin, and 204 as a customer's
order, which the venue once refused an order for leaving out when it stated
neither. An order placed with 6122 alone on a paper session would settle
whether 204 can go.

## Order-type names

A gateway takes each order type under several names, in any case, and so does
this client: `LIMIT` is `LMT`, `STOP LIMIT` is `STP LMT`, `PEG PRIM` is a
relative order. Four names are this client's own and a gateway does not know
them: `MIDPX`, `PEG MIDPT`, `SNAP MIDPT` and `SNAP PRI`, taken as `MIDPRICE`,
`PEG MID`, `SNAP MID` and `SNAP PRIM`. A name this client does not place is
refused under 387, *Unsupported order type:* followed by the name and that it
is not an order type this client places; what a gateway answers for a name
that is no order type is not established here.

Five order types a gateway takes are not placed by this client — trailing
market-if-touched and limit-if-touched, the retail price improvement order,
the volatility order and the pegged-to-stock order — nor are the four
volatility pegs. Each carries prices or companions not yet established here,
and is refused by name.

## Account groups and models

`reqAccountSummary` is checked as a gateway checks it and refused in its
words, before anything is taken: empty tags (*Tags cannot be null*), an empty
group (*Group name cannot be null*), and on a login that is not an advisor's
any group but `All` or `AllNonProp` (*Group name is invalid*), all under 321.
`All` is refused where the logon says the login may not ask for every account
(*ALL account is not supported*, 321) and, on a login the venue adds accounts
to, under 10200. A third summary while two are open is refused under 322, as a
gateway refuses it. The tag `All` is what asks for every figure; a group of
`All` with empty tags is refused.

An `All` summary includes every account held by the login. Named-account
updates, positions and profit subscriptions answer for the selected account.
`AllNonProp`, advisor group membership and model selection are not applied;
they are accepted with a warning once per selection and session. Multi-account
answers retain the caller's model label without applying that model.

## Executions and fills are the ones the venue restated, not the account's history

`reqExecutions()` answers with the executions this session has seen **and the
ones the venue restated when the session opened**.

The venue restates the account's recent executions at every logon. Those are filed for a
caller that asks and announced to nobody — a restarted program is answered with
the fills it made before it restarted, including fills on orders that had
already completed, which it never tracked and knows nothing else about.

A filter's `lastNDays` (1 to 7, counting today) and `specificDates` (within
the last seven days) select days the way a gateway selects them: anything
else asks for no window, a date of 8 or less is dropped, a date outside the
week is dropped and said in the log rather than refused, a date named twice
is one day, and a window that asks for no more than today is answered with
the executions held. A date that does not read as a whole number, or is not a
day of the calendar (the thirty-first of February), refuses the request under
320, as a gateway refuses it while reading it. Days are counted on the
session's time zone. What is answered from is what the venue restated at
logon, which reaches back to midnight six days before the logon in UTC (to
the logon's own day for a session set to today's executions); a gateway asks
the venue for each day the window names. So on a clock east of UTC, or a
session set to today's executions, the earliest days a window asks for can be
missing executions; the days concerned are named on `error` under 321 ahead
of the answer, which still comes. `acctCode` is ignored on a login holding
one account, as a gateway ignores it; on a login holding several, one the
login does not hold is refused.

A refused execution request is told so on `error`. The Rust client sends
nothing else, as a gateway sends nothing else; the Python client also ends it
with `execDetailsEnd`, as it ends every request it refuses, so a program
waiting on the end is not left waiting.

`reqCompletedOrders` is not in that position. It asks the venue, which answers
with what it has finished rather than with what this session watched finish,
and the call waits for that answer before handing it back.

## Contracts held at a time

Nothing here refuses a contract for want of room. The tables that hold a slot
per contract are made for 4,096 and grow past that, and a slot's entry never
moves while a caller reads it; a slot handed out reads as an empty quote until
its first tick, wherever it falls. What a caller meets is the venue's
allowance of quote lines, stated on the logon and counted against open
streams: a subscription past it is refused under 101, *Max number of tickers
has been reached* (100 lines on a paper login). Orders, fills, holdings and
news each take a slot as well, and nothing bounds how many of those a session
holds.

Slots are reused: cancelling a market-data subscription gives its slot back,
and a contract nothing holds any more frees its own.

What the company series state about a contract is kept for 4,096 contracts
past their subscriptions, the one heard of longest ago making way — a bound of
this client's own, so a scanner swept across thousands of contracts is not
kept whole for a session.

## A recovery attempt outlives the call that stops it, and opens nothing

`disconnect` stops the engine and returns. An attempt to reopen a connection
that was already dialling when it did is told to stop, and the trading
connection's attempt is stopped as a gateway stops a connection: its socket is
closed from the stopping thread, so a key exchange, an authentication or a
wait for a second factor in flight returns at once rather than waiting out its
timeout. This holds wherever recovery is taken back — a stop, a halt, a spent
budget — not only as the engine ends. What closing the socket cannot cut short
is a dial not yet connected (the host being resolved, the socket being
opened), which reads the flag as soon as its socket exists and opens nothing,
and a logon already written, which is let finish so the session it opens can
be told goodbye rather than dropped. An engine that is ending waits for the
trading attempt at most five seconds, the time a gateway gives a connection's
thread it is stopping; a session the attempt lands after that is told goodbye
where it lands.

Nothing it opens is used. The trading connection's attempt is waited for,
within that bound, and a
session that landed after the stop is logged out rather than dropped: on this
protocol an authenticated session is a session open at the venue, and somebody
may have approved a second factor for it. The other three are not waited for.
Their socket closes when the receiver that would have taken it is gone, and the
engine refuses to install a connection that arrives after the stop.

What the window costs a live login has not been measured. A paper session
presents no second factor, so nothing on this account reaches the case where it
would.

# The protocol

## The protocol is not published

This client speaks a protocol the venue does not document and can change
without notice. That is the standing risk of not running IB Gateway, and no
amount of testing removes it. What the repository does about it
is regenerate the coverage matrices from the source on every commit and fail the
build when a claim stops matching the code.
