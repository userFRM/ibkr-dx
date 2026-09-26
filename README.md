<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/userFRM/ibkr-dx/main/docs/book/src/banner-dark.svg">
    <img src="https://raw.githubusercontent.com/userFRM/ibkr-dx/main/docs/book/src/banner-light.svg" alt="ibkr-dx: a direct connection engine for Interactive Brokers" width="100%">
  </picture>
</p>

<p align="center">
  <strong>An Interactive Brokers client with no gateway. No JVM, no window, no process to keep alive.</strong>
</p>

<p align="center">
  <a href="https://github.com/userFRM/ibkr-dx/actions"><img src="https://github.com/userFRM/ibkr-dx/actions/workflows/tests.yml/badge.svg" alt="Build"></a>
  <img src="https://img.shields.io/badge/rust-1.89+-orange.svg" alt="Rust version">
  <img src="https://img.shields.io/badge/python-3.11+-blue.svg" alt="Python version">
  <a href="https://github.com/userFRM/ibkr-dx/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-blue.svg" alt="License"></a>
  <a href="https://userfrm.github.io/ibkr-dx/"><img src="https://img.shields.io/badge/docs-book-green.svg" alt="Docs"></a>
</p>

> [!TIP]
> For ib_async, use [ib_async-dx](https://github.com/userFRM/ib_async-dx), which runs it on this engine.

## Contents

**Start here** — [Introduction](#introduction) · [Why this exists](#why-this-exists) · [Installation](#installation) · [Quick start](#quick-start)

**What you get** — [Beyond the documented API](#beyond-the-documented-api) · [Capabilities](#capabilities) · [Calls](#calls) · [Callbacks](#callbacks) · [Beyond the canonical list](#beyond-the-canonical-list)

**Moving across** — [Running an existing program](#running-an-existing-program)

**Under the hood** — [How it works](#how-it-works) · [Configuration](#configuration)

**The honest parts** — [What is not covered](#what-is-not-covered) · [Questions](#questions) · [Testing](#testing)

**Working on it** — [Contributing](#contributing) · [Security](#security) · [License](#license) · [Credits](#credits)

## Introduction

ibkr-dx implements the IBKR client protocol directly. It authenticates, maintains
the market-data, trading, historical and security-definition connections, and
exposes the same API a program would otherwise reach through IB Gateway — with
no gateway process, JVM, or local socket in between.

The API has the TWS API's shape: `EClient` for requests, and `EWrapper` (in
Rust, `Wrapper`) for what comes back. Every call and callback on the canonical
list of the TWS API's requests and callbacks is present on both surfaces — the
[Capabilities](#capabilities) table counts them from the source — and the same
`EClient` answers what the gateway never forwarded; see
[Beyond the documented API](#beyond-the-documented-api).

Moving an existing Python program across changes its imports (`ibapi` becomes
`ibkr_dx`) and its connect call:

```diff
- client.connect("127.0.0.1", 4001, clientId=1)     # requires a running gateway
+ client.connect(username="...", password="...")    # no external process
```

> [!TIP]
> The calls, callbacks and order objects keep their names and shapes. Where an
> answer differs from a gateway's, [Limits](https://userfrm.github.io/ibkr-dx/reference/limits.html)
> names the case, and what a gateway answers the same way is under
> [Venue behaviour](https://userfrm.github.io/ibkr-dx/reference/venue-behaviour.html).

## Why this exists

A program that trades through Interactive Brokers normally talks to a local
process — IB Gateway or Trader Workstation — which talks to the venue. That
process is a Java application. It holds a heap, it holds a window unless you
fight it, it has to be logged in, and it has to stay alive for as long as your
program does.

That arrangement costs you four things:

1. **An operational dependency.** Something has to start it, watch it, restart
   it, and log in again when it drops. In a container or over ssh that is work
   you did not want.
2. **A second failure mode.** Your program can be healthy while the thing it
   depends on is wedged, and the two do not agree about it.
3. **A ceiling.** The heap is finite and bulk historical data is what finds the
   edge of it.
4. **A narrower view than the venue gives.** The gateway receives far more from
   the venue than it forwards. Whatever has no message in the documented API
   simply never reaches you.

This client speaks the venue's own protocol, so none of those apply. It
authenticates, holds the connections a session runs on, and answers the same
calls a program already makes — and because it sits where the gateway sat, what
the gateway kept to itself is reachable too.

### What this removes

* **The gateway process** — nothing to install, launch, log into, or restart
* **The JVM** — no Java runtime to install, and no heap to size
* **The localhost socket** — ticks are delivered in-process
* **The window** — runs headless, in a container, over ssh

### Requirements

* An Interactive Brokers account, paper or live
* Rust 1.89+, which both install routes build with
* Python 3.11+ for the bindings

No IB software is required. The `ibapi` package is not needed either.

> [!IMPORTANT]
> A live login enters the venue's second-factor approval, which waits on a
> device. Paper logins do not. Each program opens its own session, and a login
> holds one session at a time: a second program on the same login takes the
> first one's session, and the venue names the host that took it. Give each
> program that runs at the same time a login of its own.

## Installation

Both routes build from the repository.

### Python

```bash
pip install "git+https://github.com/userFRM/ibkr-dx"
```

The package imports as `ibkr_dx`. pip compiles the extension, which needs a
Rust toolchain at 1.89 or newer and, on Linux, the OpenSSL headers and
`pkg-config` (`libssl-dev` on Debian and Ubuntu, `openssl-devel` on Fedora and
RHEL). It builds for CPython 3.11 to 3.14 and the free-threaded 3.14t; the
free-threaded 3.13t is not supported.

### Rust

```toml
[dependencies]
ibkr-dx = { git = "https://github.com/userFRM/ibkr-dx" }
```

The Rust client needs no feature turned on. Rust 1.89 is the minimum supported
version (`rust-version`), and on Linux the build needs the OpenSSL headers and
`pkg-config` as above.

### From the source

For working on the client itself:

```bash
git clone https://github.com/userFRM/ibkr-dx
cd ibkr-dx
uv venv .venv
source .venv/bin/activate          # .venv\Scripts\activate on Windows
uv pip install maturin
maturin develop
```

`pyproject.toml` names the features the extension is built with, so
`maturin develop` needs none. A Rust program can depend on the checkout with
`ibkr-dx = { path = "../ibkr-dx" }`.

## Quick start

```python
import threading
import time
from ibkr_dx import EWrapper, EClient, Contract

class App(EWrapper):
    def __init__(self):
        super().__init__()
        self.ready = threading.Event()

    def next_valid_id(self, order_id):
        self.next_id = order_id
        self.ready.set()

    def tick_price(self, req_id, tick_type, price, attrib):
        print(f"tick {tick_type}: {price}")

app = App()
client = EClient(app)
client.connect(username="your_user", password="your_pass", paper=True)
threading.Thread(target=client.run, daemon=True).start()
app.ready.wait(timeout=10)

aapl = Contract(symbol="AAPL", secType="STK", exchange="SMART", currency="USD")
client.req_mkt_data(1, aapl, "", False)
time.sleep(5)
client.cancel_mkt_data(1)
client.disconnect()
```

Callbacks arrive on the thread that runs `client.run()`, or on the one that
calls `client.poll()`. The three that
`connect()` announces — `connect_ack`, `managed_accounts` and `next_valid_id` —
are fired on the calling thread before it returns. In Rust, `process_msgs`
delivers callbacks on the thread that calls it; after `connect`, it delivers
`next_valid_id` once the venue has named the orders the account is working.
Requests and cancels return after admission; their answers and refusals arrive
in session order. See
[Requests, delivery and shutdown](https://userfrm.github.io/ibkr-dx/reference/requests-and-delivery.html)
for admission, delivery order, error origins and shutdown.

In Rust:

```rust
use ibkr_dx::api::client::{EClient, EClientConfig};
use ibkr_dx::api::types::{Contract, Order};

let client = EClient::connect(&EClientConfig {
    username: std::env::var("IB_USERNAME").unwrap_or_default(),
    password: std::env::var("IB_PASSWORD").unwrap_or_default(),
    paper: true,
    ..Default::default()
})?;

let spy = client.qualify_contract(&Contract {
    symbol: "SPY".into(),
    sec_type: "STK".into(),
    exchange: "SMART".into(),
    currency: "USD".into(),
    ..Default::default()
})?;

let bars = client.historical_data(&spy, "", "2 D", "1 hour", "TRADES", true)?;
let preview = client.what_if_order(&spy, &Order {
    action: "BUY".into(),
    order_type: "LMT".into(),
    total_quantity: 1.0,
    lmt_price: 1.0,
    ..Default::default()
})?;
println!("{} bars, preview {}", bars.len(), preview.status);

client.req_mkt_data(1, &spy, "", false, false);
std::thread::sleep(std::time::Duration::from_secs(2));
if let Some(quote) = client.quote(1) {
    // Prices are held as integers scaled by `PRICE_SCALE`.
    let scale = ibkr_dx::types::PRICE_SCALE as f64;
    println!("bid {:.2} ask {:.2}", quote.bid as f64 / scale, quote.ask as f64 / scale);
}

client.disconnect();
```

## Beyond the documented API

A quote subscription carries far more than prices. The venue publishes well over
a hundred series against a contract, and the documented API forwards a handful
of them — the rest have no message, so a program had no way to ask. This client
sits where the gateway sat and reads them.

Everything below is stated by the venue on an ordinary session. The figures are
from one paper session and will differ with the account and the day; what does
not differ is that none of it is reachable through `ibapi` or `ib_async`.

Each is a call on `EClient`, beside the TWS API's own. The examples are Python;
the Rust `EClient` carries the same calls, spelled as
[this table](https://userfrm.github.io/ibkr-dx/reference/beyond-the-api.html#the-same-calls-in-rust)
gives them.

### What a company is worth, and what it is

```python
client.req_mkt_data(1, aapl, "454,678,705", False, False, [])

client.company_data(265598, 678)   # 212 fields, among them:
#   FLOAT      = 1.4352417904E10      the float, to the share
#   FLOAT_DATE = 20260901
client.company_data(265598, 454)
#   IISHOS     = 14594180000          shares on issue
#   IITPCHL    = 47.7348              percent held by institutions
client.company_data(265598, 705)
#   TRESGS     = 7.10404              how it scores against screening principles
```

> [!TIP]
> The float is the example worth giving: the venue states it outright, to the
> share, on a series the documented API never named.

### Where a contract's volatility stands against its own past

```python
client.numbered_figures(1, 661, fractional=True)
#   [(13, 56.155), (26, 56.155), (52, 56.155)]      implied-volatility rank
client.numbered_figures(1, 664, fractional=True)
#   [(13, 2.436), (26, 6.450), (52, 31.749)]        the same for realised
```

The number each figure is kept under is **weeks** — a quarter, a half year, a
year. Two of these series state a high and a low, and there the window is
written negative for the low and positive for the high, so a year's range is the
pair numbered `-52` and `52`.

### Five years of price history, without a history request

```python
client.numbered_figures(1, 562, fractional=False)   # the date of each point
client.numbered_figures(1, 562, fractional=True)    # the price on it
#   [(7, 765.96), (30, 776.34), … (1826, 419.586)]
```

The key is a span of days: a week, a month, a quarter, a year, three years,
five. It arrives on a quote subscription with no historical-data request behind
it at all.

### Figures with no call anywhere in the API

```python
client.stated_figures(1, 504)    # [383820.0, 1.0]   what one contract delivers
client.stated_figures(1, 527)    # [0.0154349]       volatility over twenty days
client.stated_figures(1, 407)    # four margin figures for a future
```

> [!NOTE]
> On a future, *what one contract delivers* came back as its multiplier times
> its own modelled price; on an option, a hundred times the underlying; on a
> share, the price itself. That is the kind of check every reader here is held
> to — a figure is only claimed once the wire agrees with it.

### The option model the venue states

On tick 13, and on 83 where the feed is delayed, `tick_option_computation`
carries the venue's model as a gateway builds it: the venue's greeks and option
price, the first of its model, mid and last volatilities that stands as the
implied volatility, over a year of 252 trading days, the underlying's price the
chain parameters on the underlying state for the option's expiry, and the
present value of the dividends the option's life covers. Ticks 10, 11 and 12,
and the delayed 80, 81 and 82, carry the model against the bid, the ask and the
last, worked out the way a gateway works it: at the volatility the venue states
for that side, with that side's price, and greeks from this client's own option
model. They are worked from the underlying's price the chain parameters state,
where a gateway takes the underlying's own quote when it holds one, so their
underlying's price and greeks are not a gateway's.
[Limits](https://userfrm.github.io/ibkr-dx/reference/limits.html#option-computation-ticks)
says what they do not carry.

The venue states more on the model's record, and it is all here: **rho**,
**fugit**, the exercise boundary, the forward coefficient, the model and bridge
yields — and the same model again as of the close, kept apart from the standing
one.

```python
m = client.option_model(1)
m["rho"], m["fugit"], m["exerciseBoundary"], m["modelYield"]
client.closing_option_model(1)      # the same, worked out as the contract closed
```

### Asking the venue to find a trade

```python
scan = ibkr_dx.SpreadScan(version=6, under_con_id=265598,
                          account="DU1234567", min_delta=0.25)
client.req_spread_scan(1, aapl, scan)
for s in client.scanned_strategies(1):
    s["legs"], s["figures"], s["breakEvens"]
```

The same fields in Rust:

```rust
let aapl = client.qualify_contract(&Contract {
    symbol: "AAPL".into(),
    sec_type: "STK".into(),
    exchange: "SMART".into(),
    currency: "USD".into(),
    ..Default::default()
})?;
let scan = ibkr_dx::types::SpreadScan {
    version: 6, under_con_id: aapl.con_id,
    account: "DU1234567".into(), min_delta: Some(0.25),
    ..Default::default()
};
client.req_spread_scan(1, &aapl, &scan);
for s in client.scanned_strategies(1) {
    println!("{:?} {:?} {:?}", s.legs, s.figures, s.break_evens);
}
```

### And the session itself

```python
client.enabled_features()          # what the venue permits this account
client.order_permissions()         # which order types, per security type
client.algorithms_for("STK")       # which algorithms it may use
client.order_presets()             # the defaults it fills an order's blanks from
client.req_ping(); client.last_rtt_ms()
```

The full list, with what each returns, is in
[Beyond the API](https://userfrm.github.io/ibkr-dx/reference/beyond-the-api.html).

> [!TIP]
> The calls under [Beyond the canonical list](#beyond-the-canonical-list) are
> the point of this client. The venue states all of it on an ordinary session
> and the documented API never gave it a message, so a program had no way to
> ask — a contract's float, where its volatility stands against its own year,
> what margin it takes, five years of its price with the date of each point.
> They are reached here the way every other call is.

<!-- capabilities:begin — written by scripts/gen_parity_matrix.py -->

## Capabilities

One row per capability, one column per client — every one of the 87 calls and 90 callbacks on the canonical list of the TWS API's requests and callbacks, read from each client rather than recalled.

| Client | Calls | Callbacks | |
| --- | ---: | ---: | --- |
| TWS API | 87 / 87 | 90 / 90 | nothing missing |
| ibapi | 78 / 87 | 85 / 90 | 9 absent, 5 callbacks absent |
| ib_async | 84 / 87 | 78 / 90 | 3 absent, 12 callbacks absent |
| **ibkr-dx Rust** | **87 / 87** | **88 / 90** | 2 callbacks declared, not fired |
| **ibkr-dx Python** | **87 / 87** | **88 / 90** | 2 callbacks declared, not fired |

**Every call and callback on that list is present on both surfaces.** 2 callbacks are declared and not fired: a gateway reroutes a request to another contract for a contract for difference whose definition asks for it, and this client does not read a definition's flags for that before subscribing: the request goes to the venue as asked. Each says so where it is declared, so a program that implements one still compiles and runs. 7 more are declared by the TWS API and never fire on a gateway — the four steps of the verification handshake, the exchange-for-physical quote, the delta-neutral validation, which a gateway never sends, and `win_error`, which no message on the wire carries — so they fire here exactly as often as there: never.

**And 84 more beyond that list.** The venue states more on a session than the documented calls ask for — what it permits this account, which algorithms it offers, the order defaults it fills an order's blanks from, what it says about an issuer, which session holds the account — and this client answers for those too, beside helpers and instrumentation of its own. The table under *Beyond the canonical list* says which each is, and which a reference client also names.

Every figure here is read from the client it names, on the machine that generated it. A client that is not installed is left out rather than filled in from memory.

<details>
<summary><b>The whole table — every call, every callback, and the calls beyond them</b></summary>


One row per capability, one column per client. This client exists to be
put where a gateway was, so the question is not what it can do but
whether anything you already use is missing — which is a question about
the columns, not about the rows.

The first two tables are the canonical list of the TWS API's calls and
callbacks, which are the rows a port has to match. The last is what this
client answers beyond them.

Presence and behaviour are marked apart. A reference client's mark says a
method exists, read by importing the package and listing its methods;
it says nothing about what the method does. This client's mark says what
the call does, and the evidence column says how that was established.

| Mark | Meaning |
| :---: | --- |
| ● | Present. For ibkr-dx, also served: a call does what it names; a callback is fired whenever what it reports arrives |
| ◐ | Present and not served: a call reports why on the error callback; a callback is declared and not fired here, although a gateway sends it |
| · | Absent |
| — | Beyond the canonical list: not on this surface by design. The call is the other surface's own convenience, not one this surface lacks |

Each column is read from the client it names, on the machine that
generated this page:

- **TWS API** — the canonical list this repository keeps of the TWS API's requests and callbacks, in `scripts/gen_api_docs.py`. Beyond that list, a call is marked here where the TWS API's own client, or ib_async's transport, has a method by that name; a helper of ib_async's facade is not.
- **ibapi** — IBKR's own Python client, version `9.81.1-1`, imported and enumerated. This is the copy published to PyPI; the version IBKR distributes directly is numbered 10.x and names calls this one predates, so a gap in this column is a gap in the copy that was read and not necessarily in the client you have.
- **ib_async** — version `2.1.0`, imported and enumerated across both the transport and the facade, because it carries some calls on one and some on the other.
- **ibkr-dx Rust** and **ibkr-dx Python** — this client's two surfaces, from the coverage matrix `scripts/gen_api_docs.py` generates from the source. A mark here says what the call does, not only that it exists.
- **Evidence** — how this client's status for a call was established: named by a suite that opens a session, named only by the offline suites, or not named by a test.
- **Fires on a gateway** — whether the callback fires at all for a program on a gateway. The TWS API declares seven that never do.
- **Answered from** — beyond the canonical list, whether a call asks the venue or reads what it stated, or answers from this client itself: its own state, a measurement it takes, or a helper.

A client that is not installed is left out of the table rather than
filled in from memory. A mark here is a thing that was read.

## Calls

What a program asks the venue for.

| Category | Call | TWS API | ibapi | ib_async | ibkr-dx Rust | ibkr-dx Python | Evidence |
| --- | --- | :---: | :---: | :---: | :---: | :---: | :---: |
| Connection | `connect` | ● | ● | ● | ● | ● | Live session |
|  | `disconnect` | ● | ● | ● | ● | ● | Live session |
|  | `is_connected` | ● | ● | ● | ● | ● | Live session |
|  | `start_api` | ● | ● | ● | ● | ● | Offline suites |
|  | `set_server_log_level` | ● | ● | ● | ● | ● | Live session |
|  | `req_current_time` | ● | ● | ● | ● | ● | Live session |
|  | `req_current_time_in_millis` | ● | · | · | ● | ● | Live session |
|  | `verify_request` | ● | ● | ● | ● | ● | Offline suites |
|  | `verify_message` | ● | ● | ● | ● | ● | Offline suites |
|  | `verify_and_auth_request` | ● | ● | ● | ● | ● | Offline suites |
|  | `verify_and_auth_message` | ● | ● | ● | ● | ● | Offline suites |
| Market Data | `req_mkt_data` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_mkt_data` | ● | ● | ● | ● | ● | Live session |
|  | `req_market_data_type` | ● | ● | ● | ● | ● | Live session |
|  | `req_tick_by_tick_data` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_tick_by_tick_data` | ● | ● | ● | ● | ● | Live session |
|  | `req_mkt_depth` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_mkt_depth` | ● | ● | ● | ● | ● | Live session |
|  | `req_mkt_depth_exchanges` | ● | ● | ● | ● | ● | Live session |
|  | `req_smart_components` | ● | ● | ● | ● | ● | Live session |
|  | `req_real_time_bars` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_real_time_bars` | ● | ● | ● | ● | ● | Live session |
| Historical Data | `req_historical_data` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_historical_data` | ● | ● | ● | ● | ● | Live session |
|  | `req_head_time_stamp` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_head_time_stamp` | ● | ● | ● | ● | ● | Live session |
|  | `req_historical_ticks` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_historical_ticks` | ● | · | · | ● | ● | Offline suites |
|  | `req_histogram_data` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_histogram_data` | ● | ● | ● | ● | ● | Live session |
|  | `req_historical_schedule` | ● | · | ● | ● | ● | Live session |
| Orders | `place_order` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_order` | ● | ● | ● | ● | ● | Live session |
|  | `req_open_orders` | ● | ● | ● | ● | ● | Live session |
|  | `req_all_open_orders` | ● | ● | ● | ● | ● | Live session |
|  | `req_auto_open_orders` | ● | ● | ● | ● | ● | Live session |
|  | `req_ids` | ● | ● | ● | ● | ● | Live session |
|  | `req_global_cancel` | ● | ● | ● | ● | ● | Live session |
|  | `req_completed_orders` | ● | ● | ● | ● | ● | Live session |
| Executions | `req_executions` | ● | ● | ● | ● | ● | Live session |
| Account | `req_account_updates` | ● | ● | ● | ● | ● | Live session |
|  | `req_account_summary` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_account_summary` | ● | ● | ● | ● | ● | Live session |
|  | `req_positions` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_positions` | ● | ● | ● | ● | ● | Live session |
|  | `req_pnl` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_pnl` | ● | ● | ● | ● | ● | Live session |
|  | `req_pnl_single` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_pnl_single` | ● | ● | ● | ● | ● | Live session |
|  | `req_managed_accts` | ● | ● | ● | ● | ● | Live session |
|  | `req_account_updates_multi` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_account_updates_multi` | ● | ● | ● | ● | ● | Live session |
|  | `req_positions_multi` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_positions_multi` | ● | ● | ● | ● | ● | Live session |
| Contract | `req_contract_details` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_contract_data` | ● | · | · | ● | ● | Offline suites |
|  | `req_matching_symbols` | ● | ● | ● | ● | ● | Live session |
|  | `req_market_rule` | ● | ● | ● | ● | ● | Live session |
| Scanner | `req_scanner_parameters` | ● | ● | ● | ● | ● | Live session |
|  | `req_scanner_subscription` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_scanner_subscription` | ● | ● | ● | ● | ● | Live session |
| News | `req_news_providers` | ● | ● | ● | ● | ● | Live session |
|  | `req_news_article` | ● | ● | ● | ● | ● | Live session |
|  | `req_historical_news` | ● | ● | ● | ● | ● | Live session |
|  | `req_news_bulletins` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_news_bulletins` | ● | ● | ● | ● | ● | Live session |
| Fundamental | `req_fundamental_data` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_fundamental_data` | ● | ● | ● | ● | ● | Live session |
| Options | `calculate_implied_volatility` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_calculate_implied_volatility` | ● | ● | ● | ● | ● | Live session |
|  | `calculate_option_price` | ● | ● | ● | ● | ● | Live session |
|  | `cancel_calculate_option_price` | ● | ● | ● | ● | ● | Live session |
|  | `exercise_options` | ● | ● | ● | ● | ● | Live session |
|  | `req_sec_def_opt_params` | ● | ● | ● | ● | ● | Live session |
| Reference | `req_soft_dollar_tiers` | ● | ● | ● | ● | ● | Live session |
|  | `req_family_codes` | ● | ● | ● | ● | ● | Live session |
|  | `req_user_info` | ● | · | ● | ● | ● | Live session |
| Financial Advisor | `request_fa` | ● | ● | ● | ● | ● | Offline suites |
|  | `replace_fa` | ● | ● | ● | ● | ● | Offline suites |
| Display Groups | `query_display_groups` | ● | ● | ● | ● | ● | Live session |
|  | `subscribe_to_group_events` | ● | ● | ● | ● | ● | Live session |
|  | `unsubscribe_from_group_events` | ● | ● | ● | ● | ● | Live session |
|  | `update_display_group` | ● | ● | ● | ● | ● | Live session |
| WSH | `req_wsh_meta_data` | ● | · | ● | ● | ● | Live session |
|  | `cancel_wsh_meta_data` | ● | · | ● | ● | ● | Live session |
|  | `req_wsh_event_data` | ● | · | ● | ● | ● | Live session |
|  | `cancel_wsh_event_data` | ● | · | ● | ● | ● | Live session |

## Callbacks

What the venue says back. `ib_async` delivers these as events as well as methods, so a mark here says the method exists on its wrapper, not that the information is unavailable by another route.

| Category | Call | TWS API | Fires on a gateway | ibapi | ib_async | ibkr-dx Rust | ibkr-dx Python |
| --- | --- | :---: | :---: | :---: | :---: | :---: | :---: |
| Connection | `connect_ack` | ● | yes | ● | ● | ● | ● |
|  | `connection_closed` | ● | yes | ● | ● | ● | ● |
|  | `next_valid_id` | ● | yes | ● | ● | ● | ● |
|  | `managed_accounts` | ● | yes | ● | ● | ● | ● |
|  | `error` | ● | yes | ● | ● | ● | ● |
|  | `current_time` | ● | yes | ● | ● | ● | ● |
|  | `current_time_in_millis` | ● | yes | · | · | ● | ● |
| Market Data | `tick_price` | ● | yes | ● | · | ● | ● |
|  | `tick_size` | ● | yes | ● | ● | ● | ● |
|  | `tick_string` | ● | yes | ● | ● | ● | ● |
|  | `tick_generic` | ● | yes | ● | ● | ● | ● |
|  | `tick_snapshot_end` | ● | yes | ● | ● | ● | ● |
|  | `market_data_type` | ● | yes | ● | ● | ● | ● |
|  | `tick_req_params` | ● | yes | ● | ● | ● | ● |
| Orders | `order_status` | ● | yes | ● | ● | ● | ● |
|  | `open_order` | ● | yes | ● | ● | ● | ● |
|  | `open_order_end` | ● | yes | ● | ● | ● | ● |
|  | `order_bound` | ● | yes | ● | ● | ● | ● |
| Executions | `exec_details` | ● | yes | ● | ● | ● | ● |
|  | `exec_details_end` | ● | yes | ● | ● | ● | ● |
|  | `commission_and_fees_report` | ● | yes | ● | ● | ● | ● |
| Account | `update_account_value` | ● | yes | ● | ● | ● | ● |
|  | `update_portfolio` | ● | yes | ● | ● | ● | ● |
|  | `update_account_time` | ● | yes | ● | ● | ● | ● |
|  | `account_download_end` | ● | yes | ● | ● | ● | ● |
|  | `account_summary` | ● | yes | ● | ● | ● | ● |
|  | `account_summary_end` | ● | yes | ● | ● | ● | ● |
|  | `position` | ● | yes | ● | ● | ● | ● |
|  | `position_end` | ● | yes | ● | ● | ● | ● |
|  | `pnl` | ● | yes | ● | ● | ● | ● |
|  | `pnl_single` | ● | yes | ● | ● | ● | ● |
|  | `position_multi` | ● | yes | ● | ● | ● | ● |
|  | `position_multi_end` | ● | yes | ● | ● | ● | ● |
|  | `account_update_multi` | ● | yes | ● | ● | ● | ● |
|  | `account_update_multi_end` | ● | yes | ● | ● | ● | ● |
| Contract | `contract_details` | ● | yes | ● | ● | ● | ● |
|  | `contract_details_end` | ● | yes | ● | ● | ● | ● |
|  | `bond_contract_details` | ● | yes | ● | ● | ● | ● |
|  | `symbol_samples` | ● | yes | ● | ● | ● | ● |
| Historical Data | `historical_data` | ● | yes | ● | ● | ● | ● |
|  | `historical_data_end` | ● | yes | ● | ● | ● | ● |
|  | `historical_data_update` | ● | yes | ● | ● | ● | ● |
|  | `head_timestamp` | ● | yes | ● | ● | ● | ● |
|  | `historical_ticks` | ● | yes | ● | ● | ● | ● |
|  | `historical_ticks_bid_ask` | ● | yes | ● | ● | ● | ● |
|  | `historical_ticks_last` | ● | yes | ● | ● | ● | ● |
|  | `histogram_data` | ● | yes | ● | ● | ● | ● |
|  | `historical_schedule` | ● | yes | · | ● | ● | ● |
| Market Depth | `update_mkt_depth` | ● | yes | ● | ● | ● | ● |
|  | `update_mkt_depth_l2` | ● | yes | ● | ● | ● | ● |
|  | `mkt_depth_exchanges` | ● | yes | ● | ● | ● | ● |
| Tick-by-Tick | `tick_by_tick_all_last` | ● | yes | ● | ● | ● | ● |
|  | `tick_by_tick_bid_ask` | ● | yes | ● | ● | ● | ● |
|  | `tick_by_tick_mid_point` | ● | yes | ● | ● | ● | ● |
| Scanner | `scanner_data` | ● | yes | ● | ● | ● | ● |
|  | `scanner_data_end` | ● | yes | ● | ● | ● | ● |
|  | `scanner_parameters` | ● | yes | ● | ● | ● | ● |
| News | `news_providers` | ● | yes | ● | ● | ● | ● |
|  | `news_article` | ● | yes | ● | ● | ● | ● |
|  | `historical_news` | ● | yes | ● | ● | ● | ● |
|  | `historical_news_end` | ● | yes | ● | ● | ● | ● |
|  | `tick_news` | ● | yes | ● | ● | ● | ● |
|  | `update_news_bulletin` | ● | yes | ● | ● | ● | ● |
| Real-Time Bars | `real_time_bar` | ● | yes | ● | ● | ● | ● |
| Fundamental | `fundamental_data` | ● | yes | ● | ● | ● | ● |
| Market Rules | `market_rule` | ● | yes | ● | ● | ● | ● |
| Completed Orders | `completed_order` | ● | yes | ● | ● | ● | ● |
|  | `completed_orders_end` | ● | yes | ● | ● | ● | ● |
| Options | `tick_option_computation` | ● | yes | ● | ● | ● | ● |
|  | `security_definition_option_parameter` | ● | yes | ● | ● | ● | ● |
|  | `security_definition_option_parameter_end` | ● | yes | ● | ● | ● | ● |
| Reference | `smart_components` | ● | yes | ● | ● | ● | ● |
|  | `soft_dollar_tiers` | ● | yes | ● | ● | ● | ● |
|  | `family_codes` | ● | yes | ● | ● | ● | ● |
|  | `user_info` | ● | yes | · | ● | ● | ● |
| FA | `receive_fa` | ● | yes | ● | ● | ● | ● |
|  | `replace_fa_end` | ● | yes | ● | · | ● | ● |
| Display Groups | `display_group_list` | ● | yes | ● | · | ● | ● |
|  | `display_group_updated` | ● | yes | ● | · | ● | ● |
| Other | `delta_neutral_validation` | ● | no | ● | ● | ● | ● |
| WSH | `wsh_meta_data` | ● | yes | · | ● | ● | ● |
|  | `wsh_event_data` | ● | yes | · | ● | ● | ● |
| Market Data | `reroute_mkt_data_req` | ● | yes | ● | · | ◐ | ◐ |
|  | `reroute_mkt_depth_req` | ● | yes | ● | · | ◐ | ◐ |
|  | `tick_efp` | ● | no | ● | ● | ● | ● |
| Connection | `verify_message_api` | ● | no | ● | · | ● | ● |
|  | `verify_completed` | ● | no | ● | · | ● | ● |
|  | `verify_and_auth_message_api` | ● | no | ● | · | ● | ● |
|  | `verify_and_auth_completed` | ● | no | ● | · | ● | ● |
|  | `win_error` | ● | no | ● | · | ● | ● |

## Beyond the canonical list

What this client answers that the canonical list does not name.
Three kinds, told apart by the *Answered from* column and by the
reference columns: what the venue states that no documented call
asks for (what it permits this account, which algorithms it offers,
the order defaults it holds, what it says about an issuer, which
session holds the account); a question answered in one call rather
than delivered on a callback; and this client's own state, helpers
and instrumentation, which are about the client and not the venue.

A mark against a reference client here means it names the same
thing; a mark under TWS API means the TWS API's own client, or
ib_async's transport, has a method by that name.

| Call | Answered from | TWS API | ibapi | ib_async | ibkr-dx Rust | ibkr-dx Python |
| --- | :---: | :---: | :---: | :---: | :---: | :---: |
| `account` / `account_snapshot` | venue | · | · | · | ● | ● |
| `account_id` / `get_account_id` | venue | · | · | · | ● | ● |
| `adjustments` | venue | · | · | · | ● | · |
| `adjustmentsFor` | venue | · | · | · | ● | ● |
| `algorithms` | venue | · | · | · | ● | ● |
| `algorithmsFor` | venue | · | · | · | ● | ● |
| `await_order` | venue | · | · | · | ● | — |
| `backlog` | this client | · | · | · | ● | ● |
| `calendarEvents` | venue | · | · | · | ● | ● |
| `calendarSchema` | venue | · | · | · | ● | ● |
| `cancelAdjustments` | venue | · | · | · | ● | ● |
| `cancelHistoricalNews` | venue | · | · | · | ● | ● |
| `cancelOrderByPermId` | venue | · | · | · | ● | ● |
| `ccpSessionId` | venue | · | · | · | ● | ● |
| `chainModelParameters` | venue | · | · | · | ● | ● |
| `checkConnected` | this client | · | · | · | — | ● |
| `closingOptionModel` | venue | · | · | · | ● | ● |
| `closingOptionModelByInstrument` | venue | · | · | · | ● | ● |
| `companyData` | venue | · | · | · | ● | ● |
| `companyDataSeries` | venue | · | · | · | ● | ● |
| `competingSession` | venue | · | · | · | ● | ● |
| `contractFigures` | venue | · | · | · | ● | ● |
| `contractFiguresByInstrument` | venue | · | · | · | ● | ● |
| `corporateActions` | venue | · | · | · | ● | ● |
| `enabledFeatures` | venue | · | · | · | ● | ● |
| `error_from` | this client | · | · | · | ● | ● |
| `eventsLost` | this client | · | · | · | ● | ● |
| `instrumentOf` | this client | · | · | · | ● | ● |
| `last_rtt` / `last_rtt_ms` | this client | · | · | · | ● | ● |
| `matchingSymbols` | venue | · | · | · | ● | ● |
| `miscUrl` | venue | · | · | · | ● | ● |
| `newsHeadlines` | venue | · | · | · | ● | ● |
| `nextOrderId` | this client | · | · | · | ● | ● |
| `nextSharedId` | this client | · | · | · | ● | ● |
| `next_shared_id_within` | venue | · | · | · | ● | · |
| `numberedFigures` | venue | · | · | · | ● | ● |
| `numberedFiguresSeries` | venue | · | · | · | ● | ● |
| `on_data` | venue | · | · | · | ● | · |
| `option_chain` / `option_chains` | venue | · | · | · | ● | ● |
| `optionModel` | venue | · | · | · | ● | ● |
| `optionModelByInstrument` | venue | · | · | · | ● | ● |
| `order_id_floor` | venue | · | · | · | ● | · |
| `orderPermissions` | venue | · | · | · | ● | ● |
| `orderPresets` | venue | · | · | · | ● | ● |
| `pairedFigures` | venue | · | · | · | ● | ● |
| `pairedFiguresSeries` | venue | · | · | · | ● | ● |
| `parse_algo_params` | this client | · | · | · | ● | — |
| `permittedOrderTypes` | venue | · | · | · | ● | ● |
| `poll` | this client | · | · | · | · | ● |
| `positions` | venue | · | · | ● | ● | ● |
| `positionsElsewhere` | venue | · | · | · | ● | ● |
| `qualifyContract` | venue | · | · | · | ● | ● |
| `qualifyContracts` | venue | · | · | ● | ● | ● |
| `question_retired` | this client | · | · | · | ● | — |
| `quote` | venue | · | · | · | ● | ● |
| `quoteByInstrument` | venue | · | · | · | ● | ● |
| `refuse` | this client | · | · | · | ● | ● |
| `reqAdjustments` | venue | · | · | · | ● | ● |
| `req_config` / `req_config_proto_buf` | this client | · | · | · | ◐ | ◐ |
| `reqMktDataEx` | venue | · | · | · | ● | ● |
| `reqPing` | venue | · | · | · | ● | ● |
| `reqSpreadScan` | venue | · | · | · | ● | ● |
| `reset` | this client | ● | ● | ● | · | ● |
| `run` | this client | ● | ● | ● | · | ● |
| `scan` | venue | · | · | · | ● | ● |
| `scannedStrategies` | venue | · | · | · | ● | ● |
| `schedule` / `trading_schedule` | venue | · | · | · | ● | ● |
| `serverVersion` | this client | ● | ● | ● | ● | ● |
| `sessionOver` | this client | · | · | · | ● | ● |
| `setConnectionOptions` | this client | ● | ● | ● | · | ◐ |
| `setNewsProviders` | venue | · | · | · | ● | ● |
| `shortSaleRestricted` | venue | · | · | · | ● | ● |
| `shortSaleRestrictedByInstrument` | venue | · | · | · | ● | ● |
| `statedFigures` | venue | · | · | · | ● | ● |
| `statedFiguresSeries` | venue | · | · | · | ● | ● |
| `statedRows` | venue | · | · | · | ● | ● |
| `statedRowsSeries` | venue | · | · | · | ● | ● |
| `traffic` | venue | · | · | · | ● | ● |
| `twsConnectionTime` | venue | ● | ● | · | ● | ● |
| `unreadWire` | this client | · | · | · | ● | ● |
| `update_config` / `update_config_proto_buf` | this client | · | · | · | ◐ | ◐ |
| `valuesElsewhere` | venue | · | · | · | ● | ● |
| `waitForData` | this client | · | · | · | ● | ● |
| `whatIfOrder` | venue | · | · | ● | ● | ● |

## Calls, counted

| Client | Present ● | Present, not served ◐ | Absent · |
| --- | ---: | ---: | ---: |
| TWS API | 87 | 0 | 0 |
| ibapi | 78 | 0 | 9 |
| ib_async | 84 | 0 | 3 |
| ibkr-dx Rust | 87 | 0 | 0 |
| ibkr-dx Python | 87 | 0 | 0 |

</details>

The same table stands on its own in [docs/capabilities.md](https://github.com/userFRM/ibkr-dx/blob/main/docs/capabilities.md), and what each claim rests on is in [docs/evidence.md](https://github.com/userFRM/ibkr-dx/blob/main/docs/evidence.md).

<!-- capabilities:end -->

## What is not covered

Everything this client reads is listed above. This is the other half of that
list, so nobody has to discover it by finding an empty result.

> [!NOTE]
> **Six series are read but have never been seen.** `490`, `546`, `669`, `700`,
> `726` and `733` — the price-distribution weights, the volatility curve, the
> historical ratios, the two screening dashboards, and the option model as of
> the close. Each subscribes cleanly: the venue acknowledges it, assigns it a
> tag, and then states nothing. Measured the same on a paper and a live login
> for one account, minutes apart. The readers are written and tested; they fill
> in the moment the data flows. The same is true of the spread scan.

> [!NOTE]
> **Some readers have not met their instrument.** The series for bonds,
> municipals, warrants and perpetuals are read from the record's own shape and
> have not been exercised against one of those contracts. Where a reader has
> been run against live data, it says so in its own documentation.

> [!IMPORTANT]
> **An empty result means one of two things:** the venue holds nothing for that
> contract, or the account cannot see that series. `tick_req_params` carries
> what the venue states for each request: the exchange the best bid and offer
> come from, as written (with the contract's security type appended as four hex
> digits where the name is four characters or fewer, as a gateway writes it),
> and the permission number the venue gives the request (0 nothing stated, 1 no
> top of book, 2 snapshots, 3 real-time top of book, 4 snapshots not available
> through the API; any other number is not kept, as a gateway keeps none). The
> exchange is the name `req_smart_components` answers to. Where the
> acknowledgement names no exchange, as a currency's, a crypto's or a bond's
> does, the exchange is empty. The number is 0 there, on a bond, a bill, a
> fixed-income contract or a combination, and while the frozen quote is served
> in place of the live one, whatever the venue stated, as a gateway states it. Whether that number differs between a series the account
> is not entitled to and one with nothing to say has not been seen yet, and a
> gateway may report 0 for some other contracts whatever the venue stated;
> until a capture settles both, the subscription list in account management is
> what separates them.

And two things this client does not read or fire:

| | Why |
| --- | --- |
| One series | 230, a portfolio figure, is declared by the venue and read by neither this client nor a gateway: a gateway has no reader for it and refuses it in a generic tick list. What arrives on it is kept as unread rather than dropped. |
| Two callbacks | Declared so a program written against the TWS API still compiles, and not fired: a gateway reroutes a request for a contract for difference whose definition asks for its underlying's data, and this client does not read a definition's flags for that before subscribing — the request goes to the venue as asked. |

## Running an existing program

A program written against the TWS API keeps its calls, its callbacks and its
order objects; its imports and its connect call change — the Python
[Quick start](#quick-start) is one. In Python, both naming conventions resolve
on `EClient`, `EWrapper` and the records a call or callback hands over:
`reqMktData` and `req_mkt_data`, `secType` and `sec_type`, `conId` and
`con_id`; the classes a program only builds, such as `ExecutionFilter`, carry
the TWS API's spelling alone. The TWS API's module layout resolves under
`ibkr_dx` (`ibkr_dx.client`, `ibkr_dx.wrapper`, `ibkr_dx.contract`, …).

A wrapper is called as the ibapi release it was written for calls it. An
`error` declared as `(reqId, errorCode, errorString)` (ibapi 9.81), with
`advancedOrderRejectJson` after them, or with `errorTime` second as the current
release has it receives those arguments, and a wrapper that declares
`commissionReport` and not `commissionAndFeesReport` receives each charge there.

Where this client answers differently from a gateway,
[Limits](https://userfrm.github.io/ibkr-dx/reference/limits.html) says so, case
by case.

For [ib_async](https://github.com/ib-api-reloaded/ib_async), use
[ib_async-dx](https://github.com/userFRM/ib_async-dx), which runs it on this
engine.

## How it works

There is no process between your program and the venue. The client opens the
connections a session runs on and keeps them:

| Connection | Carries |
| --- | --- |
| Trading | Orders, executions, positions, account values, news bulletins |
| Market data | Quotes, depth, the extra series, tick-by-tick |
| Historical | Bars, historical ticks, head timestamps |
| Security definition | Contract details, option chains, matching symbols |

Each is authenticated at logon, kept alive, and rebuilt on its own if it drops —
a subscription the venue was serving is asked for again, under the same request
the caller made.

> [!NOTE]
> Reconnection is per connection, not per session. A market-data connection that
> drops does not take the trading connection with it, and an order in flight is
> not orphaned by a quote feed reconnecting.

**A quote is a value, not a stream of events.** The client keeps the current
state of every contract it watches in a lock-free table and hands you a snapshot
when you ask. Callers who want events get them too — the point is that reading a
quote does not mean draining a queue first.

**The figures are the venue's.** A price, size, greek or series figure a caller
reads is the one the venue stated, and a figure the venue holds nothing for
comes back as the largest number its field carries — the venue's own way of
saying so — rather than as a zero that reads like a price. Where this client
works something out itself, the call that does it says so: the P&L figures,
worked out from the venue's figures against the session's prices; the bars
that continue a `keepUpToDate` request, formed from the venue's five-second
bars on the history's last bar, a week and a month on the calendar; a book kept
at the size asked for; and the answer to a price or a volatility the caller
supplies, solved against the venue's published model.

### Second factor and sessions

> [!IMPORTANT]
> A live login enters the venue's second-factor approval and waits on a device.
> Paper logins do not. **One program per login**: each program opens its own
> session, and a login holds one session at a time, so a second program on the
> same login takes the first one's session, and the venue names the host that
> took it.

Within a running program, a dropped connection is rebuilt on the session
already open, with no second factor. A restarted program logs in again, second
factor included. Offering the saved session (`EClientConfig::resume`, or
`session_file`) has not spared a login. See
[Login](https://userfrm.github.io/ibkr-dx/recipes/python/login.html).

## Configuration

The gateway's configuration file is replaced by settings on the client:
announced build, time zone, execution-report scope, and others — 17 in total,
readable at runtime. Fifteen gateway settings are not settings here, and each says
why or names what stands in for it (no window geometry, no local listening socket,
no JVM heap, and no message pacing: a gateway paces requests at the rate its logon
states (fifty a second where it states none) unless it is set to reject them
instead, and nothing here does either).

Rust: `EClientConfig.gateway`. Python: `ibkr_dx.configure()`.

Order IDs survive restarts in `ibkr-dx/order-ids.json` under the user's data
directory: `$XDG_DATA_HOME` (or `~/.local/share`) on Linux,
`~/Library/Application Support` on macOS, and `%APPDATA%` on Windows.
The file keeps a separate next ID for each account and API client ID (the one
stated at connect), keyed by the SHA-256 digest of the account rather than the
account number; a file keyed by account numbers is rewritten that way when a
session opens it. `next_order_id()` saves a reservation under an exclusive lock
before returning; a fresh session starts above that counter and venue replay.
A storage failure warns once in the log and leaves trading available with the
session's in-memory counter and replay floor.

Set `order_id_file` through `EClientConfig.gateway`, Python
`connect(settings={"order_id_file": "/path/order-ids.json"})`, or
`ibkr_dx.configure(order_id_file="/path/order-ids.json")`; the environment name
is `IBKR_DX_ORDER_ID_FILE`. An empty string disables persistence. To move it,
stop every process using it, move the JSON file, and configure the same new
path in each process. The `.lock` and `.tmp` files beside it are maintained by
the client. See [order IDs](https://userfrm.github.io/ibkr-dx/reference/venue-behaviour.html#order-ids-across-sessions)
for the reservation scope.

Registration is held by the engine and does not wait at the call.

Configuration read and update requests through `reqConfigProtoBuf` and
`updateConfigProtoBuf` (Rust: `req_config` and `update_config`) report 10357
on every connected session. Configuration payloads are never applied; see
[Limits](https://userfrm.github.io/ibkr-dx/reference/limits.html#configuration-requests).

## Documentation

* [The book](https://userfrm.github.io/ibkr-dx/) — guides, recipes and the generated API reference
* [Capabilities](https://github.com/userFRM/ibkr-dx/blob/main/docs/capabilities.md) — one row per capability, one column per client
* [Evidence](https://github.com/userFRM/ibkr-dx/blob/main/docs/evidence.md) — what each claim rests on, and the session that produced it
* [Notebooks](https://github.com/userFRM/ibkr-dx/tree/main/notebooks) — seven walkthroughs of `EClient` and `EWrapper`: connecting and the account, bars, contract details, depth, orders, scanners and ticks
* [Examples](https://github.com/userFRM/ibkr-dx/tree/main/examples) — 42 runnable single-file programs, 27 in Rust and 15 in Python
* [Beyond the API](https://userfrm.github.io/ibkr-dx/reference/beyond-the-api.html) — what the session states that no documented call asks for
* [Venue behaviour](https://userfrm.github.io/ibkr-dx/reference/venue-behaviour.html) — what a program meets here that is the venue's answer or the TWS API's definition
* [Limits](https://userfrm.github.io/ibkr-dx/reference/limits.html) — what this client will not do, and why

## Questions

<details>
<summary><b>Do I still need IB Gateway or TWS installed?</b></summary>

No. Nothing is installed, launched or logged into. The `ibapi` package is not
needed either — this client provides that surface itself.
</details>

<details>
<summary><b>What changes in an existing program?</b></summary>

The imports and the connect call. The calls, the callbacks and the order
objects keep their names and shapes. Where an answer differs from a gateway's,
[Limits](https://userfrm.github.io/ibkr-dx/reference/limits.html) names the
case. See [Running an existing program](#running-an-existing-program).
</details>

<details>
<summary><b>Can I run two programs, or this and a gateway, at the same time?</b></summary>

On two logins, yes. On one, no: each program opens its own session, and a login
holds one session at a time, so the second takes the first one's session, and
the venue names the host that took it. The same holds for a gateway on that
login.
</details>

<details>
<summary><b>I asked for a series and got nothing back. Is it broken?</b></summary>

Probably not. An empty result means one of two things: the venue holds nothing
for that contract, or the account is not entitled to that series. Such a
subscription is acknowledged and then nothing is stated, with no error to read,
not even `tick_req_params`: it fires once per request, including a follower
sharing another request's subscription, with the first record the request is
served, as a gateway sends it. It reads the contract's latest stored parameters
when delivered: the price increment, the exchange the best bid and offer come from,
and the permission number the venue gives the request (0
nothing stated, 1 no top of book, 2 snapshots, 3 real-time top of book, 4
snapshots not available through the API), which is 0 where the venue names no
such exchange, as for a currency or a crypto, on a bond, a bill, a
fixed-income contract or a combination, and while the frozen quote is served in
place of the live one, whatever the venue stated, as a gateway states it.
Whether that number differs between a series the account is not entitled to
and one with nothing to say has not been seen yet, and a
gateway may report 0 for some other contracts whatever the venue stated; until
a capture settles both, the subscription list in account management is what
separates them. See
[What is not covered](#what-is-not-covered).
</details>

<details>
<summary><b>Is paper different from live?</b></summary>

Both run on this engine, with the same calls and the same protocol; a live
login also enters the second-factor approval, which paper does not. What each
answers depends on that account's subscriptions and permissions. The
verification here is on a paper account, with one order round trip on a funded
account — see [Evidence](https://github.com/userFRM/ibkr-dx/blob/main/docs/evidence.md).
</details>

<details>
<summary><b>What happens when a connection drops?</b></summary>

It is rebuilt on its own and the subscriptions it was serving are asked for
again, under the request the caller made. Connections are independent: a quote
feed reconnecting does not disturb an order in flight.

A trading connection that drops is announced only once three attempts to
rebuild it have failed, or once it cannot be rebuilt at all: the caller then
hears 1100, and 1102 when it is back. A drop rebuilt sooner is not announced.
Working orders keep the status they were last given: as on a gateway, nothing
is said of them at the drop, and the venue restates each one after the
reconnect. An order the venue does not restate keeps its last status.
</details>

<details>
<summary><b>Why does a figure come back as 1.7976931348623157e308?</b></summary>

That is the largest number a double carries, and it is how the venue says it
holds nothing for that field. It is passed through rather than turned into a
zero, because zero is a real price and a real greek.
</details>

<details>
<summary><b>Rust or Python — is one behind the other?</b></summary>

Not on the canonical list of the TWS API's calls and callbacks. Every one is on
both, and the generated matrix counts the ones on which the two differ: zero. `scripts/conformance.py --compare` holds ten server responses to the same
answer on both. Beyond that list, each language spells some calls its own way
and a few are on one side only; the
[Beyond the canonical list](#beyond-the-canonical-list) table has a column for
each.
</details>

Attached stop-loss and profit-taking orders are constructed from the selected
account preset through the existing `Order` fields. The engine loads the
preset and holds the family until it transmits, sending parent, stop loss and
profit taker in order. Percentage-allocation sizing from group or model
holdings is not carried: use explicitly sized parent and child orders when
those holdings determine their quantities. The advertised level remains 217;
226 is the highest level a gateway announces. See
[attached orders](https://userfrm.github.io/ibkr-dx/reference/limits.html#attached-orders)
for fields, refusals and the remaining venue evidence.

Individually placed children with `parentId` / `parent_id` share the known
parent's cancellation group, including children added to a bracket helper's
family. A refused modification leaves the original working order and its last
accepted terms intact, and a refused cancellation leaves the order in the state
it held before the cancel went out. Hedge pricing instructions are retained on
replacement.
See [order behaviour](https://userfrm.github.io/ibkr-dx/reference/venue-behaviour.html#orders).

## Testing

Claims here rest on tests, and the tests are counted rather than described:

| Suite | Count | Needs a session |
| --- | ---: | :---: |
| Rust, unit and integration | 3,219 | No |
| Python | 1,191 | No |
| Rust, live | 9 | Yes |
| Python, live | 131 | Yes |
| Paper compatibility, 154 phases | 51 | Yes |

Every published count is checked against what is actually there, so a number in
this file cannot drift from the suite that produced it. What each claim rests on
is in [Evidence](https://github.com/userFRM/ibkr-dx/blob/main/docs/evidence.md).

Readers of the venue's own records are held to a harder standard than passing:
a test that would pass against a broken reader is not a test. Each is checked by
breaking the reader deliberately and confirming the test fails.

## Contributing

Issues and pull requests are welcome.

> [!TIP]
> The fastest way to be useful is a wire observation: a contract, a request, and
> what the venue answered with. A reader is only as good as the records it has
> been held against, and the ones that have met the fewest live records are
> named in [What is not covered](#what-is-not-covered).

Before opening a pull request, run the same gate the workflow runs. It builds
both surfaces, runs every suite, regenerates the documentation and checks that
what is published matches what is there:

```sh
python scripts/gate.py
```

## Security

> [!CAUTION]
> Credentials are the account. Never commit them, never paste them into an
> issue, and never put them in a file the repository tracks.

Pass them at connect time from the environment or a secret store. A resumed
session is written to disk encrypted with a password you supply and never in the
clear. Nothing in this repository ships credentials, and nothing that fabricates
a session is compiled into the published wheel.

If you find a security problem, please report it privately through the
repository's security advisories rather than in a public issue.

## License

[AGPL-3.0](https://github.com/userFRM/ibkr-dx/blob/main/LICENSE)

## Credits

ibkr-dx began as a fork of [ibx](https://github.com/deepentropy/ibx) by
DeepEntropy and Odyssée, at its v0.7.1 release
([deepentropy/ibx@9367845](https://github.com/deepentropy/ibx/commit/9367845), 25 July 2026),
under the same AGPL-3.0 licence. That history is the first commit here, and its
full log is in the original repository. Everything after it was written for
ibkr-dx, from 28 July 2026 on. Thank you to both of them for the foundation.

- ibx: Copyright (C) 2026 DeepEntropy and Odyssée
- ibkr-dx: Copyright (C) 2026 userFRM

## Disclaimer

Interactive Brokers®, IBKR®, Trader Workstation®, and IB Gateway® are
registered trademarks of Interactive Brokers Group, Inc. This project is **not
affiliated with, endorsed by, or supported by Interactive Brokers**.

ibkr-dx is an independent, open-source project provided "as is", without warranty
of any kind.

> [!CAUTION]
> This client places orders against a real account. Test against a paper login
> first, and satisfy yourself that an order reads the way you meant it before
> pointing it at money.

### Legal Considerations

- **No warranty.** ibkr-dx is provided "as is", without warranty of any kind. See [LICENSE](https://github.com/userFRM/ibkr-dx/blob/main/LICENSE) for full terms.
- **Use at your own risk.** Users are solely responsible for ensuring their use of ibkr-dx complies with Interactive Brokers' Terms of Service, Customer Agreement, and any applicable laws or regulations. Using ibkr-dx may carry risks including but not limited to account restriction or termination by IB.
- **Orders are real.** ibkr-dx places, modifies and cancels orders on the account it logs into, and is at an early version. It is not intended as a replacement for officially supported IB software in production trading environments. The authors accept no liability for financial losses, missed trades, account issues, or any other damages arising from the use of this software.
- **Protocol stability.** ibkr-dx relies on an undocumented protocol that IB may change at any time without notice. There is no guarantee of continued functionality.
