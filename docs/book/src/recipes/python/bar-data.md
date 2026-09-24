# Historical Bars

Fetch one trading day of 5-minute SPY bars and print the first and last. Use
this whenever you need a finished series rather than a live feed.

## What this shows

- Building a `Contract` that carries `con_id`, so the venue is not asked to
  guess which listing you meant.
- `req_historical_data(req_id, contract, end_date_time, duration_str,
  bar_size_setting, what_to_show, use_rth)`, with `format_date`,
  `keep_up_to_date` and `chart_options` left at their defaults.
- Driving the callback loop on a daemon thread with `EClient.run`.
- Reading OHLCV off the bar object.

An empty `end_date_time` means now. `use_rth=1` keeps the series inside regular
hours.

## What comes back

One `historical_data` callback per bar, in time order, then one
`historical_data_end` carrying the first and last timestamps of the range that
was served.

With `keep_up_to_date=True` the bar still forming is folded from the live
five-second stream. A daily session supplied with the history keeps its
opening and end across UTC midnight. Its updates use the session end's date
on the series' timezone, as the history does, under both date-format settings.
Timed daily history is returned as `yyyyMMdd`; explicit weekly and monthly
date strings are preserved.

Continuation beyond the last supplied daily session still uses UTC calendar
boundaries because it has no later session schedule. Intraday bars open on
whole multiples of their length from the epoch; a week opens on Monday and a
month on its first day at midnight UTC. Updates to these calendar bars remain
dates under both date-format settings.

**The bar still forming starts from the five-second bars the stream sends after
the request, not from the venue's own current bar.** Until the next bar opens —
for a week up to five trading days, for a month up to a month — every update's
open, high, low and volume count only what traded since the request. A gateway
continues the venue's own bar.

## The shorter form

One blocking method does the same request and returns the list:

```python
bars = c.historical_data(spy, "", "1 D", "5 mins", "TRADES", use_rth=1)
```

It sends, waits and returns, taking its answer off the queue by request id and
releasing the interpreter lock while it waits. Do not run `run()` beside it on
the same client: `run()` drains every queue rather than only its own, so the two
compete for the answer. Pick the callbacks or pick this.

Cancelling historical data also withdraws the corporate-actions query held
by that request. A separate corporate-actions request keeps running, even
when it uses the same caller request id.

## Limits

`bar_size_setting` and `what_to_show` are checked before anything is sent, so a
misspelling is refused here rather than answered with a different series. With
`keep_up_to_date=True` the size must be one this client can form from
the five-second bars the venue keeps sending: five seconds up to a day in
whole multiples of five seconds, a week, or a month. A second is shorter than
what arrives and is refused. A request kept up to date is refused, as a gateway
refuses it, with an end date, on a combination, or on a series other than
TRADES, MIDPOINT, BID, ASK and the two option open-interest series — an empty
series name included.

How far back a series goes, and which bar sizes pair with which durations, are
the venue's rules. A request outside them comes back as a stated refusal on the
`error` callback rather than as empty bars.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... python examples/hello_bar_data.py
```

## Source

```python
{{#include ../../../../../examples/hello_bar_data.py}}
```
