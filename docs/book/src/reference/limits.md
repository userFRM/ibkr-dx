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

Five things are not settled:

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
- **A priced hedge child on a replace.** Where the venue prices hedge children
  itself, a gateway states tag 8262 on a new limit order — an adaptive or algo
  one included — carrying a beta or a pair hedge, and so does this client. On
  a replacement a gateway states it in cases whose rule is not established
  here, and this client states it on none.

A modify of an order this session did not place — one the venue named at
connect — is restated from the caller's own statement of it, links included:
there is no placement here to restate them from.

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

## An exercise's override

A gateway told `override` is false waits for the venue's word on where the
option stands and refuses an exercise of one out of the money, or a lapse of
one in it, under 322. It sets no bound on that wait. This client sends the
exercise as given whatever `override` says, and says so in the log, until a
bound is chosen.

## Numbered ticks this client does not deliver

Ninety-six numbered market-data ticks reach a caller of the reference client.
Eighty-seven of them reach one here. These nine do not, and each for a reason
that can be checked rather than taken on trust.

| Ticks | What | Why not |
| --- | --- | --- |
| 10, 11, 12 and the delayed 80, 81, 82 | The option model struck against the bid, the ask and the last | Not sent by the venue. A gateway works each of them out itself: for a side it takes that side's option price and finds the volatility at which the model prices the option there, then evaluates the greeks at it. The venue sends one model per option and that one is delivered, on 13 and on 83 where the feed is delayed. This client's own model now reads the venue's schedule of ex-dates and reproduces the venue's published price exactly, its delta within half a per cent and its gamma within one; what one day costs is within seven. What one point of volatility is worth is not, and the venue's own figures cannot settle how far out it is — at one strike its call and its put state vegas seven per cent apart, which is further than this client is from either. Published, these would be this client's arithmetic wearing the venue's name |
| The delayed 103, 104 | Bid and ask yield on a delayed feed | The yields themselves arrive and are delivered, on 50, 51 and 52. The delayed feed states its prices under numbers of its own and no session here has seen it state a yield |
| 85 | The moment a regulatory snapshot was taken | A gateway fills it from the venue's answer to a regulatory snapshot. This client asks for one (`regulatory_snapshot` on `req_mkt_data_ex`), and the venue refuses the request on an account without that entitlement, so no answer has been read here to take the time from |

Everything else the venue publishes and a caller can ask for is delivered, on
the callback the reference client delivers it on.

# Where this client behaves differently

Not a gap in what it can ask for. A program written against the other
behaviour will still be wrong, which is why they are here.

## Callbacks nothing fires

Three callbacks exist so a program written against the TWS API compiles and
runs, and nothing here fires them.

| Callback | Why |
| --- | --- |
| `delta_neutral_validation` | A gateway sends it. No message this client receives carries the pairing it reports, so nothing here fires it |
| `reroute_mkt_data_req` | The contract a market-data request should be asked for under instead — a contract for difference standing for a share. A gateway sends it when a request is to be asked for under another contract. Nothing this connection receives has been seen to state one, so nothing here fires it |
| `reroute_mkt_depth_req` | The same, for a request for the book rather than the quote |

Six more never fire for a program on a gateway, and fire here as often: never.
They are on [Venue behaviour](./venue-behaviour.md).

Every other call and callback on the canonical list is served on both
languages. The call-by-call matrix is [generated from the source](./coverage.md).

## A book no venue can give is asked for anyway

A gateway refuses a book asked for on no particular venue, and smart depth on a
currency, before asking the venue: 10092, deep market data is not supported for
that combination of security type and exchange. This client sends both, and
the venue acknowledges each and then answers with nothing. No error follows, so
check what came back rather than assuming a subscription that was accepted is
one that will deliver.

## Arguments a gateway acts on and this client does not need

| Call | Argument | On a gateway | Here |
| --- | --- | --- | --- |
| `cancel_mkt_depth` | `is_smart_depth` | Says whether the book is found among the smart books or the exchange books, and one naming the wrong kind is answered with 310 and leaves the book running | The request id alone finds the book. Stated as the book was asked for, both withdraw the same one |
| `req_auto_open_orders` | `b_auto_bind` | Turns binding on or off for client 0 | What binding asks for is already the default: every session is told about every order on the account, so it changes nothing. A client other than 0 is refused, as a gateway refuses one |

## Request ids

This client keeps request ids at and above `0xC000_0000` for the questions it
asks itself, and refuses a request stated under one of them, or under a
negative id, rather than answering it. The interface this client mirrors
encourages one counter for orders and requests, and a program that keeps one
meets this once the account's order ids reach `0xC000_0000`: the next valid id
is then one no request can be stated under.

## A calculation asked for before the model has arrived

`calculate_implied_volatility` and `calculate_option_price` are answered from
the venue's model for the contract, and asking opens the subscription that
carries it. A gateway gives up after five seconds. This client waits until the
model arrives or the call is cancelled.

## Order fields

An order carries 155 fields. 124 go out under a tag. 19 are taken and not
sent: a gateway reads each and sends nothing for it on the orders this client
places, and neither does this client. 5 are not carried by this client, and
each says so on itself rather than being quietly dropped. 6 more are what the
venue fills in on the way back, which an order being placed does not carry
out.

The 19 are a basis-point offset and its kind, a bond's accrued interest, an
auction strategy, a shareholder, a parent's permanent id and the percentage
constraints an order would set aside — which a gateway reads and sends nothing
for — together with the order options, the choice to decline smart routing
and the kind of preview, which it checks and sends nothing for, in the same
words: an unknown option under 10337, a bad value of `manual` under 10338,
declining smart routing refused under 10348 where the venue withdrew it and
warned about under 2181 otherwise — ahead of anything the venue says about
the order, as a gateway says it before the order goes out — and a preview kind
other than the ordinary one refused. A gateway reads the kind only from an
order in the protobuf encoding; the text encoding ib_async uses has no field
for it. The delta, the price randomisation and the hedging leg's
clearing, settling, short-sale and designated-location fields go out only on
order types this client does not place — a pegged-to-stock order, a
volatility order and the hedge a gateway builds for one — so on every order it
does place, a gateway sends nothing for them either. The hedging leg's short
sale is refused on an order that is itself a short sale naming a hedging
order type, where what a gateway makes of it is not established here.

The 5 not carried are the four fields that attach a profit taker or a stop
loss, which a gateway builds from the order preset the account holds and this
client holds none of, and a combination's routing parameters, which a gateway
checks against the combination in ways not all established here.

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

## An account summary is this session's account, whatever group is named

`reqAccountSummary` takes a group. The venue selects which accounts a summary
covers from that group and from the model code beside it, and answers with the
rows it picked. This client does neither: it filters the account stream it is
already receiving for the tags the caller asked for, and the group and the
model are taken and not applied.

For a login holding one account and no model that is the same answer, which is
the case this was written and measured against. For an advisor login where a
group names accounts beyond the one this session holds, it is not: the summary
covers the session's account rather than the group's members.

The rows themselves are encoded the same way either way. What differs is which
accounts they are for.

## Executions and fills are the day's, not the account's

`reqExecutions()` answers with the executions this session has seen **and the
ones the venue restated when the session opened**.

The venue restates the day's executions at every logon. Those are filed for a
caller that asks and announced to nobody — a restarted program is answered with
the fills it made before it restarted, including fills on orders that had
already completed, which it never tracked and knows nothing else about.

What is still absent is anything the venue does not restate. A gateway asks
the venue for the account's executions over the days a filter names, up to
seven back. This client does not ask: the filter's time is a test applied here
to what it holds, and in Python a filter stating `lastNDays` or
`specificDates` is refused (the Rust filter has neither field). So an answer
holds today's executions on this account, not its history. A program
reconciling against more than a day needs another source for it, and an empty
answer means the venue restated none and this session has seen none.

`reqCompletedOrders` is not in that position. It asks the venue, which answers
with what it has finished rather than with what this session watched finish,
and the call waits for that answer before handing it back.

## 4,096 instruments at a time

The engine holds a slot for 4,096 distinct contracts concurrently. Registering
one past that is refused with a message naming the limit.

Concurrent, not cumulative: cancelling a market-data subscription frees its slot
and the slot is reused. A long-running process that subscribes and never cancels
will reach it; one that cancels what it is done with will not.

The number is this client's own allocation, not a limit the venue states, and
it is not the one a caller meets first. The venue states how many quote lines
the account may hold on the logon, and this client counts its open streams
against that allowance: a subscription past it is refused for want of a line,
which happens well before a slot runs out.

The slot table is sized well clear of it. One option chain asked for at once is
282 live subscriptions on a single underlying, and the venue served all of
them.

## A recovery attempt outlives the call that stops it, and opens nothing

`disconnect` stops the engine and returns. An attempt to reopen a connection
that was already dialling when it did is told to stop — a flag every worker
reads between the phases of a handshake — but it is not interrupted inside one,
so it finishes whatever call it is in before it reads the flag.

Nothing it opens is used. The trading connection's attempt is waited for, and a
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
