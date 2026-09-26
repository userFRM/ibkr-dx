# Historical Bars

Fetch one trading day of 5-minute SPY bars and print the first and last. Use
this whenever you need a finished series rather than a live feed.

## What this shows

- Building a `Contract` that carries `con_id`, so the venue is not asked to
  guess which listing you meant.
- `req_historical_data`, which takes every argument the request carries:
  `req_id`, contract, `end_date_time`, `duration`, `bar_size`, `what_to_show`,
  `use_rth`, `format_date`, `keep_up_to_date`.
- Pumping the callbacks with `process_msgs` until `historical_data_end` lands,
  or the request's refusal does.
- Reading OHLCV out of `BarData`.

An empty `end_date_time` means now. `use_rth: true` keeps the series inside
regular hours. `format_date: 1` asks for the date written out rather than as
seconds. `keep_up_to_date: false` asks for closed bars only.

## What comes back

One `historical_data` callback per bar, in time order, then one
`historical_data_end` carrying the first and last timestamps of the range that
was served. A request whose bars are refused once it has been handed over is
told why on `error`, and nothing follows it: a gateway states no end after a
refusal, so a program waiting for the end stops on the error too. A request
kept up to date whose five-second stream the venue refuses is told nothing, as
a gateway tells it nothing: a history still to come arrives with its end, and
no update follows. When the historical connection drops, a request kept up to
date ends on 10182, and one still arriving is told 165 and asked again once the
connection is back, as a gateway does. A 165 is a notice and ends nothing: the
call that waits for the bars waits through it.

With `keep_up_to_date: true` the bar still forming goes on from the history's
last bar, as a gateway's does: the live five-second bars inside it are folded
into the open, high, low and volume the venue stated for it. Five-second bars
are sent as the stream sends them. One that arrives before the history is in
is held, as a gateway holds it, and folded into the history's last bar with the
next one after it; one from before the bar in hand opened is passed over. So
nothing of the bar still forming is heard before `historical_data_end`, and
every update is heard on `historical_data_update`, whatever else the request's
number has done: a live bar stream under the same number is heard on
`real_time_bar`, and a second request under the number, refused while this
one answers, leaves this one's dates as it asked for them. A daily session supplied with the history
keeps its opening and end across UTC midnight. Its updates use the session
end's date on the series' timezone, as the history does, under both
date-format settings. Timed daily history is returned as `yyyyMMdd`; explicit
weekly and monthly date strings are preserved.

When that session ends, the next five-second bar opens the next bar at
midnight UTC of its own day, as a gateway opens it. That bar ends at the next
midnight UTC, or at the close the history stated where that falls between the
two, and is dated by where it ends on the series' timezone; a bar that has
ended is opened again by the next five-second bar. So a session that opens
before midnight UTC, as a future's evening session does, is dated by the day
the history closed until midnight UTC, and by the next one after it. On a
timezone east of UTC the next midnight UTC falls on the following day there,
so the bar a session there rolls over to is dated by the day after that
session, as a gateway dates it. Intraday bars open on whole multiples of their
length from the epoch; a week opens on Monday and a month on its first day at
midnight UTC. Updates to these calendar bars remain dates under both
date-format settings.

## The shorter form

Two blocking calls do the same request without a wrapper:

```rust
let bars = client.historical_data(&spy, "", "1 D", "5 mins", "TRADES", true)?;
let same = client.bars(&spy, "1 D", "5 mins")?;
```

`bars` fills in the arguments most callers state the same way: trades, regular
hours, ending now. It asks for MIDPOINT instead of TRADES on instruments that
are quoted rather than traded (`CASH`, `CFD`, `CMDTY`), which answer a TRADES
request with no history at all.

Cancelling historical data also withdraws the corporate-actions query held
by that request. A separate corporate-actions request keeps running, even
when it uses the same caller request id.

## Limits

`bar_size` and `what_to_show` are checked before anything is sent, so a
misspelling is refused here rather than answered with a different series. With
`keep_up_to_date: true` the size must be one this client can form from
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
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_bar_data
```

## Source

```rust
{{#include ../../../../../examples/hello_bar_data.rs}}
```
