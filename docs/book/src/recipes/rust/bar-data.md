# Historical Bars

Fetch one trading day of 5-minute SPY bars and print the first and last.

## What this shows

- Building a `Contract` with `con_id` so the request is unambiguous.
- Calling `req_historical_data` with duration / bar size / what-to-show.
- Pumping callbacks with `process_msgs` until `historical_data_end` lands.
- Reading OHLCV out of `BarData`.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_bar_data
```

## Source

```rust
{{#include ../../../../../examples/hello_bar_data.rs}}
```
