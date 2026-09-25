# Python API Reference (v0.1.0)

*Auto-generated from source — do not edit.*

## Table of Contents

- [EClient: Connection](#connection)
- [EClient: Calls That Answer](#calls-that-answer)
- [EClient: Account & Portfolio](#account--portfolio)
- [EClient: Orders](#orders)
- [EClient: Market Data](#market-data)
- [EClient: Reference Data](#reference-data)
- [EClient: Gateway-Local & Stubs](#gateway-local--stubs)
- [EWrapper Callbacks](#ewrapper-callbacks)

## Connection

#### `__init__`

Bind the wrapper the callbacks are delivered to.  `EClient(wrapper)` lands here through the interpreter; a subclass with a constructor of its own calls `EClient.__init__(self, wrapper)`, as the reference sample does with `wrapper=self`. Bound once: a second, different wrapper is refused, since callbacks may already be on their way to the first.

```python
def __init__(wrapper)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `wrapper` | `Py<PyAny>` | Wrapper callback receiver for synchronous delivery. |

---

#### `connect`

Connect to IB and start the engine.  Live logins (``paper=False``) enter a second-factor approval window and **block** until the factor is approved (mobile push) or the deadline fires (``ib_key_timeout_secs``, default ~18 min). This is a human approval gate, not a hang. To bound or avoid it: use ``paper=True``, pass a smaller ``ib_key_timeout_secs``, or run ``connect()`` on a worker thread with your own timeout. Paper logins skip the gate entirely. Set ``RUST_LOG=info`` to see a log line when the wait begins.  ``code_provider`` answers that factor with a typed code instead: ``code_provider(factor, display_id, avth_url) -> str``, where ``factor`` is ``"ibkey"`` (return the code shown for ``display_id``) or ``"authenticator"`` (return the account's current code; ``display_id`` and ``avth_url`` are empty). An authenticator account has no push to fall back to and cannot log in without this. It is called once, on a thread of its own, and holds the GIL while it runs — return the code, don't block on input. It is asked once and the login carries whatever it returns; what the venue does with a wrong code has not been exercised from here.  Multiple ``EClient`` instances can run concurrently in one process; each owns its own state, sockets, and engine thread, and ``connect()`` does not serialize across instances. If you pin engines via ``core_id``, give each a distinct value.  `port` is kept and read back on `port`, as the reference client keeps it. The session connects to the venue directly, so there is no local socket for it to open.

```python
def connect(host, port=0, client_id=0, username="", password="", paper=True, core_id=None, ib_key_timeout_secs=None, ib_key_token_sub_type=None, code_provider=None, readonly=False, settings=None, session_file=None, *, clientId=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `host` | `str` | Server hostname. |
| `port` | `int` | Port number (unused — ibkr-dx connects directly). |
| `client_id` | `int` | API client ID for order ownership and the saved order-id counter. |
| `username` | `str` | Account username. |
| `password` | `str` | Account password. |
| `paper` | `bool` | If `true`, connect to paper trading. If `false`, connect blocks on the live second-factor approval window (see method note). |
| `core_id` | `usize or None` | CPU core affinity for the hot loop thread. Use a distinct value per engine when running several in one process. |
| `ib_key_timeout_secs` | `int or None` | Live second-factor approval timeout in seconds (default ~18 min). Lower it to fail fast on unattended live logins; ignored for paper. |
| `ib_key_token_sub_type` | `str or None` | Fallback second-factor token sub-type (default `"2a"`), used only when the server states none for the session; ignored for paper. |
| `code_provider` | `Py<PyAny> or None` | Callable `(factor, display_id, avth_url) -> str` returning the second-factor code. `factor` is `"ibkey"` (the code shown for `display_id`) or `"authenticator"` (the account's current code). Required for authenticator accounts, which have no push to fall back to; ignored for paper. |
| `readonly` | `bool` |  |

---

#### `disconnect`

Disconnect from IB.

```python
def disconnect()
```

---

#### `is_connected`

Whether there is a session to make requests on.  False before `connect` and after `disconnect`, and false from the moment the engine gives the session up — which it writes down itself. The record saying so is delivered only by a read, and a program that drives its own loop, or none at all, is told nowhere else: it read connected on a session that was over, and went on issuing requests into it. The Rust surface answers this the same way.

```python
def is_connected()
```

---

#### `conn_state`

Which of `DISCONNECTED`, `CONNECTING` and `CONNECTED` this client is in.  `CONNECTING` is the span of `connect`, as read from another thread: the connection is claimed at the top of that call and the session's state handed over at the bottom, and `is_connected` reads true for the whole of it. A session that ended without being closed is connecting again from the moment `connect` is called on it, though what it held stays in place until the logon answers.  A session the engine has given up is `DISCONNECTED` from the moment it writes that down, however the caller drives its loop — read from the cached flags alone it stayed `CONNECTED` for as long as the program went without dispatching.

```python
client.conn_state  # read-only attribute
```

---

#### `client_id`

The number this session connected under, as the caller gave it, and `None` when there is no session.

```python
client.client_id  # read-only attribute
```

---

#### `host`

The server this session connected to, and `None` when there is no session.  The one it knocked on, which is the one the caller named — the venue may then send it elsewhere, and the rest of that list is where. The reference client holds what its caller passed here too.

```python
client.host  # read-only attribute
```

---

#### `port`

The port `connect` was given, and `None` when there is no session.  Kept as the caller gave it, as the reference client keeps it. This session speaks to the venue's servers on the ports the venue names, so the number opens nothing; a program that reads it back gets what it passed.

```python
client.port  # read-only attribute
```

---

#### `conn`

The client itself while a session is held, and `None` without one.  The reference client keeps its connection here, and what a program reads off it is `isConnected()`, `host` and `port` — which this client answers, following the session. Its socket is not here: this client's connections are opened, read and kept alive inside its engine.

```python
client.conn  # read-only attribute
```

---

#### `asynchronous`

`False`: there is no asynchronous mode. The reference client sets this nowhere either; its sample program reads it in `connect_ack` to decide whether to start the exchange itself. Here `connect` returns with the session up and its engine running, and `connect_ack` is announced from inside it, so there is nothing left for a caller to start.

```python
client.asynchronous  # read-only attribute
```

---

#### `server_version`

The protocol level this client implements: 217, the reference client's `MIN_SERVER_VER_ADDITIONAL_ORDER_PARAMS_2`. `None` before a session and after one, as the reference client answers before its greeting — and held while a lost connection is recovered, which the reference client rides out holding the number.  In the reference architecture this number is the API level of the process a program is talking to. That process was a gateway, which announced it and had every request gated on it; here it is this client, so the number is a statement about this client and not a reading off the venue, whose logon names no such level.  Attached orders (218) load the selected account preset and construct the parent and children. Percentage allocations still need positions by account and model and the applicable allocation-group state, so the level remains 217. A percentage allocation cannot yet determine the quantities of its attached children here; supply explicitly sized parent and child orders when those quantities depend on group or model holdings. Configuration requests (219, 221) and the last price and size stated to their precision (222, 224) are absent. `hedgeMaxSize` (223) is sent on a beta hedge, and odd-lot quotes (225) are served: generic tick 787 is asked for and its prices, sizes and venues delivered. `conditionsIncludeOvernight` (226) is sent with an order's conditions, and refused under 10371 when the order is placed where the logon does not enable it or the contract trades on no overnight venue. 226 is the highest level a gateway announces.  Below it, a program that believes the number is wrong about the following, and each is said on use rather than passed over:  * An order field this client does not carry, refused by name on `error` under 321 when the order is placed: `smartComboRoutingParams` (57). * A withdrawal stating a manual time (169): the withdrawal goes, with its operator and who entered it, and the caller is told on `error` that the time did not travel.  Every other gate at or below 217 names a request, field or callback that is here and does what it does through a gateway.

```python
def server_version()
```

---

#### `tws_connection_time`

When the venue says this session logged in, by its own clock and in its own spelling; `None` when there is no session, and held while a lost connection is recovered, as the level is.  The reference client answers the time its gateway stamped on its greeting. The venue stamps every message it sends with the time it sent it, the answer to the logon included, and this is that stamp — the clock `competing_session` reads the other session's logon off. Where the venue stamped none, `connect` holds this machine's clock instead and says so in the log.

```python
def tws_connection_time()
```

---

#### `set_connect_options`

The reference client carries these on its greeting to its gateway, which reads them; there is no gateway between this client and the venue to read them, so an option a caller states cannot be carried.  Said rather than swallowed. Every one of these options changes how the session behaves — how fast it may ask, what it is told — and a caller who set one and heard nothing has a session that is not the one they asked for and no way to know it. Stating none is stating nothing, which is what the reference client's own is on an ordinary call.

```python
def set_connect_options(opts)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `opts` | `str` | Connect options, as the reference client takes them. Not applied. |

---

#### `start_api`

Nothing to start, once there is a session. The reference client sends its client id here and its gateway begins the exchange on receiving it. Here the id is kept on the client and never sent — one session holds the account — and `connect` announces `connect_ack`, the accounts and the next order id itself before it returns. Before a session exists this is reported the way the reference client reports it: on `error`, under 504.

```python
def start_api()
```

---

#### `check_connected`

Raises when there is a session, with the message `connect` refuses a second call under. Nothing otherwise.

```python
def check_connected()
```

---

#### `reset`

Forget the session — which here is closing it. The reference client drops its hold on the socket and leaves the socket to its reader thread; a session here is an engine that stays logged in at the venue until it is stopped, so this is `disconnect`. Nothing to forget is fine.

```python
def reset()
```

---

#### `events_lost`

How many engine events this session's channel discarded: none.  This surface reads every engine callback through the session's one order, which keeps each record until a read delivers it, and attaches no event channel that could drop one.

```python
def events_lost()
```

---

#### `backlog`

How many requests this client has handed the engine that the engine has not finished with: still waiting to be taken, or taken and held — for the contract to be named, in the order buffer, or behind the session's own replay. Zero before a session exists.  No call waits for the engine to take what it is handed, so this is what bounds what a caller has handed over. Read once per lap of the engine's loop, which takes at most 64 commands a lap.

```python
def backlog()
```

---

#### `poll`

Deliver everything waiting, once, and return.  `run` owns the thread it is called on, which a program with an event loop of its own cannot give it: an asyncio framework has to drive the callbacks from its own loop, and a blocking loop leaves it nowhere to stand. This is one pass of the same dispatch.

```python
def poll()
```

---

#### `run`

Deliver callbacks until the session ends.  Blocks the calling thread. Everything a program receives arrives from here, so it runs on a thread of its own or is the last call a program makes. `poll` does one pass instead, for a program that owns its loop.

```python
def run()
```

---

#### `wait_for_data`

Wait for the engine to signal, for at most `timeout` seconds: True when it signalled, False when the wait ran out.  The engine signals at the end of each pass of its loop, and when a connection goes or comes back. One waiter takes each signal. A thread that wakes a loop of its own waits here and then has that loop call `poll`: True is a reason to poll, not a promise that the poll delivers anything. Waits with the interpreter released, so other threads run meanwhile. Raises when there is no session.

```python
def wait_for_data(timeout)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `timeout` | `float` | The longest to wait. |

---

#### `get_account_id`

Get the account ID.

```python
def get_account_id()
```

---

#### `session_over`

Whether this session is finished rather than merely disconnected: closed by `disconnect()`, or given up on by the engine. A loss the engine is still working on is neither — `is_connected()` reads false between the 1100 and the 1102, and a request made then is carried when the transports come back.

```python
def session_over()
```

---

#### `instrument_of`

Which slot a contract holds on this session, if it holds one. Read through the lookup every other reader uses, which drops what the engine has given back, so a slot that has gone to the next contract is not named.

```python
def instrument_of(con_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `con_id` | `int` | Contract ID. Unique per instrument. |

---

#### `unread_wire`

What the venue sent this session that nothing here reads, as pairs of the connection and what arrived: each kind of message named once, the first time it arrives. With `IBKR_DX_CAPTURE_WIRE` set, every frame is kept here as well, whole and as sent.

```python
def unread_wire()
```

---

#### `competing_session`

Another session that already held this account when this one connected.  `None` when this session is alone. Otherwise where the other one connected from, when it logged in, and whether this session is held to reading only because the other has the account.  Worth asking before starting work: the venue permits one logon at a time and takes the account from the older session without saying which it dropped, so a second client reads as data that stops arriving.

```python
def competing_session()
```

---

#### `ccp_session_id`

Session ID surfaced to webapp REST clients as `x-ccp-session-id`.

```python
def ccp_session_id()
```

---

#### `misc_url`

Logical-name → host URL lookup from the MiscUrls block of the logon response (e.g. `region_dam`). `None` when the logon did not carry the key.

```python
def misc_url(key)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `key` | `str` | Account value key (e.g. `"NetLiquidation"`, `"BuyingPower"`). |

---

## Calls That Answer

#### `contract_details`

Everything the venue knows about the contracts matching a description.  Sends the lookup, waits for the venue to say it has finished, and hands back every match. A description matching nothing returns an empty list; a venue that refuses the lookup raises with the reason it gave. 

```python
def contract_details(contract)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |

---

#### `corporate_actions`

A contract's corporate actions, asked for and waited on.  One dict per action, stating what the venue stated: its kind as the two-letter name the venue uses, the day it takes effect, its value, and the dates and dividend descriptions the kind carries. A field the kind does not carry is empty rather than invented.  `contract` must carry the venue's id for it. Days are `YYYYMMDD`.

```python
def corporate_actions(contract, start_date, end_date)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `start_date` | `str` |  |
| `end_date` | `str` |  |

---

#### `historical_data`

Bars for a contract over a period, handed back rather than delivered a bar at a time to a callback.  The venue has no adjusted series to pass through: what it serves is raw trades, and the two series the vendor states as adjusted — TRADES and ADJUSTED_LAST — are those folded with the contract's own actions. The fold is made once the series is whole and the actions are in hand, before a bar is handed to anyone. This call waits and hands the series back in one piece; `reqHistoricalData` delivers the same bars one at a time on its callbacks. Both ask for the actions by the venue's id for the contract, which the venue is asked for first where the contract is named some other way.

```python
def historical_data(contract, end_date_time, duration_str, bar_size_setting, what_to_show, use_rth=1)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `end_date_time` | `str` | End date/time in `"YYYYMMDD HH:MM:SS"` format, or empty for now. |
| `duration_str` | `str` | Duration string, e.g. `"1 D"`, `"1 W"`, `"1 M"`, `"1 Y"`. |
| `bar_size_setting` | `str` | Bar size: `"1 min"`, `"5 mins"`, `"1 hour"`, `"1 day"`, etc. |
| `what_to_show` | `str` | Data type: `"TRADES"`, `"MIDPOINT"`, `"BID"`, `"ASK"`, `"BID_ASK"`, etc. |
| `use_rth` | `int` | If `true`, only return data from Regular Trading Hours. |

---

#### `head_timestamp`

The earliest moment the venue holds data for a contract.

```python
def head_timestamp(contract, what_to_show="TRADES", use_rth=1)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `what_to_show` | `str` | Data type: `"TRADES"`, `"MIDPOINT"`, `"BID"`, `"ASK"`, `"BID_ASK"`, etc. |
| `use_rth` | `int` | If `true`, only return data from Regular Trading Hours. |

---

#### `matching_symbols`

Contracts whose symbol or name matches a pattern.

```python
def matching_symbols(pattern)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `pattern` | `str` | Symbol search pattern. |

---

#### `news_headlines`

The headlines the venue holds for a contract.  Answers rather than reporting through the wrapper, because a program written against the reference client reads the return value.

```python
def news_headlines(con_id, provider_codes, start_date_time, end_date_time, total_results)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `con_id` | `int` | Contract ID. Unique per instrument. |
| `provider_codes` | `str` | Pipe-separated news provider codes. |
| `start_date_time` | `str` | Start date/time for tick query. |
| `end_date_time` | `str` | End date/time in `"YYYYMMDD HH:MM:SS"` format, or empty for now. |
| `total_results` | `int` | Maximum number of news results. |

---

#### `trading_schedule`

When a contract trades, over a stretch of days.  Each session is its opening, its close, and the day it belongs to; the time zone they are stated in comes with them.

```python
def trading_schedule(contract, end_date_time, duration_str, use_rth)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `end_date_time` | `str` | End date/time in `"YYYYMMDD HH:MM:SS"` format, or empty for now. |
| `duration_str` | `str` | Duration string, e.g. `"1 D"`, `"1 W"`, `"1 M"`, `"1 Y"`. |
| `use_rth` | `bool` | If `true`, only return data from Regular Trading Hours. |

---

#### `option_chains`

Every venue's option chain for an underlying, returned rather than delivered on a callback: expiries and strikes, per venue.

```python
def option_chains(underlying_symbol, fut_fop_exchange, underlying_sec_type, underlying_con_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `underlying_symbol` | `str` | Underlying symbol (e.g. `"AAPL"`). |
| `fut_fop_exchange` | `str` | Exchange for futures/FOP options. |
| `underlying_sec_type` | `str` | Underlying security type (e.g. `"STK"`). |
| `underlying_con_id` | `int` | Underlying contract ID. |

---

#### `histogram_data`

How a contract's traded volume is spread across prices over a period.

```python
def histogram_data(contract, use_rth=True, time_period="3 days")
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `use_rth` | `bool` | If `true`, only return data from Regular Trading Hours. |
| `time_period` | `str` | Histogram time period. |

---

#### `fundamental_data`

A fundamental report on a contract, as the venue supplies it.

```python
def fundamental_data(contract, report_type)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `report_type` | `str` | Report type: `"ReportSnapshot"`, `"ReportsFinSummary"`, `"RESC"`, etc. |

---

#### `positions`

Every holding in the account: one tuple per holding, its account, its contract, the position and its average cost — what `position` states, handed back rather than delivered.  Read once the account has finished stating its holdings, as `req_positions` reads them; where it had not within the wait, what this session already held is answered and the log says so. A holding named by id alone is given a moment for its definition to land, as there. Nothing is subscribed: asking again reads again.

```python
def positions()
```

---

#### `what_if_order`

What the venue says an order would cost, without placing it: the order's own placement with the question marked on it, answered with the state the venue states for it.  Numbered in the band these calls take, so the answer is this call's and the dispatch loop leaves it, with anything said about it. A placement this client refuses raises at once, with the refusal's words.

```python
def what_if_order(contract, order)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `order` | `Order` | Order parameters (action, quantity, type, price, TIF, etc.). |

---

#### `scan`

Run a scan and hand back what it found: one tuple per row, its rank and the contract's details, then the distance, benchmark and projection the venue states beside it, empty where it states none.  The subscription is withdrawn before this returns: a scan asked for once is a question, and left running it keeps answering into a session nobody is reading. A scan the venue will not run raises with its words.

```python
def scan(instrument, location_code, scan_code, most)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `instrument` | `str` | Instrument type for scanner (e.g. `"STK"`, `"FUT"`). |
| `location_code` | `str` | Scanner location (e.g. `"STK.US.MAJOR"`). |
| `scan_code` | `str` | Scanner code (e.g. `"TOP_PERC_GAIN"`, `"HIGH_OPT_IMP_VOLAT"`). |
| `most` | `int` | Maximum number of scanner results. |

---

#### `calendar_schema`

What the corporate-events calendar says it carries, as the venue's JSON.

```python
def calendar_schema()
```

---

#### `calendar_events`

The calendar's events for one contract, as the venue's JSON.

```python
def calendar_events(con_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `con_id` | `int` | Contract ID. Unique per instrument. |

---

#### `qualify_contract`

Fill in what the venue knows about a contract, above all its id.  Most of what this client sends carries a contract, and a contract with an id is worth more than one without: market data is answered only for a contract named by id, and an order carrying one needs to state nothing else.  A description matching more than one contract is refused rather than resolved to whichever came back first — the same symbol on the same venue exists in more than one currency, and picking one silently is how an order reaches the wrong one.

```python
def qualify_contract(contract)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |

---

#### `qualify_contracts`

Fill in a whole list of contracts, keeping their order.  One that cannot be resolved fails the call rather than being dropped: a list quietly shorter than it was asked for is how a program trades something other than what it named.

```python
def qualify_contracts(contracts)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `contracts` | `list` | Contract specification (symbol, secType, exchange, currency, etc.). |

---

## Account & Portfolio

#### `req_pnl`

Subscribe to the named account's profit. The account is checked as a gateway checks it. Each request has its own subscription; a repeated active request number is refused under 102. A model is taken and not applied, with a log notice once per session.

```python
def req_pnl(req_id, account, model_code="")
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `account` | `str` | Account ID. |
| `model_code` | `str` | Model portfolio code (empty for default). |

---

#### `cancel_pnl`

Cancel P&L subscription.

```python
def cancel_pnl(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_pnl_single`

Subscribe to a position's profit in the named account. The account is checked as for the account-level profit. A model is taken and not applied, with a log notice once per session.

```python
def req_pnl_single(req_id, account, model_code, con_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `account` | `str` | Account ID. |
| `model_code` | `str` | Model portfolio code (empty for default). |
| `con_id` | `int` | Contract ID. Unique per instrument. |

---

#### `cancel_pnl_single`

Cancel single-position P&L subscription.

```python
def cancel_pnl_single(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_account_summary`

Request an account summary. `All` answers for every account the login holds. Account groups and `AllNonProp` are taken and not applied, with a log notice once per session. Validation and the limit of two standing summary requests follow a gateway.

```python
def req_account_summary(req_id, group_name, tags)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `group_name` | `str` | Account group name (e.g. `"All"`). |
| `tags` | `str` | Comma-separated account tags: `"NetLiquidation,BuyingPower,..."`. |

---

#### `cancel_account_summary`

Cancel account summary.

```python
def cancel_account_summary(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_positions`

Request all positions.  Before a session exists this is reported on the error callback and the call returns, as every other request made before connecting is. A program written against the reference client has no exception handling around a request, because that client does not raise there.

```python
def req_positions()
```

---

#### `cancel_positions`

Cancel positions.

```python
def cancel_positions()
```

---

#### `req_account_updates`

Subscribe to the named account's figures and holdings, or withdraw the subscription. A single-account login ignores the name as a gateway does. Subscribing asks the venue to restate that account now; the engine holds the answer until its download ends or the existing wait expires.

```python
def req_account_updates(subscribe, acct_code="")
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `subscribe` | `bool` | `true` to start updates, `false` to stop. |
| `acct_code` | `str` | Account code (e.g. `"DU1234567"`). |

---

#### `req_managed_accts`

Request managed accounts list. Answered with every account this login holds, comma separated, matching the reference client.  Before a session exists there are no accounts to name, and an empty list reads as a login holding none rather than as a question asked too early.

```python
def req_managed_accts()
```

---

#### `req_account_updates_multi`

Subscribe to the named account's figures under this request number. `ledger_and_nlv` selects the per-currency ledger and net liquidation. A model is taken and not applied, with a log notice once per session. The initial batch ends with `account_update_multi_end`; changes keep arriving until the request is cancelled.

```python
def req_account_updates_multi(req_id, account, model_code, ledger_and_nlv=False)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `account` | `str` | Account ID. |
| `model_code` | `str` | Model portfolio code (empty for default). |
| `ledger_and_nlv` | `bool` | If `true`, only the per-currency ledger: each currency's cash, market values and `NetLiquidationByCurrency`. |

---

#### `cancel_account_updates_multi`

Cancel multi-account updates.  The request stops being reported to. The venue keeps the account current whether or not anyone is listening; what stops is the reporting — a figure that moves after this is no longer delivered on `accountUpdateMulti` for this request.  A request the engine still holds is withdrawn, never answered; one it answered stops where the withdrawal stands, after its answer.

```python
def cancel_account_updates_multi(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_positions_multi`

Subscribe to holdings of the named account under this request number. A model is taken and not applied, with a log notice once per session.

```python
def req_positions_multi(req_id, account, model_code)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `account` | `str` | Account ID. |
| `model_code` | `str` | Model portfolio code (empty for default). |

---

#### `cancel_positions_multi`

Cancel multi-account positions. 

```python
def cancel_positions_multi(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `account_snapshot`

Read account state snapshot. Returns a dict with all account values.

```python
def account_snapshot()
```

---

#### `positions_elsewhere`

Holdings the venue reports that this broker does not hold itself: positions held away at another broker, and rows it marks as shown but not held.  Kept apart from `req_positions`, which answers what the account itself holds. The reference client has no call for these — its own front end shows them in a separate table — so this is the only way to reach them. One dict per holding: `con_id`, `symbol`, `sec_type`, `currency`, `position`, `avg_cost`, and `held`, which is `"Away"` for a position held at another broker, `"DisplayOnly"` for a row shown but not held, and `"Aside"` for one reported apart without saying why. Empty with no session.

```python
def positions_elsewhere()
```

---

#### `values_elsewhere`

The account figures describing one of the sets of holdings the account does not hold itself, as name, value and the currency each is stated in. A figure stated in two currencies is two figures.  `held` names the set as `positions_elsewhere` does: `"Away"`, `"DisplayOnly"` or `"Aside"`. The venue states these the same way it states the account's own, and mixing them in would overstate what the account is worth, so they are kept where the holdings they describe are kept. Empty with no session.

```python
def values_elsewhere(held)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `held` | `str` | Which set of holdings kept elsewhere: `Away`, `DisplayOnly` or `Aside`. |

---

## Orders

#### `place_order`

Place an order.  A request the client will not send is reported under the number the reference client reports it under, and the call returns. A program moved from that client has an `error` handler and no exception handling around a request, because nothing it was written against raises there. A send the engine can no longer take is reported the same way, stating what has already reached the engine and what has not.  `slOrderId` / `slOrderType` and `ptOrderId` / `ptOrderType` construct children from the selected account preset. State each child id with `PRESET` (case-insensitive). Preset loading and any quote wait run in the engine; the call returns without waiting. The parent goes first, then stop loss and profit taker. `transmit=False` holds the family until a parent or child transmits it. Replacing the parent preserves its existing children. Percentage-allocation sizing from group or model holdings is not carried; use explicitly sized parent and child orders where their quantities depend on it.

```python
def place_order(order_id, contract, order)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `order_id` | `int` | Order identifier. Must be unique per session. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `order` | `Order` | Order parameters (action, quantity, type, price, TIF, etc.). |

---

#### `exercise_options`

Exercise or lapse a long option position.

```python
def exercise_options(req_id, contract, exercise_action, exercise_quantity, account, _override, manual_order_time, customer_account, professional_customer)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `exercise_action` | `int` | 1=exercise, 2=lapse. |
| `exercise_quantity` | `int` | Number of contracts to exercise. |
| `account` | `str` | Account ID. |
| `override` | `int` | Override flag for exercise. |
| `manual_order_time` | `str` |  |
| `customer_account` | `str` |  |
| `professional_customer` | `bool` |  |

---

#### `cancel_order`

Cancel an order.  The second argument is what the reference client states about the withdrawal itself — when a person entered it, on whose authority, and whether a person entered it at all. It is taken as that object or as the time alone.  Who is withdrawing it and whether a person entered it travel on the cancel, as a gateway writes them: from the withdrawal, not from the placement. A time does not travel. A gateway sends it only where the venue has turned that record on for the login, and this client does not read whether it has. The cancel goes anyway and the caller is told the time did not: refused outright, a live order would be left standing over a record this client does not send. Taken silently it would be withdrawn without the record while the caller had given one, so it is said. A time a gateway cannot read is refused as a gateway refuses it, under 10301, and nothing is withdrawn.

```python
def cancel_order(order_id, order_cancel=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `order_id` | `int` | Order identifier. Must be unique per session. |
| `order_cancel` | `Py<PyAny> or None` | What the withdrawal states about itself: an `OrderCancel`, or a manual time alone. `""` states nothing, and so does `None` from Python. |

---

#### `cancel_order_by_perm_id`

Cancel an order identified by `permId` — stable across sessions, unlike the local order id. The cancel frame is orderId-only, so the local id is looked up from the open-order cache; fails if `perm_id` is not tracked.

```python
def cancel_order_by_perm_id(perm_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `perm_id` | `int` | Permanent order ID assigned by the server. |

---

#### `req_global_cancel`

Cancel every order the account is working.  This wire carries no request to withdraw everything, so it is composed here: one cancel for each order held, which is what a caller asking for everything back is asking for. What is held is what the venue named as working at connect and what this session placed since. The venue names the former after the connect returns, so a global cancel issued straight away waits for that naming, as asking for the open orders does, and covers what was named. Where the naming does not finish within the wait, what had been named is still withdrawn and the call says so rather than returning as though every order were covered: a partial cancel that reads as one beats the same cancel in silence, which reads as a complete answer.  What the withdrawal states — who is withdrawing and whether a person entered it — travels on every cancel, as a gateway states it on every order it withdraws. A time does not: the reference client writes none on a withdrawal of everything, so a gateway never reads one, and one stated here goes the same way.

```python
def req_global_cancel(order_cancel=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `order_cancel` | `Py<PyAny> or None` | What the withdrawal states about itself: an `OrderCancel`, or a manual time alone. `""` states nothing, and so does `None` from Python. |

---

#### `req_ids`

Request next valid order ID.  `num_ids` has no effect, as on a gateway: the next valid id is answered whatever number is asked for.  Before a session exists there is no counter to answer from: the id an account may next use is the venue's to state. Answering announces zero, which names no order the venue will hold and is refused on placement. Reported the way the reference client reports every request made before connecting.

```python
def req_ids(num_ids=1)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `num_ids` | `int` | Number of IDs to reserve (unused). |

---

#### `next_order_id`

Reserve the next order ID above the saved counter and venue replay. The account and API client's counter is written under an exclusive lock before returning. A storage failure warns once in the log and leaves allocation using session memory and venue replay.

```python
def next_order_id()
```

---

#### `next_shared_id`

The first id past everything the account has used that a request can also carry.  A caller that numbers its orders and its requests out of one counter — which is how `ib_async` is written — needs both at once: clear of every id an order has spent, and inside the numbers a request can carry. An account that has been given a wider order id than that has no such number above it, so this answers with one past the widest the account has used that a request can carry, and the counting goes on from there. After a connect it waits, for at most three seconds in all, for the venue to name the orders the account is working.  Raises RuntimeError where even that is not a number a request can carry. Answers 1 where there is no session.

```python
def next_shared_id()
```

---

#### `req_open_orders`

Request all open orders for this client.

```python
def req_open_orders()
```

---

#### `req_all_open_orders`

Request all open orders across all clients.  The same answer as `req_open_orders`. The reference client splits the two by client id; this wire carries no client id on an order, so the venue names the orders on the account without stating who entered them. A subset would be an attribution the venue does not supply.

```python
def req_all_open_orders()
```

---

#### `req_auto_open_orders`

Binding orders entered elsewhere to this client.  Served for client 0. Any other client is refused with 327, as a gateway refuses a client other than 0. On a gateway, client 0's flag turns binding on or off; here what binding asks for is the default, since this session is told about every order on the account, whoever entered it. So `b_auto_bind` changes nothing whichever way it is set, and nothing goes to the venue.  `order_bound` does not follow from this call. It is fired once for each order the venue restates when the session opens that this session did not place, pairing the venue's permanent id with the order id it is reached under here.

```python
def req_auto_open_orders(b_auto_bind)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `b_auto_bind` | `bool` | If `true`, auto-bind future orders to this client. |

---

#### `req_executions`

Request execution reports.  Before a session exists this is reported on the error callback, as every other request made before connecting is. Answered instead, the answer waits for a dispatch pass no session is there to make, and the caller hears nothing at all.  `lastNDays` and `specificDates` select days as a gateway selects them, counted on the session's time zone. A date that does not read as a number, or is not a day of the calendar, is refused under 320, as a gateway refuses it. The executions answered from reach back to midnight six days before the logon in UTC, or to the logon's own day for a session set to today's executions, so the earliest days asked for can be missing some; those days are named on `error` under 321 ahead of the answer, which still comes. `acctCode` is ignored on a login holding one account and refused on one holding several where the login does not hold it, as a gateway does both.

```python
def req_executions(req_id, exec_filter=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `exec_filter` | `Py<PyAny> or None` |  |

---

#### `req_completed_orders`

Request completed orders.  `api_only` asks for the orders entered through an API rather than by hand. The venue states no origin beside a finished order, and it does number the ones an API placed: an order that went out through one carries the number that API gave it, and one typed in carries none. So `true` is answered with the orders the venue numbered.

```python
def req_completed_orders(api_only=False)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `api_only` | `bool` |  |

---

## Market Data

#### `set_news_providers`

Set news provider codes for per-contract news ticks (e.g. "BRFG*BRFUPDN").

```python
def set_news_providers(providers)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `providers` | `str` | News provider list. |

---

#### `req_mkt_data`

Request market data for a contract.  With `snapshot`, `tickSnapshotEnd` follows once the snapshot is whole, as a gateway ends one: when the venue has stated the bid, the ask, the last, the open and the close; on a contract of a type a gateway marks as an option (`OPT`, `FOP`, `IOPT`, `WAR`, `EC`) also the option model, 13 (83 delayed); on a delayed feed also the last trade's time, 88; or eleven seconds after the request. Ticks 10, 11 and 12 (80, 81 and 82 delayed), the bid's, the ask's and the last's greeks, are not produced: a gateway computes them with an option model of its own, and the venue does not state them. A gateway's snapshot of an option also waits for them; this client's does not.  `mkt_data_options` is checked as a gateway checks it: `manual`, `0` or `1`, is taken and changes nothing a gateway sends; any other key is refused under 10337, another value under 10338, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a `manual` that does not read as the number nought or one is refused under 321.

```python
def req_mkt_data(req_id, contract, generic_tick_list="", snapshot=False, regulatory_snapshot=False, mkt_data_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `generic_tick_list` | `str` | Comma-separated generic tick IDs (e.g. `"233"` for RT volume). |
| `snapshot` | `bool` | If `true`, delivers one quote then auto-cancels. |
| `regulatory_snapshot` | `bool` | If `true`, request a regulatory snapshot (additional fees may apply). |
| `mkt_data_options` | `list` |  |

---

#### `req_mkt_data_ex`

Like `req_mkt_data`, but names the market-data mode on the request itself (0=realtime, 1=delayed, 2=frozen, 3=delayed-frozen) rather than taking the one the session is set to. The frozen one keeps thinly-traded names quoting after hours, when the realtime feed is silent.  A contract holds one subscription at a time, so this states the mode for that subscription rather than adding a second alongside it: a later request for a contract already subscribed follows the one that is up and is handed its quotes. To compare two modes on one contract, withdraw between them.  `regulatory_snapshot` asks for the venue's own chargeable one-shot snapshot: a request type of its own rather than a mode on an ordinary quote. It needs the entitlement, and an account without it is refused by the venue, which names the request type back through `error`. It ends the way an ordinary snapshot does, so `tickSnapshotEnd` fires either way.

```python
def req_mkt_data_ex(req_id, contract, generic_tick_list="", snapshot=False, regulatory_snapshot=False, mode_9887=0, mkt_data_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `generic_tick_list` | `str` | Comma-separated generic tick IDs (e.g. `"233"` for RT volume). |
| `snapshot` | `bool` | If `true`, delivers one quote then auto-cancels. |
| `regulatory_snapshot` | `bool` | If `true`, request a regulatory snapshot (additional fees may apply). |
| `mode_9887` | `int` |  |
| `mkt_data_options` | `list` |  |

---

#### `cancel_mkt_data`

Cancel market data.

```python
def cancel_mkt_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_tick_by_tick_data`

Request tick-by-tick data.  `number_of_ticks` and `ignore_size` are sent as stated: a count of past ticks goes out as the length of the run the stream opens with, and the size filter as the query's filter term. Neither goes out at its default — no prelude, sizes included — which is what the venue does on its own. Whether the venue honours the size filter is the venue's, and what it sends is passed on as it stands, as a gateway passes it: one session saw size-only changes still arrive on a stream that asked to leave them out.

```python
def req_tick_by_tick_data(req_id, contract, tick_type, number_of_ticks=0, ignore_size=False)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `tick_type` | `str` | Tick type ID or tick-by-tick type string. |
| `number_of_ticks` | `int` | Maximum number of ticks to return. |
| `ignore_size` | `bool` | If `true`, asks that a bid/ask change moving only a size be left out. |

---

#### `cancel_tick_by_tick_data`

Cancel tick-by-tick data.

```python
def cancel_tick_by_tick_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_ping`

Request an auth-connection round-trip time sample: sends a lightweight liveness probe with no side effects on subscriptions, contract caches, or pacing budgets. Poll `last_rtt_ms()` after a moment for the result.

```python
def req_ping()
```

---

#### `last_rtt_ms`

Last measured auth-connection round-trip time in milliseconds, or None if never measured. A gauge, not a benchmark `req_ping`. Also sampled automatically by the engine's own liveness probes.

```python
def last_rtt_ms()
```

---

#### `req_market_data_type`

Name the kind of data every subscription after this one asks for: 1 live, 2 frozen, 3 delayed, 4 delayed-frozen.  The type is carried on each subscription that follows, and the `market_data_type` callback reports the type that subscription was made under. A type this client does not know is logged and leaves subscriptions live. `req_mkt_data_ex` states the type per request, which allows two feeds on one contract at once.

```python
def req_market_data_type(market_data_type)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `market_data_type` | `int` | 1=live, 2=frozen, 3=delayed, 4=delayed-frozen. |

---

#### `req_mkt_depth`

Request market depth (L2 order book).  `mkt_depth_options` is checked as a gateway checks it: `manual`, `0` or `1`, is taken and changes nothing a gateway sends; any other key is refused under 10337, another value under 10338, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a `manual` that does not read as the number nought or one is refused under 321.

```python
def req_mkt_depth(req_id, contract, num_rows=5, is_smart_depth=False, mkt_depth_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `num_rows` | `int` | Number of order book rows to subscribe to. |
| `is_smart_depth` | `bool` | If `true`, aggregate depth from multiple exchanges via SMART. |
| `mkt_depth_options` | `list` |  |

---

#### `cancel_mkt_depth`

Cancel market depth.  `is_smart_depth` has no effect: a book is withdrawn by the request that asked for it, and this client remembers which kind that was. Stated as the book was asked for, it withdraws the same book a gateway would.

```python
def cancel_mkt_depth(req_id, is_smart_depth=False)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `is_smart_depth` | `bool` | If `true`, aggregate depth from multiple exchanges via SMART. |

---

#### `req_real_time_bars`

Request real-time 5-second bars.  `bar_size` has no effect, as on a gateway: a real-time bar is five seconds, and the venue's request carries no bar size. A gateway reads the number and does not use it.  Requests for the same bars of one contract — this call's, or the stream a historical request kept up to date rides — read one stream, as on a gateway: the venue serves it under one number, each request is handed every bar under its own id, and a cancel withdraws its own request alone. The stream is withdrawn when the last of them leaves.  `real_time_bars_options` is checked as a gateway checks it: `manual`, `0` or `1`, is taken and changes nothing a gateway sends; any other key is refused under 10337, another value under 10338, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a `manual` that does not read as the number nought or one is refused under 321.

```python
def req_real_time_bars(req_id, contract, bar_size=5, what_to_show="TRADES", use_rth=0, real_time_bars_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `bar_size` | `int` | Bar size: `"1 min"`, `"5 mins"`, `"1 hour"`, `"1 day"`, etc. |
| `what_to_show` | `str` | Data type: `"TRADES"`, `"MIDPOINT"`, `"BID"`, `"ASK"`, `"BID_ASK"`, etc. |
| `use_rth` | `int` | If `true`, only return data from Regular Trading Hours. |
| `real_time_bars_options` | `list` |  |

---

#### `cancel_real_time_bars`

Cancel real-time bars.

```python
def cancel_real_time_bars(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `quote`

Zero-copy SeqLock quote read by req_id. Returns a dict with bid, ask, last, bid_size, ask_size, last_size, volume, high, low, open, close, or None if the req_id is not mapped.

```python
def quote(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `quote_by_instrument`

Zero-copy SeqLock quote read by InstrumentId. Returns a dict with bid, ask, last, bid_size, ask_size, last_size, volume, high, low, open, close, and whether the venue has halted the contract and why — or None if not connected, or for an id past every slot the instrument table holds. Whether it is restricting short sales in it is `shortSaleRestrictedByInstrument`, which is stated on the same record and kept off this one.

```python
def quote_by_instrument(instrument)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `instrument` | `int` | Instrument type for scanner (e.g. `"STK"`, `"FUT"`). |

---

#### `option_model`

What the venue's own model last made of an option, whole.  `tickOptionComputation` carries its greeks and its price, beside the volatility and the underlying's price a gateway takes from the venue's other series. The venue states eighteen figures on this record, and every one is in this dict, its own volatility and underlying price among them. A figure the venue did not state is this API's own unset double; zero is a real greek.  `None` where the request names no subscription, or the venue has not stated a model for it yet.

```python
def option_model(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `short_sale_restricted`

Whether the venue is restricting short sales in the contract a request is watching.  The circuit breaker a venue puts on a contract that has fallen far enough in a day, which stops a short from resting below the bid. Stated on the same record as the halt, and with no field anywhere in the documented API. Not the same question as whether the contract can be borrowed, which ticks 46 and 89 already answer beside it.

```python
def short_sale_restricted(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `short_sale_restricted_by_instrument`

The same, by InstrumentId, for callers who track them themselves.

```python
def short_sale_restricted_by_instrument(instrument)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `instrument` | `int` | Instrument type for scanner (e.g. `"STK"`, `"FUT"`). |

---

#### `contract_figures`

What the venue says about the contract itself, beside its prices: how many shares are on issue, and what it opened at a year ago.  Both arrive on the tick carrying the price extremes and neither has a tick of its own in the documented API — a share count is a fundamentals request there, and a year-ago open has no call at all.

```python
def contract_figures(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `contract_figures_by_instrument`

The same, by InstrumentId, for callers who track them themselves.

```python
def contract_figures_by_instrument(instrument)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `instrument` | `int` | Instrument type for scanner (e.g. `"STK"`, `"FUT"`). |

---

#### `stated_figures`

What one of the venue's own series last stated for a subscription, in the order the series states it.  The venue runs dozens of series the documented API has no call for: a bond's analytics, an option's model volatility, the volume a contract usually opens on, what margin a future takes. Ask for one by its own number in the generic tick list, and read what it stated here. A figure the venue holds nothing for comes as the largest number its field carries.

```python
def stated_figures(req_id, series)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `series` | `int` |  |

---

#### `numbered_figures`

What one of the venue's numbered-table series last stated for a subscription: its figures under the venue's own numbering.  Four series state their figures as two tables, whole numbers and fractional ones, each entry naming what it is before stating it. `fractional` picks the table; the venue numbers the two separately, so the same number in each is not the same figure.

```python
def numbered_figures(req_id, series, fractional=False)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `series` | `int` |  |
| `fractional` | `bool` |  |

---

#### `paired_figures`

The run of paired figures one series last stated for a subscription.  Two series state their figures as a count and then that many pairs: the volatility the venue's own model puts on each point of a curve, and the weight it puts on each price a contract might reach.

```python
def paired_figures(req_id, series)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `series` | `int` |  |

---

#### `req_spread_scan`

Ask the venue to scan an underlying for strategies worth putting on.  The scan goes out beside a subscription for the series the venue states its answer on, because that is how it is asked for: the series carries the answer and the scan tells the venue what to look for. Read the answer with `scanned_strategies` under the same request, and withdraw it with `cancel_mkt_data`.  The documented API has no call for this at all. What the scan states about each strategy is the venue's own, in the venue's own words, and nothing here translates them.

```python
def req_spread_scan(req_id, contract, scan)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `scan` | `SpreadScan` | What to scan the underlying for. |

---

#### `scanned_strategies`

The strategies a spread scan stated for a request, as the venue stated them.  Each is a dict of its legs, which shape of strategy it is and how pressing, the thirteen figures the venue states about it, where it comes out even, and one figure behind those. Empty until a scan has been asked for and answered. The documented API has no call for this.

```python
def scanned_strategies(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `stated_rows`

The rows of three figures one series last stated for a subscription.  What the three are is the series' own:  | Series | The three figures | | --- | --- | | 547 | A quantity, what it is offered at, and a second price where the form states one | | 491 | Which strategy the leg belongs to, the contract it names, and its size | | 320, 376, 530, 532 | The venue's number for a field of a packed quote, its figure, and how far that figure's decimal point moves |  On the packed quotes the venue numbers its fields itself: nought and one are the two sides of the quote and four and five the size behind each, read off a live session. A side the venue is not standing behind reads as minus one hundred. Those figures are counted in the contract's own increments, as every packed figure is — 75815 against an increment of a hundredth is 758.15 — and no scale is put on them here, because which fields are prices and which are counts is the venue's to change.  `f64::MAX` stands where a form states no third figure. The documented API has no call for any of these.

```python
def stated_rows(req_id, series)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `series` | `int` |  |

---

#### `chain_model_parameters`

What one of the option model's chain series last stated for a subscription on an underlying.  A list with one dict per class of the underlying's options: the underlying's price, the dividends expected, and per expiry the yield, the interest rate, the forward and the at-the-money volatilities the model works from. Ask for series 687 for the standing set, or 691 for the set as the chain closed, in the generic tick list of a subscription on the underlying.  The documented API has no call for either: a gateway reads them for its own option model and hands none of it on. Empty where the request names no subscription, or the series has stated nothing for it.

```python
def chain_model_parameters(req_id, series)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `series` | `int` |  |

---

#### `stated_rows_series`

Which series have stated rows for a subscription, in order.

```python
def stated_rows_series(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `paired_figures_series`

Which series have stated paired figures for a subscription, in order.

```python
def paired_figures_series(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `numbered_figures_series`

Which series have stated numbered figures for a subscription, in order.

```python
def numbered_figures_series(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `stated_figures_series`

Which series have stated figures for a subscription, in order.

```python
def stated_figures_series(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `closing_option_model`

What the venue's model made of an option as it closed.  The same model as `option_model` and in the same shape — every greek it states, the ones the documented API has no field for included — but worked out as the contract closed rather than as it stands. The documented API has no call for it at all. Ask for it by the venue's own number for the series in the generic tick list.

```python
def closing_option_model(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `closing_option_model_by_instrument`

The same, by InstrumentId, for callers who track them themselves.

```python
def closing_option_model_by_instrument(instrument)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `instrument` | `int` | Instrument type for scanner (e.g. `"STK"`, `"FUT"`). |

---

#### `option_model_by_instrument`

The same, by InstrumentId, for callers who track them themselves.

```python
def option_model_by_instrument(instrument)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `instrument` | `int` | Instrument type for scanner (e.g. `"STK"`, `"FUT"`). |

---

## Reference Data

#### `req_historical_data`

Request historical bar data.  `chart_options` is checked as a gateway checks it: `manual`, `0` or `1`, is taken and changes nothing a gateway sends; any other key is refused under 10337, another value under 10338, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a `manual` that does not read as the number nought or one is refused under 321.

```python
def req_historical_data(req_id, contract, end_date_time, duration_str, bar_size_setting, what_to_show, use_rth, format_date=1, keep_up_to_date=False, chart_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `end_date_time` | `str` | End date/time in `"YYYYMMDD HH:MM:SS"` format, or empty for now. |
| `duration_str` | `str` | Duration string, e.g. `"1 D"`, `"1 W"`, `"1 M"`, `"1 Y"`. |
| `bar_size_setting` | `str` | Bar size: `"1 min"`, `"5 mins"`, `"1 hour"`, `"1 day"`, etc. |
| `what_to_show` | `str` | Data type: `"TRADES"`, `"MIDPOINT"`, `"BID"`, `"ASK"`, `"BID_ASK"`, etc. |
| `use_rth` | `int` | If `true`, only return data from Regular Trading Hours. |
| `format_date` | `int` | Date format: 1=`"YYYYMMDD HH:MM:SS"`, 2=Unix seconds. |
| `keep_up_to_date` | `bool` | If `true`, continue receiving updates after initial history. |
| `chart_options` | `list` |  |

---

#### `cancel_historical_data`

Cancel historical data.

```python
def cancel_historical_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_head_time_stamp`

Request head timestamp.

```python
def req_head_time_stamp(req_id, contract, what_to_show, use_rth, format_date=1)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `what_to_show` | `str` | Data type: `"TRADES"`, `"MIDPOINT"`, `"BID"`, `"ASK"`, `"BID_ASK"`, etc. |
| `use_rth` | `int` | If `true`, only return data from Regular Trading Hours. |
| `format_date` | `int` | Date format: 1=`"YYYYMMDD HH:MM:SS"`, 2=Unix seconds. |

---

#### `cancel_head_time_stamp`

Cancel head timestamp request.

```python
def cancel_head_time_stamp(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_contract_details`

Request contract details.

```python
def req_contract_details(req_id, contract)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |

---

#### `cancel_contract_data`

Withdraw a contract lookup.  Nothing is sent and nothing answers: there is nothing to withdraw. A gateway asks the venue nothing for this either: it only stops re-sending a lookup it held back while its connection to the venue was down, and this client holds none back — a lookup made with no connection is refused there and then. A lookup already asked for is still answered, as it is through a gateway.

```python
def cancel_contract_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_mkt_depth_exchanges`

Request available exchanges for market depth.

```python
def req_mkt_depth_exchanges()
```

---

#### `req_matching_symbols`

Search for matching symbols.

```python
def req_matching_symbols(req_id, pattern)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `pattern` | `str` | Symbol search pattern. |

---

#### `req_sec_def_opt_params`

Request option chain parameters.  Every argument is stated by the caller, as the reference client requires them to be. With `underlying_sec_type` defaulted to stocks, a caller who left it off asked about the chains of a stock by that name rather than being told they had left it off.

```python
def req_sec_def_opt_params(req_id, underlying_symbol, fut_fop_exchange, underlying_sec_type, underlying_con_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `underlying_symbol` | `str` | Underlying symbol (e.g. `"AAPL"`). |
| `fut_fop_exchange` | `str` | Exchange for futures/FOP options. |
| `underlying_sec_type` | `str` | Underlying security type (e.g. `"STK"`). |
| `underlying_con_id` | `int` | Underlying contract ID. |

---

#### `req_scanner_subscription`

Request scanner subscription.  `scanner_subscription_options` is checked as a gateway checks it: `manual`, `0` or `1`, is taken and changes nothing a gateway sends; any other key is refused under 10337, another value under 10338, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a `manual` that does not read as the number nought or one is refused under 321.

```python
def req_scanner_subscription(req_id, subscription, scanner_subscription_options=None, scanner_subscription_filter_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `subscription` | `Py<PyAny>` | Scanner subscription parameters. |
| `scanner_subscription_options` | `list` |  |
| `scanner_subscription_filter_options` | `list` | Scanner filter tags from `req_scanner_parameters`, e.g. `priceAbove` = `"10"`. |

---

#### `cancel_scanner_subscription`

Cancel scanner subscription.

```python
def cancel_scanner_subscription(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_scanner_parameters`

Request scanner parameters XML.

```python
def req_scanner_parameters()
```

---

#### `req_news_article`

Request a news article.  `news_article_options` is checked as a gateway checks it: `manual`, `0` or `1`, is taken and changes nothing a gateway sends; any other key is refused under 10337, another value under 10338, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a `manual` that does not read as the number nought or one is refused under 321.

```python
def req_news_article(req_id, provider_code, article_id, news_article_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `provider_code` | `str` | News provider code (e.g. `"BRFG"`). |
| `article_id` | `str` | News article identifier. |
| `news_article_options` | `list` |  |

---

#### `req_historical_news`

Request historical news.  Bounds are UTC timestamps, `YYYYMMDD-HH:MM:SS` or `YYYYMMDD HH:MM:SS`, optionally with fractional seconds. Empty bounds are omitted; unreadable ones are refused so the window is not lost.  `historical_news_options` is checked as a gateway checks it: `manual`, `0` or `1`, is taken and changes nothing a gateway sends; any other key is refused under 10337, another value under 10338, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a `manual` that does not read as the number nought or one is refused under 321.

```python
def req_historical_news(req_id, con_id, provider_codes, start_date_time, end_date_time, total_results, historical_news_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `con_id` | `int` | Contract ID. Unique per instrument. |
| `provider_codes` | `str` | Pipe-separated news provider codes. |
| `start_date_time` | `str` | Start date/time for tick query. |
| `end_date_time` | `str` | End date/time in `"YYYYMMDD HH:MM:SS"` format, or empty for now. |
| `total_results` | `int` | Maximum number of news results. |
| `historical_news_options` | `list` |  |

---

#### `cancel_historical_news`

Withdraw a historical news query.  The TWS API has no call for this; the venue has a message for it. One message carrying the number the query went out under, sent whether or not the query has been answered: the venue serves it past the reply.

```python
def cancel_historical_news(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_adjustments`

Ask for a contract's corporate actions over a range of days.  No callback carries the answer; a refusal arrives on `error` under this id, as any request's does, and gives the request up: nothing is held for it after, and there is nothing to withdraw. The answer is held under the id until `adjustments_for` takes it or `cancel_adjustments` gives it up, so a request that is neither taken nor withdrawn holds its answer for the rest of the session. `corporate_actions` asks and waits in one call; this is the request on its own.

```python
def req_adjustments(req_id, con_id, sec_type, exchange, start_date, end_date)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `con_id` | `int` | Contract ID. Unique per instrument. |
| `sec_type` | `str` |  |
| `exchange` | `str` | Exchange name. |
| `start_date` | `str` |  |
| `end_date` | `str` |  |

---

#### `adjustments_for`

The corporate actions answering a `req_adjustments` under this id, once they have arrived: one dict per action, as `corporate_actions` states them.  Taken rather than read: the answer is handed over once and the request holds nothing after it. `None` until the answer arrives, and for a request this session is not holding one for. A contract the venue states nothing for answers with an empty list, which is an answer.

```python
def adjustments_for(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `cancel_adjustments`

Give up on a `req_adjustments`: whatever it holds is let go of, and the venue is told to stop serving the query.  For a request whose answer has not come and is no longer wanted: the venue serves the query until it is withdrawn. A withdrawal naming no query this client is waiting on, one already answered included, is reported on `error` under 300.

```python
def cancel_adjustments(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_fundamental_data`

Request fundamental data.  `fundamental_data_options` is taken and nothing in it is checked or applied, as through a gateway: a gateway reads no option list on this request.

```python
def req_fundamental_data(req_id, contract, report_type, fundamental_data_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `report_type` | `str` | Report type: `"ReportSnapshot"`, `"ReportsFinSummary"`, `"RESC"`, etc. |
| `fundamental_data_options` | `list` |  |

---

#### `cancel_fundamental_data`

Cancel fundamental data.

```python
def cancel_fundamental_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_historical_ticks`

Request historical tick data.  `ignore_size` asks the venue to leave out a bid/ask change that moves only a size, and what it answers is passed on as it stands, as a gateway passes it: nothing is filtered here. A gateway asks for midpoint ticks that way whatever the caller asked, and so does this client; for trades it is not asked. One session saw the venue answer the same with the filter as without it.  `misc_options` is checked as a gateway checks it: `manual`, `0` or `1`, is taken and changes nothing a gateway sends; any other key is refused under 10337, another value under 10338, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a `manual` that does not read as the number nought or one is refused under 321.

```python
def req_historical_ticks(req_id, contract, start_date_time="", end_date_time="", number_of_ticks=1000, what_to_show="TRADES", use_rth=1, ignore_size=False, misc_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `start_date_time` | `str` | Start date/time for tick query. |
| `end_date_time` | `str` | End date/time in `"YYYYMMDD HH:MM:SS"` format, or empty for now. |
| `number_of_ticks` | `int` | Maximum number of ticks to return. |
| `what_to_show` | `str` | Data type: `"TRADES"`, `"MIDPOINT"`, `"BID"`, `"ASK"`, `"BID_ASK"`, etc. |
| `use_rth` | `int` | If `true`, only return data from Regular Trading Hours. |
| `ignore_size` | `bool` | If `true`, asks that a bid/ask change moving only a size be left out. |
| `misc_options` | `list` |  |

---

#### `cancel_historical_ticks`

Withdraw a historical ticks request.  Nothing is sent and nothing answers: there is nothing to withdraw, as for `cancel_contract_data`. A gateway only stops re-sending a request it held back while its connection to the venue was down, which this client never does. Ticks already asked for still arrive, and a request waiting for its contract to be named still goes once it is, as through a gateway.

```python
def cancel_historical_ticks(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_market_rule`

Request market rule details.

```python
def req_market_rule(market_rule_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `market_rule_id` | `int` | Market rule ID. |

---

#### `req_histogram_data`

Request histogram data.

```python
def req_histogram_data(req_id, contract, use_rth, time_period)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `use_rth` | `bool` | If `true`, only return data from Regular Trading Hours. |
| `time_period` | `str` | Histogram time period. |

---

#### `cancel_histogram_data`

Cancel histogram data.

```python
def cancel_histogram_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_historical_schedule`

Request historical trading schedule.

```python
def req_historical_schedule(req_id, contract, end_date_time="", duration_str="1 M", use_rth=True)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `end_date_time` | `str` | End date/time in `"YYYYMMDD HH:MM:SS"` format, or empty for now. |
| `duration_str` | `str` | Duration string, e.g. `"1 D"`, `"1 W"`, `"1 M"`, `"1 Y"`. |
| `use_rth` | `bool` | If `true`, only return data from Regular Trading Hours. |

---

## Gateway-Local & Stubs

#### `req_config_proto_buf`

Request configuration. Reports 10357 on the request's error callback.

```python
def req_config_proto_buf(config_request_proto)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `config_request_proto` | `Bound<'_, PyAny>` |  |

---

#### `update_config_proto_buf`

Request a configuration update. Reports 10357 on the request's error callback.

```python
def update_config_proto_buf(update_config_request_proto)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `update_config_request_proto` | `Bound<'_, PyAny>` |  |

---

#### `order_permissions`

Security type → the order types the venue permits for it, as stated at logon. Empty until the session is up.

```python
def order_permissions()
```

---

#### `permitted_order_types`

The order types permitted for one security type, or `None` when the type is not permitted at all. A combination is named `COMB`.

```python
def permitted_order_types(sec_type)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `sec_type` | `str` |  |

---

#### `enabled_features`

Feature tokens the venue enables for this account.

```python
def enabled_features()
```

---

#### `algorithms`

Which algorithms the venue offers, keyed `PROVIDER/SECTYPE`.

```python
def algorithms()
```

---

#### `algorithms_for`

The algorithms offered for one security type, across every provider.

```python
def algorithms_for(sec_type)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `sec_type` | `str` |  |

---

#### `order_presets`

The sets of order defaults this account holds, as `(key, attributes, when it last changed)`.  The venue keeps one per security type and fills parts of an order the caller left unstated from them, so the same call on two accounts is not the same order. The key is the venue's own. The attributes are as the venue writes them, `&` between them: `v=` names the set's variant and `a=1` marks it active. The values in a set are asked for separately.  The moment says *when* a set last changed, which is what tells a caller whether an order it sent at a given time was filled in from the old defaults or the new. Empty where the venue stated none.

```python
def order_presets()
```

---

#### `company_data`

What the venue states about a contract's company or its terms on one series, as the pairs it wrote.  Ask for the series on the market data request by the venue's own number for it. Seventeen of them carry this text: 386 is what the company has coming and when; 434 and 548 are the two analyst ratings; 454 is how much of the company institutions and insiders hold, on what date each was counted, and how many shares are on issue, from which a float is worked out rather than stated; 505 is what a fund will take part in; 628 and 633 are a fund family's figures and the same by financial year; 631 the technical readings taken on the contract; 669 the ratios the venue keeps a history of; 678 and 699 two scores worked out from what is written about the company; 700 and 705 how it scores against the principles an account can screen on; 703 what margin dealing in it takes; 726 the lens the venue publishes over its accounts; 750 the price the venue holds it against for reference; and 752 whether it passes a religious screen.  The keys are the venue's own, unchanged.

```python
def company_data(con_id, series)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `con_id` | `int` | Contract ID. Unique per instrument. |
| `series` | `int` |  |

---

#### `company_data_series`

Which of those series have been stated for a contract.

```python
def company_data_series(con_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `con_id` | `int` | Contract ID. Unique per instrument. |

---

#### `calculate_implied_volatility`

What volatility a price implies for an option, under the model the venue publishes for that contract. Answered on `tick_option_computation`.  `implied_vol_options` is checked as a gateway checks it: this request takes no key, so any is refused under 10337, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a list that reads is taken. Nothing in it is sent: this client answers the calculation itself, from the venue's model.

```python
def calculate_implied_volatility(req_id, contract, option_price, under_price, implied_vol_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `option_price` | `float` | Option market price. |
| `under_price` | `float` | Underlying asset price. |
| `implied_vol_options` | `list` |  |

---

#### `calculate_option_price`

What an option is worth at a stated volatility, under the same model. Answered on `tick_option_computation`.  `opt_prc_options` is checked as a gateway checks it: this request takes no key, so any is refused under 10337, and an entry not written `key=value` under 320. Where the venue has lifted the key checks, a list that reads is taken. Nothing in it is sent: this client answers the calculation itself, from the venue's model.

```python
def calculate_option_price(req_id, contract, volatility, under_price, opt_prc_options=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Contract` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `volatility` | `float` | Implied volatility. |
| `under_price` | `float` | Underlying asset price. |
| `opt_prc_options` | `list` |  |

---

#### `cancel_calculate_implied_volatility`

Stop waiting on an implied-volatility request.  A question answered in the call it was asked in leaves nothing to withdraw. One that opened a watch is holding a subscription the caller never asked for by name, and this is what releases it.

```python
def cancel_calculate_implied_volatility(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `cancel_calculate_option_price`

As for `cancel_calculate_implied_volatility`.

```python
def cancel_calculate_option_price(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_news_bulletins`

Ask for the notices the venue broadcasts to everyone. Answered on `update_news_bulletin`.  `all_msgs` asks for the day's bulletins as well as the ones still to come. Nothing is sent to the venue: it broadcasts these unasked and has been doing so since the session opened, so the day's are answered from what is queued. Asking only for what follows drops that queue, or the next poll opens with a bulletin published before the caller asked for any. What cannot be had either way is anything from before the session existed, because there is no request to ask for it with. The last `NEWS_BULLETIN_LIMIT` are kept for a caller who has not asked yet.

```python
def req_news_bulletins(all_msgs=True)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `all_msgs` | `bool` | If `true`, receive all existing bulletins on subscribe. |

---

#### `cancel_news_bulletins`

Stop receiving broadcast notices.

```python
def cancel_news_bulletins()
```

---

#### `req_current_time`

Ask for the venue's own clock. Answered on `current_time`.  Before a session exists this is reported on `error`, the way every request made before connecting is: an answer waits for a dispatch pass, and with no session there is nothing to make one.

```python
def req_current_time()
```

---

#### `req_current_time_in_millis`

Ask for the venue's own clock in milliseconds. Answered on `current_time_in_millis`.  The same clock `req_current_time` reports and worked out the same way. What differs is the precision kept: asking in seconds throws away the fraction this one keeps.  Before a session exists this is reported on `error`, as `req_current_time` is.

```python
def req_current_time_in_millis()
```

---

#### `request_fa`

Ask the venue for a partition of the advisor's own configuration.  The reference client names the partition by a number: its groups, its allocation profiles, its aliases. The venue names it by a word, so the number is turned into the word it stands for. A number that stands for nothing is refused rather than sent as an empty partition.  The venue's answer reaches `receive_fa` under the same number the partition was asked for by.

```python
def request_fa(fa_data_type)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `fa_data_type` | `int` | FA data type (1=Groups, 2=Profiles, 3=Aliases). |

---

#### `replace_fa`

Replace a partition of the advisor's configuration with the one given.  `replace_fa_end` fires with `req_id` once the venue has taken it, and a venue that refuses states why on `error` under the same number.

```python
def replace_fa(req_id, fa_data_type, cxml)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `fa_data_type` | `int` | FA data type (1=Groups, 2=Profiles, 3=Aliases). |
| `cxml` | `str` | FA XML configuration data. |

---

#### `query_display_groups`

Ask which display groups exist. Answered on `display_group_list`.

```python
def query_display_groups(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `subscribe_to_group_events`

Watch what a display group is showing. Answered on `display_group_updated`.

```python
def subscribe_to_group_events(req_id, group_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `group_id` | `int` | Display group ID. |

---

#### `unsubscribe_from_group_events`

Stop watching a display group.

```python
def unsubscribe_from_group_events(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `update_display_group`

Tell a display group what to show.

```python
def update_display_group(req_id, contract_info)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract_info` | `str` | Display group contract info string. |

---

#### `verify_request`

Answered as the reference client answers it: on `error`, under 508 and no request, because intent to authenticate is stated on the initial connect and was not. Nothing is sent, so `api_name` and `api_version` reach nothing.

```python
def verify_request(api_name, api_version)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `api_name` | `str` |  |
| `api_version` | `str` |  |

---

#### `verify_and_auth_request`

As `verify_request`: `api_name`, `api_version` and `opaque_isv_key` reach nothing.

```python
def verify_and_auth_request(api_name, api_version, opaque_isv_key)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `api_name` | `str` |  |
| `api_version` | `str` |  |
| `opaque_isv_key` | `str` |  |

---

#### `verify_message`

Nothing is sent and nothing answers. A gateway reads this message and discards it, so a program on one is answered by nothing either, and `api_data` reaches nothing there or here.

```python
def verify_message(api_data)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `api_data` | `str` |  |

---

#### `verify_and_auth_message`

As `verify_message`: `api_data` and `xyz_response` reach nothing.

```python
def verify_and_auth_message(api_data, xyz_response)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `api_data` | `str` |  |
| `xyz_response` | `str` |  |

---

#### `req_smart_components`

Ask which venue each bit of a quote's exchange mask refers to. The venue states the map beside the quote, so a quote has to have been asked for first. Answered on `smart_components`.  `bbo_exchange` names the map: the one `tick_req_params` states for the contract. The venue states a map per BBO exchange and security type, so one contract's venues are not another's. A BBO exchange no subscription named is refused as a gateway refuses it; one whose map has not arrived yet is waited for up to two seconds, as a gateway waits, and answered from the dispatch loop rather than by holding this call.

```python
def req_smart_components(req_id, bbo_exchange)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `bbo_exchange` | `str` | BBO exchange for smart component lookup (e.g. `"SMART"`). |

---

#### `req_news_providers`

Ask which news providers this account may read. Answered on `news_providers`.

```python
def req_news_providers()
```

---

#### `req_soft_dollar_tiers`

Ask which soft dollar tiers this account may direct commission to. Answered on `soft_dollar_tiers`.

```python
def req_soft_dollar_tiers(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_family_codes`

Ask which account families this login belongs to. Answered on `family_codes`.

```python
def req_family_codes()
```

---

#### `set_server_log_level`

How much to log about this session, 1 to 5.  1 to 5 are a gateway's System, Error, Warning, Info and Detail, and set this client's logger to error, error, warn, info and trace. A gateway applies the level to its own log; this client, which serves the caller in its place, applies it to the logger it installed. Nothing goes to the venue, which has no message for it. Where the program installed a logger of its own, the call says so on `error` rather than reporting a level it did not set. A level outside 1 to 5 is refused rather than reported back as `warn`, which would tell a caller they had a level that does not exist.

```python
def set_server_log_level(log_level=2)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `log_level` | `int` | A gateway's level: 1=System, 2=Error, 3=Warning, 4=Info, 5=Detail; this client's logger goes to error, error, warn, info and trace. |

---

#### `req_user_info`

Ask what this login is entitled to. Answered on `user_info`.

```python
def req_user_info(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_wsh_meta_data`

What event types the corporate-events calendar carries. Answered on `wshMetaData`.

```python
def req_wsh_meta_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `cancel_wsh_meta_data`

Stop waiting on the event types.  The query is one message and one answer, so there is nothing at the venue to withdraw: what is withdrawn is the answer, which would otherwise reach a caller who has said they are done with it. A cancel naming no waiting request says so rather than returning as though it acted.

```python
def cancel_wsh_meta_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `cancel_wsh_event_data`

Stop waiting on the calendar's events. As above.

```python
def cancel_wsh_event_data(req_id)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `req_wsh_event_data`

The calendar's events. Answered on `wshEventData`.  `wsh_event_data` is the object the public API takes: a contract id, or a filter the caller writes, plus the window and what to fill from.

```python
def req_wsh_event_data(req_id, wsh_event_data=None)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `wsh_event_data` | `Py<PyAny> or None` |  |

---

## EWrapper Callbacks

#### `new`

Create a new EClient (or EWrapper) instance.

| Parameter | Type | Description |
|-----------|------|-------------|
| `args` | `Bound<'_, pyo3::types::PyTuple>` |  |
| `kwargs` | `Bound<'_, pyo3::types::PyDict> or None` |  |

---

#### `connect_ack`

The session is open. Nothing has been asked for yet.

---

#### `connection_closed`

The session is over, because this client ended it. A session that went away instead is reported on `error` under 1100.

---

#### `next_valid_id`

The first order id this session may use. Each order needs one higher than the last.

| Parameter | Type | Description |
|-----------|------|-------------|
| `order_id` | `int` | Order identifier. Must be unique per session. |

---

#### `managed_accounts`

Every account this login may act for, separated by commas. One for most logins; an advisor has several.

| Parameter | Type | Description |
|-----------|------|-------------|
| `accounts_list` | `str` | Comma-separated account IDs. |

---

#### `error`

What the venue said about a request, under the number it says it with. Codes from 2100 to 2200 are notices about a connection rather than failures. `req_id` is -1 for anything that answers no particular request.  A request this client will not send is reported here too, under the same numbers the reference client uses: 321 for a request that fails validation, 200 for a contract description that matches nothing, 504 for a call made with no session.  `error_time` is the reference client's second parameter and is stated wherever it states one. It carries a clock reading in milliseconds for trouble this client raises before anything reached the venue, and zero for trouble the venue stated — which is what that client passes for a session speaking a protocol older than the one that added the field, and this one says it speaks an older protocol than that.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `error_time` | `int` |  |
| `error_code` | `int` | Error code. |
| `error_string` | `str` | Error message. |
| `advanced_order_reject_json` | `str` | JSON with advanced rejection details. |

---

#### `error_from`

An error, with what it is about: a request and whether nothing more follows for it, an order and the operation it answers, a request that carries no number, the session, or a lookup this client made for itself. The number `error` carries can be any of these and does not say which; `origin` says.  Every error reaches a subclass of this class here. By default it goes on to `error`, under the number `origin.id` states it under, so a subclass that overrides only `error` is told exactly what it was told before. A wrapper that is not a subclass and has no method of this name is called on `error`, as the reference client calls it.

| Parameter | Type | Description |
|-----------|------|-------------|
| `origin` | `ErrorOrigin` | What the error is about: a request, an order and the operation on it, a request with no number of its own, the session, or a lookup this client made for itself. |
| `error_time` | `int` |  |
| `error_code` | `int` | Error code. |
| `error_string` | `str` | Error message. |
| `advanced_order_reject_json` | `str` | JSON with advanced rejection details. |

---

#### `current_time`

The venue clock, in seconds since the epoch.

| Parameter | Type | Description |
|-----------|------|-------------|
| `time` | `int` | Tick timestamp (Unix seconds). |

---

#### `current_time_in_millis`

The venue clock, in milliseconds since the epoch.  The same clock `current_time` reports, at the precision the venue stated it in. The stamp can carry a fraction of a second and this reads it where it does, but the stamps measured against this venue carried none, so the answer lands on a whole second.

| Parameter | Type | Description |
|-----------|------|-------------|
| `time_in_millis` | `int` |  |

---

#### `tick_price`

One price of a quote, and which price it is. `tick_type` names it — 1 bid, 2 ask, 4 last, 9 close — and `attrib` says whether it can be traded against and whether it is past its limit. A size arrives on `tick_size` under the type that belongs to it.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `tick_type` | `int` | Tick type ID or tick-by-tick type string. |
| `price` | `float` | Tick price. |
| `attrib` | `Py<PyAny>` | Tick attributes. |

---

#### `tick_size`

One size of a quote, and which size it is: 0 bid, 3 ask, 5 last, 8 the day's volume.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `tick_type` | `int` | Tick type ID or tick-by-tick type string. |
| `size` | `float` | Tick size. |

---

#### `tick_string`

A quote's value that is not a number — a timestamp, an exchange map, a set of ids.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `tick_type` | `int` | Tick type ID or tick-by-tick type string. |
| `value` | `str` | Account value. |

---

#### `tick_generic`

A quote's value that is a number and is not a price or a size — an implied volatility, an index future's premium, a halt.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `tick_type` | `int` | Tick type ID or tick-by-tick type string. |
| `value` | `float` | Account value. |

---

#### `tick_snapshot_end`

A snapshot has stated everything it is going to. Only for a subscription asked for as a snapshot; a streaming one never ends.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `market_data_type`

Which feed a subscription is being served from: 1 live, 2 frozen, 3 delayed, 4 delayed and frozen.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `market_data_type` | `int` | 1=live, 2=frozen, 3=delayed, 4=delayed-frozen. |

---

#### `order_status`

Where an order stands now. Fires on every change, and again on each fill. `filled` and `remaining` are shares, `avg_fill_price` the average of what has filled so far.

| Parameter | Type | Description |
|-----------|------|-------------|
| `order_id` | `int` | Order identifier. Must be unique per session. |
| `status` | `str` | Order status string (`"Submitted"`, `"Filled"`, `"Cancelled"`, etc.). |
| `filled` | `float` | Cumulative filled quantity. |
| `remaining` | `float` | Remaining quantity. |
| `avg_fill_price` | `float` | Average fill price. |
| `perm_id` | `int` | Permanent order ID assigned by the server. |
| `parent_id` | `int` | Parent order ID (0 if no parent). |
| `last_fill_price` | `float` | Price of the last fill. |
| `client_id` | `int` | API client ID for order ownership and the saved order-id counter. |
| `why_held` | `str` | Reason the order is held (e.g. `"locate"`). |
| `mkt_cap_price` | `float` | Market cap price for the order. |

---

#### `open_order`

An order as the venue holds it, and the state it is in. Fires beside every `order_status`, when open orders are asked for, and once for a preview — where the state carries what the order would cost and no status follows, because a preview is not an order.

| Parameter | Type | Description |
|-----------|------|-------------|
| `order_id` | `int` | Order identifier. Must be unique per session. |
| `contract` | `Py<PyAny>` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `order` | `Py<PyAny>` | Order parameters (action, quantity, type, price, TIF, etc.). |
| `order_state` | `Py<PyAny>` | Order state (status, margin, commission info). |

---

#### `open_order_end`

Every open order has been stated.

---

#### `exec_details`

One fill, against the order and contract it filled. What it cost arrives separately, on `commission_and_fees_report`.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract` | `Py<PyAny>` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `execution` | `Py<PyAny>` | Execution details (exec_id, time, price, shares, etc.). |

---

#### `exec_details_end`

Every execution answering this request has been stated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `commission_and_fees_report`

What a fill cost, matched to it by execution id.

| Parameter | Type | Description |
|-----------|------|-------------|
| `commission_and_fees_report` | `Py<PyAny>` |  |

---

#### `update_account_value`

One figure the venue states about an account, in the currency it states it in. An account is stated in several currencies at once, so the same key arrives more than once.

| Parameter | Type | Description |
|-----------|------|-------------|
| `key` | `str` | Account value key (e.g. `"NetLiquidation"`, `"BuyingPower"`). |
| `value` | `str` | Account value. |
| `currency` | `str` | Currency code (e.g. `"USD"`). |
| `account_name` | `str` | Account identifier. |

---

#### `update_portfolio`

One position, as the venue values it now.

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Py<PyAny>` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `position` | `float` | Book position (row index) or position size. |
| `market_price` | `float` | Current market price. |
| `market_value` | `float` | Current market value of position. |
| `average_cost` | `float` | Average cost basis. |
| `unrealized_pnl` | `float` | Unrealized profit/loss. |
| `realized_pnl` | `float` | Realized profit/loss. |
| `account_name` | `str` | Account identifier. |

---

#### `update_account_time`

When the account figures above were last stated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `timestamp` | `str` | Timestamp string. |

---

#### `account_download_end`

The account has been fully stated. Fires once the venue has stopped adding to it, not on the first figure.

| Parameter | Type | Description |
|-----------|------|-------------|
| `account` | `str` | Account ID. |

---

#### `account_summary`

One figure answering `req_account_summary`, in the currency the venue states it in.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `account` | `str` | Account ID. |
| `tag` | `str` | Account tag name (e.g. `"NetLiquidation"`). |
| `value` | `str` | Account value. |
| `currency` | `str` | Currency code (e.g. `"USD"`). |

---

#### `account_summary_end`

Every figure answering this request has been stated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `position`

One position held, on any account this login may act for.

| Parameter | Type | Description |
|-----------|------|-------------|
| `account` | `str` | Account ID. |
| `contract` | `Py<PyAny>` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `pos` | `float` | Position size (decimal shares). |
| `avg_cost` | `float` | Average cost per share. |

---

#### `position_end`

Every position has been stated.

---

#### `pnl`

An account's running profit: today's, what is unrealised, and what has been realised.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `daily_pnl` | `float` | Daily profit/loss. |
| `unrealized_pnl` | `float` | Unrealized profit/loss. |
| `realized_pnl` | `float` | Realized profit/loss. |

---

#### `pnl_single`

The same for one position, with the size held.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `pos` | `float` | Position size (decimal shares). |
| `daily_pnl` | `float` | Daily profit/loss. |
| `unrealized_pnl` | `float` | Unrealized profit/loss. |
| `realized_pnl` | `float` | Realized profit/loss. |
| `value` | `float` | Account value. |

---

#### `historical_data`

One bar answering a historical request. `bar.date` is a day for a daily bar and a moment for anything shorter, in the zone the bar carries.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `bar` | `Py<PyAny>` | Bar data (date, open, high, low, close, volume, wap, bar_count). |

---

#### `historical_data_end`

Every bar answering this request has been stated, and the window they cover.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `start` | `str` | Period start date/time. |
| `end` | `str` | Period end date/time. |

---

#### `historical_data_update`

A bar that continues a `keep_up_to_date` request, after its first batch completed. The bar still forming is restated as it changes.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `bar` | `Py<PyAny>` | Bar data (date, open, high, low, close, volume, wap, bar_count). |

---

#### `head_timestamp`

The earliest moment the venue holds data for a contract.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `head_timestamp` | `str` | Earliest available data timestamp string. |

---

#### `contract_details`

One contract matching a description, with everything the venue states about it. A description can match more than one.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract_details` | `Py<PyAny>` |  |

---

#### `contract_details_end`

Every contract matching this request has been stated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `symbol_samples`

Contracts whose symbol or name matches a pattern, across venues.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract_descriptions` | `Py<PyAny>` |  |

---

#### `tick_by_tick_all_last`

One trade, as it happens. `tick_attrib_last` says whether it was past a limit and whether it goes unreported to the tape.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `tick_type` | `int` | Tick type ID or tick-by-tick type string. |
| `time` | `int` | Tick timestamp (Unix seconds). |
| `price` | `float` | Tick price. |
| `size` | `float` | Tick size. |
| `tick_attrib_last` | `Py<PyAny>` |  |
| `exchange` | `str` | Exchange name. |
| `special_conditions` | `str` | Special trade conditions. |

---

#### `tick_by_tick_bid_ask`

One change to the top of the book, as it happens.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `time` | `int` | Tick timestamp (Unix seconds). |
| `bid_price` | `float` | Bid price. |
| `ask_price` | `float` | Ask price. |
| `bid_size` | `float` | Bid size. |
| `ask_size` | `float` | Ask size. |
| `tick_attrib_bid_ask` | `Py<PyAny>` |  |

---

#### `tick_by_tick_mid_point`

One change to the midpoint, as it happens.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `time` | `int` | Tick timestamp (Unix seconds). |
| `mid_point` | `float` | Midpoint price. |

---

#### `scanner_data`

One row of a scan, in rank order.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `rank` | `int` | Scanner result rank (0-based). |
| `contract_details` | `Py<PyAny>` |  |
| `distance` | `str` | Scanner distance metric. |
| `benchmark` | `str` | Scanner benchmark. |
| `projection` | `str` | Scanner projection. |
| `legs_str` | `str` | Combo legs description. |

---

#### `scanner_data_end`

Every row of this scan has been stated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `scanner_parameters`

Every scan the venue offers and what each can be filtered by, as the XML the venue publishes.

| Parameter | Type | Description |
|-----------|------|-------------|
| `xml` | `str` | XML string. |

---

#### `news_providers`

Every news provider this account may read.

| Parameter | Type | Description |
|-----------|------|-------------|
| `news_providers` | `Py<PyAny>` |  |

---

#### `news_article`

The body of one article. `article_type` is 0 for text and 1 for a binary document.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `article_type` | `int` | Article type: 0=plain text, 1=HTML. |
| `article_text` | `str` | Full article body. |

---

#### `historical_news`

One headline from the archive.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `time` | `str` | Tick timestamp (Unix seconds). |
| `provider_code` | `str` | News provider code (e.g. `"BRFG"`). |
| `article_id` | `str` | News article identifier. |
| `headline` | `str` | News headline text. |

---

#### `historical_news_end`

Every headline answering this request has been stated, and whether the archive holds more.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `has_more` | `bool` | If `true`, more results available. |

---

#### `tick_news`

A headline about a contract being watched, as it is published.

| Parameter | Type | Description |
|-----------|------|-------------|
| `ticker_id` | `int` | Ticker/request ID. |
| `time_stamp` | `int` | Timestamp string. |
| `provider_code` | `str` | News provider code (e.g. `"BRFG"`). |
| `article_id` | `str` | News article identifier. |
| `headline` | `str` | News headline text. |
| `extra_data` | `str` | Additional tick data. |

---

#### `update_mkt_depth`

One level of a book that names no venue. `operation` is 0 to insert, 1 to update, 2 to delete; `side` is 0 ask, 1 bid.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `position` | `int` | Book position (row index) or position size. |
| `operation` | `int` | Book operation: 0=insert, 1=update, 2=delete. |
| `side` | `int` | Book side: 0=ask, 1=bid. Or order side `"BOT"`/`"SLD"`. |
| `price` | `float` | Tick price. |
| `size` | `float` | Tick size. |

---

#### `update_mkt_depth_l2`

One level of a book that names the venue it stands on. Every level from this client names one.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `position` | `int` | Book position (row index) or position size. |
| `market_maker` | `str` | Market maker ID. |
| `operation` | `int` | Book operation: 0=insert, 1=update, 2=delete. |
| `side` | `int` | Book side: 0=ask, 1=bid. Or order side `"BOT"`/`"SLD"`. |
| `price` | `float` | Tick price. |
| `size` | `float` | Tick size. |
| `is_smart_depth` | `bool` | If `true`, aggregate depth from multiple exchanges via SMART. |

---

#### `mkt_depth_exchanges`

Every exchange the venue names, in the two sections it names them in: shares and derivatives.

| Parameter | Type | Description |
|-----------|------|-------------|
| `depth_mkt_data_descriptions` | `Py<PyAny>` |  |

---

#### `real_time_bar`

One five-second bar of a live stream.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `date` | `int` | Bar date string. |
| `open` | `float` | Open price. |
| `high` | `float` | High price. |
| `low` | `float` | Low price. |
| `close` | `float` | Close price. |
| `volume` | `float` | Volume. |
| `wap` | `float` | Volume-weighted average price. |
| `count` | `int` | Trade count. |

---

#### `historical_ticks`

Historical midpoints, in batches, until `done`.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `ticks` | `Py<PyAny>` | Historical tick data. |
| `done` | `bool` | If `true`, all ticks have been delivered. |

---

#### `historical_ticks_bid_ask`

Historical quotes, in batches, until `done`.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `ticks` | `Py<PyAny>` | Historical tick data. |
| `done` | `bool` | If `true`, all ticks have been delivered. |

---

#### `historical_ticks_last`

Historical trades, in batches, until `done`.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `ticks` | `Py<PyAny>` | Historical tick data. |
| `done` | `bool` | If `true`, all ticks have been delivered. |

---

#### `tick_option_computation`

The venue's model for an option: the volatility its price implies, the greeks, and the modelled value of the option and its underlying.  Every figure is `None` where the venue stated none. The reference client hands a caller `None` for those, so this surface does too, and a wrapper that has not overridden this is handed the same. Declared as a number, the default refused the call and the exception left the caller's reading loop: a caller who watched an option and did not write this method lost the session on the first model the venue did not fill in.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `tick_type` | `int` | Tick type ID or tick-by-tick type string. |
| `tick_attrib` | `int` |  |
| `implied_vol` | `float or None` | Implied volatility. |
| `delta` | `float or None` | Option delta. |
| `opt_price` | `float or None` | Option theoretical price. |
| `pv_dividend` | `float or None` | Present value of dividends. |
| `gamma` | `float or None` | Option gamma. |
| `vega` | `float or None` | Option vega. |
| `theta` | `float or None` | Option theta. |
| `und_price` | `float or None` | Underlying price. |

---

#### `security_definition_option_parameter`

One venue's option chain for an underlying: the expiries and strikes it lists.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `exchange` | `str` | Exchange name. |
| `underlying_con_id` | `int` | Underlying contract ID. |
| `trading_class` | `str` | Trading class. |
| `multiplier` | `str` | Contract multiplier. |
| `expirations` | `Py<PyAny>` | Available expiration dates. |
| `strikes` | `Py<PyAny>` | Available strike prices. |

---

#### `security_definition_option_parameter_end`

Every venue's chain has been stated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `fundamental_data`

A fundamental report, as the XML the venue publishes.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `data` | `str` | Raw data string (XML/JSON). |

---

#### `update_news_bulletin`

A notice the venue broadcasts to everyone — an exchange unavailable, a system message.

| Parameter | Type | Description |
|-----------|------|-------------|
| `msg_id` | `int` | Bulletin message ID. |
| `msg_type` | `int` | Bulletin message type (1=regular, 2=exchange). |
| `message` | `str` | Bulletin message text. |
| `orig_exchange` | `str` | Originating exchange. |

---

#### `receive_fa`

A partition of an advisor's configuration, as the XML the venue holds it in.

| Parameter | Type | Description |
|-----------|------|-------------|
| `fa_data_type` | `int` | FA data type (1=Groups, 2=Profiles, 3=Aliases). |
| `xml` | `str` | XML string. |

---

#### `replace_fa_end`

An advisor configuration has been replaced.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `text` | `str` | Informational text. |

---

#### `position_multi`

One position, for a request naming an account or a model.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `account` | `str` | Account ID. |
| `model_code` | `str` | Model portfolio code (empty for default). |
| `contract` | `Py<PyAny>` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `pos` | `float` | Position size (decimal shares). |
| `avg_cost` | `float` | Average cost per share. |

---

#### `position_multi_end`

Every position answering this request has been stated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `account_update_multi`

One account figure, for a request naming an account or a model.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `account` | `str` | Account ID. |
| `model_code` | `str` | Model portfolio code (empty for default). |
| `key` | `str` | Account value key (e.g. `"NetLiquidation"`, `"BuyingPower"`). |
| `value` | `str` | Account value. |
| `currency` | `str` | Currency code (e.g. `"USD"`). |

---

#### `account_update_multi_end`

Every figure answering this request has been stated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |

---

#### `display_group_list`

Which display groups exist, as the venue numbers them.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `groups` | `str` | FA group definitions. |

---

#### `display_group_updated`

What a display group is now showing.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract_info` | `str` | Display group contract info string. |

---

#### `market_rule`

The price ladder a contract trades on: each step, and what the price moves in above it.

| Parameter | Type | Description |
|-----------|------|-------------|
| `market_rule_id` | `int` | Market rule ID. |
| `price_increments` | `Py<PyAny>` | Price increment rules `[{low_edge, increment}]`. |

---

#### `smart_components`

Which venue each bit of a quote's exchange mask refers to, and the letter that venue is named by.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `smart_component_map` | `Py<PyAny>` |  |

---

#### `soft_dollar_tiers`

The soft dollar tiers this account may direct commission to.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `tiers` | `Py<PyAny>` | Soft dollar tier list. |

---

#### `family_codes`

The account families this login belongs to.

| Parameter | Type | Description |
|-----------|------|-------------|
| `family_codes` | `Py<PyAny>` |  |

---

#### `histogram_data`

How much traded at each price over a window.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `items` | `Py<PyAny>` | Histogram entries `[(price, count)]`. |

---

#### `user_info`

What the login is entitled to, as the venue states it.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `white_branding_id` | `str` | White branding ID (empty for standard accounts). |

---

#### `wsh_meta_data`

What the corporate-events calendar carries: its event types and the fields each one has, as the JSON the venue publishes.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `data_json` | `str` |  |

---

#### `wsh_event_data`

Events from the corporate-events calendar, as the JSON the venue publishes. Events themselves need a Wall Street Horizon subscription; a login without one is answered with an empty set.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `data_json` | `str` |  |

---

#### `completed_order`

An order that is done — filled, cancelled or expired — as the venue holds it.

| Parameter | Type | Description |
|-----------|------|-------------|
| `contract` | `Py<PyAny>` | Contract specification (symbol, secType, exchange, currency, etc.). |
| `order` | `Py<PyAny>` | Order parameters (action, quantity, type, price, TIF, etc.). |
| `order_state` | `Py<PyAny>` | Order state (status, margin, commission info). |

---

#### `completed_orders_end`

Every completed order has been stated.

---

#### `order_bound`

An order placed elsewhere has been bound to this session, so its changes arrive here.

| Parameter | Type | Description |
|-----------|------|-------------|
| `order_id` | `int` | Order identifier. Must be unique per session. |
| `api_client_id` | `int` |  |
| `api_order_id` | `int` |  |

---

#### `tick_req_params`

What a subscription was given: the increment its prices move in, which venues it is served from, and which feed answered.

| Parameter | Type | Description |
|-----------|------|-------------|
| `ticker_id` | `int` | Ticker/request ID. |
| `min_tick` | `float` | Minimum tick size. |
| `bbo_exchange` | `str` | BBO exchange for smart component lookup (e.g. `"SMART"`). |
| `snapshot_permissions` | `int` | What the venue says this request may be given: 0 nothing stated, 1 no top of book, 2 snapshots, 3 real-time top of book, 4 snapshots not available through the API. |

---

#### `bond_contract_details`

One bond matching a description, with its terms: what it pays, how and when, whether it can be called or put, whether it converts, what it is rated.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `contract_details` | `Py<PyAny>` |  |

---

#### `reroute_mkt_data_req`

The contract a market-data request should be asked for under instead.  A gateway sends it, and asks the venue nothing, for a contract for difference whose own definition asks for its underlying's data, where the account's logon permissions allow that: the underlying's contract and the venue to ask on. This client does not read a definition's flags for asking on the underlying before subscribing: the request goes to the venue as asked, and this never fires.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `con_id` | `int` | Contract ID. Unique per instrument. |
| `exchange` | `str` | Exchange name. |

---

#### `reroute_mkt_depth_req`

The same, for a request for the book rather than the quote.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `con_id` | `int` | Contract ID. Unique per instrument. |
| `exchange` | `str` | Exchange name. |

---

#### `delta_neutral_validation`

The contract the venue paired with a delta-neutral order.  Declared by the TWS API and never fired on a gateway: a gateway never sends it. It fires here as it does there: never.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `delta_neutral_contract` | `Py<PyAny>` |  |

---

#### `tick_efp`

An exchange-for-physical quote. Declared by the TWS API and never fired on a gateway, so it fires here as it does there: never.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `tick_type` | `int` | Tick type ID or tick-by-tick type string. |
| `basis_points` | `float` |  |
| `formatted_basis_points` | `str` |  |
| `implied_future` | `float` |  |
| `hold_days` | `int` |  |
| `future_last_trade_date` | `str` |  |
| `dividend_impact` | `float` |  |
| `dividends_to_last_trade_date` | `float` |  |

---

#### `verify_message_api`

A step in the TWS API's verification handshake. Declared by the TWS API and never fired on a gateway, so these four fire here as they do there: never.

| Parameter | Type | Description |
|-----------|------|-------------|
| `api_data` | `str` |  |

---

#### `verify_completed`

Whether that handshake was accepted.

| Parameter | Type | Description |
|-----------|------|-------------|
| `is_successful` | `bool` |  |
| `error_text` | `str` |  |

---

#### `verify_and_auth_message_api`

The same handshake, where the terminal also authenticates the program.

| Parameter | Type | Description |
|-----------|------|-------------|
| `api_data` | `str` |  |
| `xyz_challenge` | `str` |  |

---

#### `verify_and_auth_completed`

Whether that one was accepted.

| Parameter | Type | Description |
|-----------|------|-------------|
| `is_successful` | `bool` |  |
| `error_text` | `str` |  |

---

#### `win_error`

Declared by the TWS API as `winError`. No message on the wire carries it, and the TWS API's Python client declares it and never raises it, so it fires here as it does there: never. Here, trouble on a connection reaches a caller on the error callback.

| Parameter | Type | Description |
|-----------|------|-------------|
| `text` | `str` | Informational text. |
| `last_error` | `int` |  |

---

#### `historical_schedule`

When a contract's venue was open over a window, session by session, in the zone the venue keeps.

| Parameter | Type | Description |
|-----------|------|-------------|
| `req_id` | `int` | Request identifier. Used to match responses to requests. |
| `start_date_time` | `str` | Start date/time for tick query. |
| `end_date_time` | `str` | End date/time in `"YYYYMMDD HH:MM:SS"` format, or empty for now. |
| `time_zone` | `str` | Timezone string (e.g. `"US/Eastern"`). |
| `sessions` | `Py<PyAny>` | Trading sessions `[(ref_date, open, close)]`. |

---
