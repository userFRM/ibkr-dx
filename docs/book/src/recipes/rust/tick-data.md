# Streaming Ticks

Subscribe to live SPY market data, collect ticks for 5 seconds, then print the latest bid / ask / last.

## What this shows

- Calling `req_mkt_data` for top-of-book streaming.
- Routing tick types through the `tick_price` callback (1=bid, 2=ask, 4=last).
- Cancelling cleanly with `cancel_mkt_data` before disconnecting.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_tick_data
```

## Source

```rust
{{#include ../../../../../examples/hello_tick_data.rs}}
```
