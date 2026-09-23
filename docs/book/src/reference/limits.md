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

## One order a modify cannot restate

A modify is a full statement of the order, rebuilt from what was placed. One
kind cannot be stated that way and the call is refused rather than sent: a
what-if preview, which is a margin preview rather than a resting order, so
there is nothing on the book for a replace to act on.

Everything else is replaced as itself, including the ones that carry more than
a type and a price: hidden, all-or-none, iceberg, discretionary, sweep-to-fill,
an OCA group, a good-till date, a bracket child, an algo, a conditional order,
an adjustable stop, and the trailing, pegged, midpoint and limit-if-touched
types. An adjustable stop is an ordinary stop defined by what it becomes, and
the replace states that conversion again: the marker, the type it adjusts to,
the trigger, the adjusted stop, its limit and the trailing amount, the same
numbers the placement wrote and in the same order. A modify of one
of those names its defining number in the field the placement used: a trail, a
peg offset or a snap offset on the auxiliary price, a cap on the limit price, a
trailing stop limit's limit offset on `lmtPriceOffset`. The one number a modify
cannot move is a trailing percent, and a modify naming a new one is refused
rather than sent. A modify of an order this session did not place — one the
venue named at connect — is restated from the caller's own statement of it,
which is what the reference client sends — links included, on every replace,
so a statement that omits the group drops it at the venue whatever the type,
as it does on the reference client. A change of type on an order with a
parent or a group is refused: the replace carries neither across a change of
type, and the venue reads their absence as their removal. A replace stating a
parent or a group other than the one an order placed here was placed with is
refused too, for the same reason.

A relative order is refused a modify as well. It answers both ways: sometimes
the venue takes the replace and the order goes on working, and sometimes
neither the replace nor a withdrawal after it draws any answer at all. A modify
that strands the order some of the time is worse than one that is refused.

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

## Order fields the protocol has nowhere to put

An order carries 154 fields. 118 go out under a tag. 29 have no field in this
protocol to carry them, and each says so on itself rather than being quietly
dropped. 6 more are what the venue fills in on the way back, which an order
being placed does not carry out.

The 29 are not a gap in this client. The protocol numbers 288 order fields, and
not one of them carries a basis-point offset, a bond's accrued interest, an
auction strategy, an origin, a shareholder, a smart-combo routing parameter, a
scale table, a price randomisation, a what-if kind, a parent's permanent id, a
bracket preset's legs, the hedging leg's clearing, settling, short-sale or
designated location, or the percentage constraints an order would set aside.
They are fields a caller can state and nothing on the other side can receive,
which is the same answer a gateway gives.

One of them was settled the other way round, on a session rather than on the
vocabulary: a caller's own name for an algo has a number in the protocol, and
the venue refuses an order carrying it — *"Invalid value in field # 8016"* —
whether or not the order runs an algo.

None is silently dropped, and that is checked rather than claimed:
`python scripts/gen_order_field_reach.py` recounts all four figures from the
order builders and exits non-zero if any field becomes settable and unread.

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
