# Send a Limit Order

Place a non-marketable BUY LMT on SPY far below market, watch it acknowledge, then cancel it. End-to-end: connect → next_order_id → place → status → cancel → disconnect.

## What this shows

- Allocating a fresh `order_id` from `client.next_order_id()`.
- Building an `Order` with `order_type = "LMT"` and `lmt_price` set.
- Reading `order_status` callbacks (`PreSubmitted` → `Submitted`).
- Cancelling with `cancel_order` and observing the `Cancelled` terminal status.

> Paper account only. The price is set far below market so it will not fill.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_limit_order
```

## Source

```rust
{{#include ../../../../../examples/hello_limit_order.rs}}
```
