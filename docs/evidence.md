# Evidence

What was measured, and on what. The capability table lives in
`capabilities.md`; this page is the sessions and counts behind it.

Capability status for the Rust client, the Python bindings, and the engine
underneath both. Status is assigned from a named artifact — a test, a script,
or a recorded server response — not from code inspection.

| Status | Definition |
| :---: | --- |
| ✅ Supported | Implemented; exercised against IBKR production servers; the response is parsed and delivered to the caller |
| 🔬 Implemented | Implemented and unit-tested; the request reaches the server, but the response has not been observed end to end |
| ⛔ Unavailable | Not carried by the protocol; the call returns an error stating the reason |
| ✅ Documented | A property of the venue rather than a call: established by observation against IBKR production servers, with nothing for this client to implement |

Verification runs against a paper account on IBKR production servers, and the order path has additionally been run once end to end on a funded account during regular hours. Where the venue refused a request, or acknowledged it and stated nothing, that answer is recorded as the result.

| | |
| --- | --- |
| Requests | 86. Every one either does what it says or reports why it cannot — none returns success having sent nothing |
| Order fields | 159. 128 are sent; 23 are taken and not sent, as a gateway sends nothing for them on the orders this client places; 1 is not carried by this client and the call says so rather than dropping them; 6 are what the venue fills on the way back, which an order does not carry out; 1 is acted on here rather than sent |
| Rust and Python | every canonical call and callback is on both, with the same status on each; `scripts/conformance.py --compare` holds 10 server responses to the same answer on both |
| Tests | 4,392 offline, and 191 more that live in the suites run against a broker session |

## API surface

| Measure | Count |
| --- | --- |
| Canonical calls | 87 |
| Served, Rust | 87 |
| Served, Python | 87 |
| Taken and not applied, Rust | 0 |
| Taken and not applied, Python | 0 |
| Canonical callbacks | 90 |
| Calls where the two surfaces differ | 0 |
| Callbacks where the two surfaces differ | 0 |

Counted from the source by `scripts/gen_api_docs.py` and checked against this
table by `scripts/check_status_counts.py`, both of which CI runs. A figure
here that the source stops supporting fails the build rather than standing. On
the canonical calls and callbacks the two surfaces agree: a program written
against either finds the same thing, and a call that cannot be served says so
on both rather than being absent from one. Beyond that list each surface
carries calls of its own, some spelled differently and a few on one side only;
the capability table has a column for each.

### Calls not served

Every call that exists with the expected signature and reports, through the
error callback, that it cannot be served. Taken from the generated coverage
matrix, which CI checks against the source.

None on the canonical list, and none taken and not applied either.
Configuration reads and updates sit beyond that list and always report
10357; see [Configuration requests](book/src/reference/limits.md#configuration-requests).

`set_server_log_level` is served without reaching the venue, whose protocol has
no message for it. 1 to 5 are a gateway's System, Error, Warning, Info and
Detail, and this client's logger goes to error, error, warn, info and trace. A
gateway applies the level to its own log; this client, which serves the caller
in its place, applies it to the logger it installed.
Where a program installed its own logger, that one is the program's, and the
call says so rather than reporting a level it did not set.

The advisor configuration request and its replacement go to the venue like
every other call, and the venue's reply is read and handed over on
`receive_fa`, `replace_fa_end` and the error callback. The content of that
reply needs an advisor account to see, and the status row below says so.

## Test inventory

| Suite | Count | Requires credentials |
| --- | ---: | :---: |
| Rust unit and integration | 3,211 | No |
| Rust, live | 9 | Yes |
| Python | 1,181 | No |
| Python, live | 131 | Yes |
| Paper compatibility suite (154 phases) | 51 tests | Yes |

Counted rather than stated: `scripts/check_status_counts.py` names every test
in each suite and fails the gate when this table disagrees with it, so a figure
here cannot go quietly out of date as the suites grow.


42 of the 45 capabilities are verified against IBKR production servers. Of the
other three, advisor configuration reaches the server and needs an advisor
account to see what it answers, two order types are checked by the offline
suites only, and session lifetime is a property of the venue rather than a call
this client makes.

Every figure above is measured on each commit, and the build fails if one moves.

---

With the `test-helpers` feature, Python's `_test_set_enabled_features(features)`
replaces the simulated session's enabled-feature list. It is absent from
ordinary wheels.

## Client surfaces

| Surface | Status | Verification |
| --- | :---: | --- |
| `EClient` / `EWrapper` (TWS API shape) | ✅ Supported | `tests/ib_paper_compat`, `tests/python/test_compat_tier1..3.py` |
| Gateway settings | ✅ Supported | 17 settings carried, 15 recorded as not settings here, both lists the same on either client; `tests/python/test_gateway_settings.py`, `tests/python/test_settings_parity.py`; session opened under a stated build and time zone |
| Rust/Python equivalence | ✅ Supported | 4 static gates (settings, order fields, surface, error behaviour) plus `scripts/conformance.py --compare`, which compares 10 server responses across both clients |

## Market data

| Capability | Status | Verification |
| --- | :---: | --- |
| Top of book | ✅ Supported | Streaming and snapshot; US equities and FX; `scripts/sdk_sweep.py`. Concurrent subscribers on one contract share one wire subscription |
| Market depth (L2) | ✅ Supported | A book is asked for once, at the venue named, and every level names it; inserts, updates and deletes are delivered as the venue states them. Which venues answer depends on the account's entitlements — see the note below. `tests/python/test_live_depth.py`, `src/bin/capture_depth.rs` |
| Historical bars | ✅ Supported | 9 markets in one session (`src/bin/capture_global.rs`); `keepUpToDate` verified in `tests/python/test_historical_and_scanner.py` |
| Historical ticks and schedules | ✅ Supported | `scripts/sdk_sweep.py`; unsupported tick types return an error rather than substituting another series |
| Tick-by-tick quotes | ✅ Supported | FX and US equities, concurrent streams, each record carrying its request id; `tests/python/test_live_python_wrappers.py` |
| Tick-by-tick trades | ✅ Supported | 67,785 trades over a 20-minute session; 327 in the first twenty seconds of one subscription. A stream is asked for by the venue's id for the contract, which is resolved first when the caller states a description, and by the name the caller used: `Last` and `AllLast` are two queries the venue answers apart, and which trades are on which is the venue's to say — measured on a liquid future, 104 and 139 records over two windows of the same length |
| Real-time bars | ✅ Supported | Five-second bars streaming during regular hours, each carrying open, high, low, close and volume, alongside a book on the same session; `tests/python/test_live_python_wrappers.py::TestFiveSecondBars` |
| Trading halt status | ✅ Supported | Tick 437 decoded from status mask and status index; `src/bin/capture_status.rs` |
| Tick attributes | ✅ Supported | Per-trade `unreported` and `pastLimit`, read from the marks the venue writes beside a print, not from the size. A frame the venue sent is kept in the tests: on it no mark is set and the sizes are 1, 2, 1, 4, 1 |
| Venue map behind the exchange mask | ✅ Supported | Asked for beside the quote and answered at regular trading hours with 18 venues, each with the letter the mask's bits refer to. Outside those hours the venue states none, to a gateway as to this client, and a venue's letter is empty until it does |

**Depth.** On this account IEX answered, with 227 levels on one SPY
subscription; NASDAQ and CME refused by name, and the refusal reached the
caller. A book asked for on no particular venue was acknowledged and produced
nothing.

## Orders

| Capability | Status | Verification |
| --- | :---: | --- |
| 24 order types | ✅ Supported | The check every placement passes accepts 24, each sent as itself: MKT, LMT, STP, STP LMT, TRAIL, TRAIL LIMIT, MOC, LOC, MIT, LIT, MTL, MKT PRT, STP PRT, REL, PASSV REL, PEG MKT, PEG MID, PEG BEST, PEG BENCH, MIDPX, SNAP MKT, SNAP MID, SNAP PRI and BOX TOP. Every one but PEG BEST and BOX TOP is placed against the venue by `tests/ib_paper_compat` or the Python live suites |
| PEG BEST and BOX TOP | 🔬 Implemented | Built and checked by the order builder's offline tests, `src/engine/hot_loop/order_builder/tests.rs`; no suite here places them against the venue |
| Order fields | ✅ Supported | An order has 159 fields. 128 are sent. 23 are taken and not sent: a gateway reads them and sends nothing for them on the orders this client places, and neither does this client. 1 is not carried by this client, and it says so on itself rather than being quietly ignored. 6 more are what the venue fills on the way back, which an order does not carry out. One is acted on here rather than sent: an order held back is kept until one in its family transmits, which is what a gateway does with it. A check on every commit fails if a field starts being dropped |
| Non-US markets | ✅ Supported | Previews accepted on DE, NL, GB, CH, AU, CA, US equities and FX; JP and HK rejected for lot size, which is the exchange rule and is surfaced to the caller |
| Modify, cancel, global cancel | ✅ Supported | `scripts/sdk_lifecycle.py` (place → modify → cancel) and `scripts/order_round_trip.py` (a limit far from the market on a contract that trades nearly around the clock: placed, repriced, withdrawn); `tests/ib_paper_compat` Phase 9 / 9b |
| Brackets, OCA, combos | ✅ Supported | Per-leg pricing; leg order validated by server rejection of the inverted spread; `src/bin/capture_combo.rs` |
| Conditions | ✅ Supported | All 6 types (price, volume, percent change, margin, execution, time) accepted and held by the server; `tests/ib_paper_compat` Phase 60 |
| Order acceptance | ✅ Supported | Every change to an order answers with the order as this client sent it and the status it is now in, which is the pair the protocol answers a change with. 45 orders placed, modified and withdrawn over a 15-cycle session, every one reaching Cancelled, with no error |
| Executions and fills | ✅ Supported | Fill reported and position reconciled; execution report retains server fields including unnamed tags; `tests/ib_paper_compat` Phase 97 |
| A round trip on a funded account | ✅ Supported | One same-day option bought and sold on a funded account during regular hours: limit in, filled, limit out, position and account values reconciled, and nothing left open. Run once by hand rather than by a phase, a funded account being one the suite may not trade — so unlike every row beside it this one records a session rather than something a reader can repeat |
| Option exercise and lapse | ✅ Supported | Both submitted for a resolved option contract; server response 399 *"You have not got the number of options requested to be exercised"* delivered to the caller; `tests/python/test_option_greeks_stream.py::test_an_option_not_held_is_neither_exercised_nor_lapsed` |

## Account

| Capability | Status | Verification |
| --- | :---: | --- |
| Account values and summary | ✅ Supported | 135 values, each in the currency the server states; subscribed at connect; `tests/python/test_account_updates_and_pnl.py` |
| Positions and P&L | ✅ Supported | Delivered on login and after each fill; `tests/python/test_account_updates_and_pnl.py` |
| Managed accounts | ✅ Supported | `tests/python/test_account_updates_and_pnl.py` |
| Financial advisor configuration | 🔬 Implemented | Request reaches the server on both clients; the response requires an advisor account |

## Reference data

| Capability | Status | Verification |
| --- | :---: | --- |
| Contract details | ✅ Supported | 12 lookups across 9 countries: 11 resolved to one contract, and the 12th matched 2 and was refused as ambiguous rather than resolved to one, as ib_async's `qualifyContracts` declines to pick one; `src/bin/capture_global.rs` |
| Option chains, symbol search | ✅ Supported | `scripts/sdk_sweep.py` |
| Scanners, fundamentals | ✅ Supported | 697 KB scanner parameter set, `tests/python/test_historical_and_scanner.py`; fundamental report, `tests/rust_api_live.rs::the_calls_no_other_live_test_names` and `tests/ib_paper_compat` Phases 83 and 93 |
| News | ✅ Supported | 117 providers parsed. Headline retrieval requires a news subscription; this account holds none, and every provider returns an empty result set |
| Exchange directory | ✅ Supported | 203 exchanges, in the two sections the venue states them in: shares and derivatives. What each carries and which group each aggregates into are not stated by the venue and are not stated here |
| Corporate events calendar | ✅ Supported | 43 event types with their field schemas, 179 KB, over the security-definition connection; an event query is answered with a well-formed result and can be withdrawn. Event content needs a subscription — see the note below. `src/bin/capture_calendar.rs`, `tests/python/test_live_python_wrappers.py::TestCorporateEventsCalendar` |
| Implied volatility, option price | ✅ Supported | The venue computes the model and publishes it per option on a subscription of its own, beside the volatilities it states it from, and the model tick a caller is given is built from those as a gateway builds it. The bid's, the ask's and the last's are worked out with an option model of this client's own the way a gateway works them, from the chain parameters' underlying price where a gateway takes the underlying's own quote when it holds one; they have not been compared with a gateway's. A hypothetical the caller supplies — a price, or a volatility — is solved against that model in the engine, as a gateway solves it, and is held until the model is published; solved here it reproduces the venue's price to the cent on 2 contracts. `src/bin/capture_option_model.rs`, `tests/python/test_option_greeks_stream.py::test_the_calculations_are_answered_from_the_venues_model` |

**Corporate events data.** Event content requires a Wall Street Horizon
subscription; this account holds none, so every query — by contract and by
filter — is answered with an empty set. The schema itself is delivered either
way. A query is one message and one answer, so a withdrawal cancels the answer.

---

## Invariants

Three properties are measured on every commit, and the build fails if one
stops holding.

| What is guaranteed | Where it stands |
| --- | --- |
| A call never returns success having sent nothing | 86 requests, none silent |
| A field a caller sets is never quietly ignored | 159 order fields, none dropped |
| A field the server sends is never thrown away | What this client has no name for is kept under its tag number — 49 such fields on an equity definition, 46 on a bond |

A fourth is held by a test rather than a measurement: **no wire parser aborts
on malformed input.** Every parser is given each prefix of a well-formed frame,
that frame with a byte replaced at each position, and runs that are not frames
at all (`tests/malformed_input.rs`).

## Constraints

What a request meets here. Each says whether it is the protocol's or this
client's own allocation; what a gateway answers the same way is on
[Venue behaviour](https://userfrm.github.io/ibkr-dx/reference/venue-behaviour.html).

- **Two order fields a gateway sends are refused by the server by name.**
  `algo_id` → *Invalid value in field # 8016*; `scale_init_fill_qty` → *Can
  not contain field # 6486*. This client sends both, as a gateway does, and the
  caller receives the server's refusal.
- **One market-data subscription per contract on the wire.** The server holds
  one subscription per contract for a session, so callers asking for the same
  contract are multiplexed client-side and each is served from it.
- **A caller's request id is not what the venue is asked under.** Every
  subscription is asked for under an id this client allocates and is mapped
  back to the caller who wanted it. The venue echoes an id back, so one taken
  from the caller cannot be told apart from one allocated here. This client
  allocates the same way, from one upward, and keys its subscriptions on it.
- **A request id states four bytes, and the top quarter of that range is this
  client's own allocation.** A caller numbers requests below `0xC000_0000`; at
  and above it are the ids this client allocates for the questions it asks on a
  caller's behalf, and above `0xF000_0000` the ones the engine asks for itself.
  A negative id is refused, and so is one inside the range, by number rather
  than answered, because answered it would be indistinguishable from one of
  this client's own and its reply withheld. An order may be numbered wider —
  the venue takes a wider one — but an order id reused as a request id has to
  satisfy this, and the interface this client mirrors encourages one counter
  for both.
- **`keepUpToDate` queries are closed on first response.** Continuation is
  provided by folding the 5-second bar stream into the requested bar size.
  Daily updates retain the session bounds supplied with the history and use
  its end date in the series' timezone. When that session ends, the next one is
  the contract's own session holding the next five-second bar, dated the same
  way; UTC calendar boundaries are used only while the contract's sessions are
  not in hand. A contract whose definition no lookup has stated is looked up
  first, as a gateway looks up a request's contract, to ask for them. A week and a month are folded on the calendar, opening on the Monday and on
  the 1st at midnight UTC, and start from the stream rather than from the
  venue's current bar.
- **The option-exercise interest rate series is not served.**
  `OptExInterestRate` is accepted as a tick query against an option contract
  and rejected by name against the underlying, and every window tested returns
  an empty result set.
- **A crypto's trade stream delivers and its book does not.** Measured on 13
  September: the tick-by-tick `AllLast` stream on BTC on PAXOS delivered 61
  records in 30 seconds, and the pair's book delivered nothing on an account
  with no crypto depth subscription.

## Sessions

A login holds one session at a time, and each program opens its own. The venue
states this at connect, names the session already holding the login, and
states when that session logged in.

| Behaviour | Status | Verification |
| --- | :---: | --- |
| The session holding the login is reported to the caller | ✅ Supported | `competing_session()` returns the address, the logon time, and whether this session may trade. A read-only flag from the venue is carried as stated |
| A logon later than this one is another session, and keeps the login | ✅ Supported | A reconnect that finds one reports it and stops; retrying cannot change it. Both times are read from the venue's clock, so two machines' clocks cannot decide it |
| A logon at or before this one is this session's own, still being reaped | ✅ Supported | The reconnect completes over it, which is what an ordinary recovery is |
| The heartbeat is the interval the venue answered with | ✅ Supported | The interval a logon proposes is not what it is held to; the answer is read from the logon response and applied on every reconnect |
| A reconnect follows the venue | ✅ Supported | It uses the hosts this session reached the venue through, on the port the venue named in its redirect, and stops walking hosts when one answers and refuses |
| The first connect knocks on the next door when one does not answer | ✅ Supported | One host per region. A door that answers and refuses ends the walk, so a refused logon is not repeated at every door |
| An order id is counted from what the account is working | ✅ Supported | An order id belongs to the account, not the process, and the venue refuses one while the order under it is still working. Nothing is kept on disk: at each connect the venue replays what the account is working, and ids count from one past the highest of those — from one when nothing is working |
| A session survives losing its connection | ✅ Supported | A dropped connection is rebuilt on the session already open, with no second factor: five forced drops recovered in 2-8s, and an eight hour session rode through its losses unattended |
| A session does not survive its process | ✅ Documented | The venue holds a session for a socket, not for an account: killed without logging out, it was already gone forty seconds later, and a later start is answered with a handshake. A session is therefore bound to the socket that opened it, so a restart is a fresh logon and an uninterrupted session is not. What that costs an account with a second factor has not been measured here; a paper session presents none |
| A session that has ended answers at once | ✅ Supported | Requests made after a terminal loss are refused with 504 immediately, rather than waiting out a timeout each. Every request already answered keeps the venue's answer |

One session, held open for 175 minutes across a market open: 106,053 quotes,
180,433 trades, 95,985 book rows and 4,148 bars, with no unrequested
disconnect and no error other than the venue's answer for a series it does
not hold. `scripts/endurance.py --minutes 175`.

That check takes every subscription out and puts it back each cycle, which no
other check here does, and it is the only one that sees what a long-running
program sees. Run it before believing a change to the quote path: it requires
a book and a trade stream to arrive at least once. Outside the hours US shares
trade, the currency pair's book on `IDEALPRO` has been seen to arrive, and the
crypto pair's trade stream delivers at any hour (see the constraint above), so
neither check waits on US shares trading. On the account measured, a book was
refused by name on some venues and acknowledged with nothing on others; both
read from here as a stream that did not arrive.

## Architectural differences from a gateway process

| Gateway | This client |
| --- | --- |
| Configuration file and settings window | Settings on the client, read back at runtime. 15 gateway settings are not settings here, each saying why or naming what stands in for it: no window geometry, no local listening socket, no JVM heap, no message pacing (a gateway paces by default), and a bar dated on the zone the venue names beside it |
| Local socket for client programs | The client is in-process; there is no socket to connect to, authorise, or keep running |
| Java runtime | None |

## Known limitations

These are this client's own.

- **1 order field is not carried by this client.** Combination routing parameters: A gateway checks them
  against the combination in ways not all established here; they are refused
  when stated.
- **Attached orders are constructed; percentage-allocation sizing is incomplete.**
  The selected account preset supplies the children. Quantities depending on
  group or model holdings require explicitly sized parent and child orders.
  The advertised level stays 217; 226 is the highest a gateway announces.
  Offline tests cover preset loading, construction and engine holds; a complete
  preset-values answer and attached family still need venue confirmation.
  See [attached orders](book/src/reference/limits.md#attached-orders).
- **An exercise is sent without the moneyness check a gateway makes when
  `override` is false.** A gateway waits for the venue's word on where the
  option stands and refuses an exercise out of the money or a lapse in it.
  This client does not ask for that word: it sends the exercise as given, and
  says so in the log where `override` is false.
- **A preview under the number of a working order is refused.** A gateway
  prices it as a new order and leaves the working one alone; this client keys
  both on the number, and does not yet keep the two apart.
- **The order types a contract takes on its exchange are not checked before an
  order goes.** A gateway refuses a change of type into one the contract does
  not take, and a midpoint or best peg where the exchange takes neither; this
  client leaves both to the venue, because how the definition keys its lists
  of order types to each exchange is not yet established here.

- **The carry term in a hypothetical solve is fitted, not read.** The series
  that states it is the option chain's model parameters, 687, asked for on the
  option model's name for the underlying: per expiry it states a model yield,
  an interest rate, a forward and the at-the-money volatilities. A gateway
  reads it for its own option model and never forwards it to a program; this
  client reads it and hands it to a caller (`chain_model_parameters`). The
  venue states no unit for the yield and the rate, and the solve does not use
  them while none is established: a caller-supplied price or volatility is
  solved against a carry fitted to the venue's model price rather than one the
  venue stated. It absorbs whatever else the two models disagree about: two
  contracts on one underlying and expiry fitted 4.9% and 20.1%. It does not
  affect the volatility or the greeks a caller reads, which are the venue's and
  are not solved here.

## Refusals

A request this client will not send is reported on the error callback, and the
call returns rather than raising. In Python that is
`error(reqId, errorTime, errorCode, errorString, advancedOrderRejectJson)`; in
Rust, `Wrapper::error(req_id, error_code, error_string, advanced_order_reject_json)`.
The code is the one the TWS API defines for that class: 321 for a request that
fails validation, 200 for a contract description that matches nothing, 504 for
a call with no session, and 327 for a client other than 0 asking not to bind
orders entered elsewhere; asking to bind them, that client fails validation, as
on a gateway. Construction and configuration raise, as does a synchronous call
with a return value.

## Calls

| Category | Calls |
| --- | --- |
| **Connection** | `connect`, `disconnect`, `is_connected`, `run`, `get_account_id` |
| **Market data** | `req_mkt_data`, `cancel_mkt_data`, `req_tick_by_tick_data`, `cancel_tick_by_tick_data`, `req_mkt_depth`, `cancel_mkt_depth`, `req_market_data_type` |
| **Orders** | `place_order`, `cancel_order`, `req_global_cancel`, `req_ids`, `req_open_orders`, `req_all_open_orders`, `req_auto_open_orders`, `req_completed_orders`, `req_executions` |
| **Account** | `req_positions`, `cancel_positions`, `req_positions_multi`, `cancel_positions_multi`, `req_account_summary`, `cancel_account_summary`, `req_account_updates`, `req_account_updates_multi`, `cancel_account_updates_multi`, `req_pnl`, `cancel_pnl`, `req_pnl_single`, `cancel_pnl_single`, `req_managed_accts` |
| **Historical** | `req_historical_data`, `cancel_historical_data`, `req_head_time_stamp`, `cancel_head_time_stamp`, `req_historical_ticks`, `req_historical_schedule`, `req_real_time_bars`, `cancel_real_time_bars`, `req_histogram_data`, `cancel_histogram_data` |
| **Reference** | `req_contract_details`, `req_matching_symbols`, `req_sec_def_opt_params`, `req_mkt_depth_exchanges`, `req_market_rule`, `req_smart_components` |
| **Scanner** | `req_scanner_parameters`, `req_scanner_subscription`, `cancel_scanner_subscription` |
| **News** | `req_news_providers`, `req_news_article`, `req_historical_news`, `req_news_bulletins`, `cancel_news_bulletins` |
| **Fundamental** | `req_fundamental_data`, `cancel_fundamental_data` |
| **Options** | `calculate_implied_volatility`, `cancel_calculate_implied_volatility`, `calculate_option_price`, `cancel_calculate_option_price`, `exercise_options` |
| **Other** | `req_current_time`, `req_user_info`, `req_family_codes`, `req_soft_dollar_tiers`, `set_server_log_level`, `req_wsh_meta_data`, `cancel_wsh_meta_data`, `req_wsh_event_data`, `cancel_wsh_event_data`, `query_display_groups`, `subscribe_to_group_events` |

**Order types.** The 24 the check every placement passes accepts: MKT, LMT,
STP, STP LMT, TRAIL, TRAIL LIMIT, MOC, LOC, MIT, LIT, MTL, MKT PRT, STP PRT, REL,
PASSV REL, PEG MKT, PEG MID, PEG BEST, PEG BENCH, MIDPX, SNAP MKT, SNAP MID,
SNAP PRI and BOX TOP. PEG MIDPT, MIDPRICE, SNAP MIDPT, SNAP PRIM and PEGBENCH
are accepted as other spellings of five of them. Algos: VWAP, TWAP, Arrival
Price, Close Price, Dark Ice, PctVol. Conditions: price, volume, percent change, margin, execution
and time. Brackets, one-cancels-all, and combinations with a price per leg.

**Settings.** The gateway's configuration file is replaced by settings on the
client: announced build, time zone, execution-report scope, and others — 17 in
total, readable at runtime. Fifteen gateway settings are not settings here, and
each says why or names what stands in for it (no window geometry, no local
listening socket, no JVM heap, and no message pacing: a gateway paces requests
at the rate its logon states (fifty a second where it states none) unless it is
set to reject them instead, and nothing here does either).
Rust: `EClientConfig.gateway`. Python: `ibkr_dx.configure()`.
`order_id_file` retains the next ID per account and API client across restarts;
reservations share an exclusive file lock. See [order IDs across sessions](book/src/reference/venue-behaviour.md#order-ids-across-sessions)
for the location, configuration and storage-failure behaviour. Attached children
share the known parent's cancellation group, a rejected revision keeps the
original order's accepted terms, and replacements retain automatic hedge
pricing. An order is stated to a program as a gateway states it: nothing while
the venue has said nothing — a market-on-close order in regular hours draws no
report until it is withdrawn — and, asked for, under its type's own name, on
the account it went out for, with the number it went out under as its
permanent id.
