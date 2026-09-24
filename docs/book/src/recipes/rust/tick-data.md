# Streaming Ticks

Subscribe to top-of-book on SPY, collect for five seconds, print the latest bid,
ask and last.

## What this shows

- `req_mkt_data(req_id, &contract, generic_tick_list, snapshot, regulatory_snapshot)`.
- Reading prices off `tick_price`, where `tick_type` names which price it is:
  1 bid, 2 ask, 4 last, 9 close.
- `cancel_mkt_data(req_id)` before disconnecting.

## What comes back

`tick_req_params` (`tickReqParams` in Python) states the contract's price
increment, exchange and snapshot permission once per request. It reads the
latest stored parameters when the callback is delivered. A follower sharing
the subscription gets its own callback; cancelling and reusing its request id
starts a new request. A request cancelled before delivery receives no callback.

`tick_price` and `tick_size` for as long as the subscription runs.

`tick_generic` fires for the halt state the venue states on tick 49: 0 while the
contract is trading, 1 once it has stopped.

With `snapshot: true` you get the first available quote, then
`tick_snapshot_end`, and this client cancels the subscription for you. That is
this client ending a subscription, not a separate request.

`regulatory_snapshot: true` is the venue's own one-shot snapshot. It is a
different request type, and an account without the
entitlement is refused by the venue. It also ends on `tick_snapshot_end`.

## Limits

Each number in `generic_tick_list` is asked for, as a subscription of its own
beside the prices. The number you state is the number the venue knows the series
by, so there is nothing to translate. `"292"` additionally subscribes to news for
that contract, and `"292:BRFG+DJNL"` asks for it from the providers it names; an
entry that is not a number is reported rather than sent.

Every number the protocol will accept in that list is read here, and each
arrives under the number the reference client publishes it under:

| you ask for | you receive |
|---|---|
| `100` | call and put option volume, on `tick_size` 29 and 30 |
| `101` | call and put open interest, on `tick_size` 27 and 28 |
| `104` `512` | historical volatility, on `tick_generic` 23 |
| `105` | average option volume, the two sides added, on `tick_size` 87 |
| `106` | option implied volatility, on `tick_generic` 24 |
| `162` | the premium of an index over the future written on it, on `tick_generic` 31 |
| `165` | the day's volume and the 13, 26 and 52-week extremes, on `tick_size` 21 and `tick_price` 15 to 20 |
| `220` | the mark the venue keeps, on `tick_price` 78 |
| `221` `232` | the mark, on `tick_price` 37 |
| `225` | auction volume and imbalance on `tick_size` 34 and 36, auction price on `tick_price` 35, the regulatory imbalance on `tick_size` 61 |
| `233` | the trade tape, on `tick_string` 48: `price;size;time;volume;vwap;single` |
| `236` | shortability on `tick_generic` 46, and the borrowable share count on `tick_size` 89 |
| `258` | company ratios, on `tick_string` 47 |
| `292` `292:BRFG+DJNL` | news, on `tick_news` |
| `293` `294` `295` | trade count, trade rate and volume rate, on `tick_generic` 54, 55 and 56 |
| `318` | last regular-session trade, on `tick_price` 57 |
| `375` | the trade-report tape, on `tick_string` 77 |
| `411` | real-time historical volatility, on `tick_generic` 58 |
| `456` | what the contract pays out, on `tick_string` 59 |
| `460` | bond factor multiplier, on `tick_generic` 60 |
| `499` | borrow fee rate, on `tick_price` 111 |
| `577` `623` | an ETF's net asset value, last and frozen, on `tick_price` 96 and 97 |
| `586` | the estimated IPO midpoint on `tick_generic` 101, and what it opened at on 102 |
| `588` | futures open interest, on `tick_size` 86 |
| `595` | the 3, 5 and 10-minute volumes, on `tick_size` 63, 64 and 65 |
| `614` | an ETF's net asset value high and low, on `tick_price` 98 and 99 |
| `619` | the slow mark, on `tick_price` 79 |
| `787` | the odd lot: both prices on `tick_price` 105 and 106, their sizes on `tick_size` 107 and 108, where each is quoted on `tick_string` 109 and 110 |

Any other number is asked for the same way, as a series of its own. What the
venue states on series the TWS API has no tick for is read through the calls
under [Beyond the documented API](../../reference/beyond-the-api.md).

One subscription per contract. To change the mode on a contract, cancel first.

Types 3 (delayed) and 4 (delayed-frozen) on `req_market_data_type` enable
a delayed feed when live data is unavailable. Both start live. A bid/ask
refusal that states delayed data is available switches to the requested
delayed feed and reports 10167 without ending the request. `market_data_type`
reports the feed delivered: 1 while live, then 3 or 4 after the switch.

`req_mkt_data_ex` selects a feed directly: `mode_9887` is 0 realtime, 1 delayed,
2 frozen, or 3 delayed-frozen. Frozen keeps thinly traded names quoting after
hours, when the realtime feed is silent.

A refusal naming several requests reaches each affected caller. The bid/ask
and last entries of one quote produce one subscription error. Cancelling a
tick-by-tick stream uses its query identifier before acknowledgement and its
assigned stream identifier afterwards; another caller sharing that stream
keeps receiving data until it cancels too.

`req_mkt_data_ex` takes `mkt_data_options: &[TagValue]` after `mode_9887`.
Pass `&[]` when there are no options. `req_mkt_data` keeps its existing
signature and supplies an empty list. With no session, both calls report
504, *Not connected*, before checking the request.

The list accepts `manual=0` or `manual=1`. An unknown key is refused under
10337, an invalid value under 10338, and a malformed list under 320.
`NOAPIMISCVLD` at logon lifts the key and value checks; the later check that
`manual` is zero or one remains.

## Reading without the callback loop

`quote(req_id)` returns the latest bid, ask, last and sizes for a running
subscription, with no wrapper and no lock. `quote_of(&contract)` does the same
by contract. Both return `None` until the first tick has landed.

Prices and sizes on `Quote` are integers scaled by `PRICE_SCALE` and `QTY_SCALE`
(both 10<sup>8</sup>). Divide when you display; the `tick_price` callback hands
you `f64` already.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_tick_data
```

## Source

```rust
{{#include ../../../../../examples/hello_tick_data.rs}}
```
