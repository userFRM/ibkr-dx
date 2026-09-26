# Requests, delivery and shutdown

Requests and cancels return after admission. The engine holds work while it
waits for contract naming, order replay, account download, an option's model,
or an earlier exchange to finish. A cancel withdraws its own kind of request,
including one still waiting, except `cancel_contract_data` and
`cancel_historical_ticks`, which withdraw nothing, as on a gateway (see
[Venue behaviour](venue-behaviour.md)). Cancelling a question that is ready follows its
answer; cancelling one still waiting prevents that answer. A request for a
contract the venue names no single contract for is over, as it is at a gateway:
its number holds nothing, a cancel under it is refused as one of nothing held,
and it can be asked under again.

## Requests and callbacks

Rust request and cancel methods return `()`. Their refusals arrive on
`Wrapper::error_from` or `Wrapper::error`. Request methods take no wrapper:
pass the wrapper to `process_msgs` to receive their answers. The answering
conveniences, such as `historical_data`, `contract_details` and
`qualify_contract`, wait and return their results.

Records are delivered in session order. Refusals, immediate answers, connection
notices, registrations and order reports share that order. Quotes, positions,
account figures and other conflated state follow the records of a read. A
record arriving during a read waits for the next read. A fill and its status
from one report are delivered as `order_status`, then `exec_details`; the
commission report follows where the venue states it. It carries the realized
P&L and a bond's yield where the venue states them beside the charge, and the
unset value where it states none or, for the P&L, zero.

Python request refusals also wait for `poll()` or `run()`. A 504 about a feed or
trading connection that ended within an admitted session follows the earlier
records. With no session, “not connected” is reported inside the call, as in
the reference client.

A Rust answering convenience with `keep_record` delivers a whole read through
that record and its answer collector. Without a kept record, it takes only its
own answers and leaves other callbacks for `process_msgs`.

## Error origins

Every error reaches `error_from`. Its default forwards to the `error` callback,
so a wrapper that implements only `error` receives every error there. Rust
`ErrorOrigin` distinguishes:

| Origin | Meaning |
| --- | --- |
| `Request { id, ends }` | A numbered request; `ends` distinguishes a refusal from a notice followed by more answers |
| `Order { id, op }` | An order's placement, modification, cancellation, exercise, or a venue report |
| `Question { q, ends }` | A request without an id, such as positions, open orders or scanner parameters |
| `Session` | A connection notice or another session-wide outcome |
| `Internal(id)` | An engine lookup |

Python `ErrorOrigin` exposes `kind`, `id`, `ends`, `op` and `question`.
`ends` is `None` for origins other than requests and questions; `op` and
`question` are `None` when inapplicable. Ordinary ibapi wrappers receive
`error` with the original number and code, with the arguments the wrapper's
`error` declares: `reqId, errorCode, errorString` as ibapi 9.81 calls it,
`advancedOrderRejectJson` after them as the releases before `errorTime` did,
or `errorTime` second as the current release does. An `error` declared with
three or four is called directly, without `error_from`.

A call that answers asks under a number this client took for that call. What
answers it after the call has returned — the end of a lookup that was refused,
an answer that came after the wait ran out — reaches no wrapper on either
surface.

`refuse(origin, code, msg)` adds a caller-side refusal to the same stream, on
both surfaces. Python makes the origin from the fields it reads back:
`ErrorOrigin(kind, id=-1, ends=True, op=None, question=None)`.
`question_retired(Question)` marks the ordered cancellation of positions
or account updates; its default does nothing. Python delivers no cancellation
callback for these questions.

## Shutdown

Python `disconnect()` finishes the engine's wire work, calls `connectionClosed`
before returning, and discards queued callbacks.

Rust `disconnect()` returns `Shutdown { logout_sent, engine }`. The engine
result is `EngineEnd::Ended` or `EngineEnd::Panicked`; either confirms that the
engine thread ended. Concurrent callers wait for the same end. Call
`process_msgs` afterwards to receive the remaining records and final state,
followed by `connection_closed`. Nothing from that session follows the close.
Orders already admitted finish before logout or are refused under their order
numbers. Orders kept with `transmit = false` are not sent.

`EClientConfig.cancel` can take back a connect during socket operations,
second-factor polling and client construction. Name resolution and a supplied
code provider must return before cancellation completes. Shutdown waits for
recovery workers to finish, including those calls, and logs out a session that
opened while cancellation was underway.

## Admission and wakeups

`backlog()` counts admitted commands that have not finished, including held
work and unsent orders. Work kept for `transmit = false` or a model value stays
counted. A global cancellation is one command even when it covers many
contracts. Admission uses an unbounded channel; released work and new commands
share a limit of 64 commands per engine lap. A caller that limits admission
must still allow the transmitting order or cancellation that releases its
staged orders.

Rust `on_data(Some(hook))` installs a wake hook; `None` removes it. The hook runs
on the engine thread, outside engine locks, after a record or state change, at
most once until the next `process_msgs` begins. It must return immediately and
must not call `disconnect` or drop the last client owner. A panicking hook is
logged and removed. Bound an idle wait when relying on completions whose
deadlines are evaluated by `process_msgs`, such as snapshots and smart
components.

Both surfaces expose `traffic()`: `bytes_sent`, `bytes_received`,
`messages_sent` and `messages_received` for the session. Bytes are protocol
frames before TLS encryption and after decryption. Complete frames count as
messages, including one message per compressed outer frame. Initial
authentication is outside these counts; replacement connections preserve them.

## Order IDs

Both surfaces state `next_valid_id` once a session has connected, after the
venue has named the orders the account is working: Python before `connect()`
returns, Rust on the first `process_msgs` read after that naming.

Rust `order_id_floor()` reads the ID `next_valid_id` would state, without
waiting; `next_shared_id()` answers the request-compatible one.
`next_shared_id_within(timeout)` adds a bound to the replay wait and observes
`EClientConfig.cancel`.
`next_order_id()` is an answering call; an exercise
that asks the engine to assign its number returns immediately and is numbered
after replay.

The order-id file keeps a separate next ID for each account and API client ID
across sessions; the `order_id_file` setting selects it. See
[order IDs across sessions](./venue-behaviour.md#order-ids-across-sessions)
for its default location, reservations and moving the file.

## Settings and request values

There are 17 carried settings, on Rust `EClientConfig.gateway` and Python
`ibkr_dx.configure()`, and 15 settings recorded as inapplicable here.
Registration is held by the engine and does not wait on a caller thread.

Historical news takes signed `total_results: i32`. Values above 300 are sent as
300; zero and negative values pass through. Historical ticks carry
`ignore_size`. Order-condition margin percentages and price trigger methods
are signed `i32` and are sent unchanged.

Rust `req_scanner_subscription` takes a final `scanner_setting_pairs: &str`;
pass `""` when none are stated. Both surfaces accept these pairs and warn once
that they are not carried to the venue.

## Accounts, exercises and snapshots

Named-account updates, positions, profit and multi requests keep the account
they name. An `All` summary includes every account held by the login.
Concurrent profit requests have separate subscriptions. Multi answers echo
the requested model label on the initial and subsequent rows. Model selection,
`AllNonProp` membership and advisor groups are not applied and produce a
warning once per selection and session.

An exercise requires a positive position in the selected account. It requests
tick 493 and waits for its in-the-money figure when necessary, even with
`override`. Without override it applies the exercise/lapse check. Its quantity
is limited to the whole-contract position captured before waiting. Shutdown
refuses an exercise still waiting for that figure. The alternate exercise
transport is not implemented.

Snapshot registration follows contract naming, including options given only
by contract id. Its eleven-second bound starts when the engine takes the named
subscription. The snapshot waits for bid, ask, last, open and close; OPT, FOP,
IOPT, WAR and EC also wait for model computation 13 and the bid's, ask's and
last's computations 10 to 12, or 83 and 80 to 82 for delayed data. Delayed
snapshots also wait for tick string 88. Each computation is sent to a snapshot
once and only with all eight figures stated; at the end it is sent each one it
has not been sent where any figure is stated. On a frozen or delayed-frozen
feed the model is also sent once whatever it states, and at the end a side only
with all eight figures stated and the model again where it was not sent that
way. A snapshot of an option on an index or a future, or of one whose definition
the venue has not answered, is sent no 10 to 12 here and so runs the full
eleven seconds.

## Lower-level Rust users

`from_parts`, engine constructors and core command senders use
`std::sync::mpsc::Sender<ControlCommand>`. Order, market-data and question
commands carry their inputs to the engine. `MarketDataTaken` carries the slot
generation and registration time. Order records share immutable cached reports.
`Ask` and `Answer` multi-account variants carry the account and model label.
A global cancellation is one order-buffer request. An automatically numbered
exercise carries its caller's shared allocator until replay completes.

`SharedState.portfolio` is an `Arc`, and named accounts have separate stores.
Profit request and cancellation commands distinguish account and single-position
subscriptions. `Connection::new` takes `TlsStream<LogonSocket>`; explicit socket
construction wraps the TCP stream with `LogonSocket::new`. Established
connections keep their ordinary timeouts.

Per-client order views, outcomes for children of refused placements and changes
behind a refused or withdrawn placement, complete option greeks, account-group
and model application, alternate exercise transport and scanner settings-pair
carriage are limited as [Limits](./limits.md) describes.
