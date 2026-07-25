# Send a Stop Order

Place a BUY STOP on SPY far above market — the trigger never fires, so the order stays resting. Watch the acknowledgement, then cancel it.

## What this shows

- Allocating a fresh `order_id` from `client.next_order_id()`.
- Building an `Order` with `order_type = "STP"` and the trigger in `aux_price`.
- Reading `order_status` callbacks (`PreSubmitted` → `Submitted`).
- Cancelling with `cancel_order` and observing the `Cancelled` terminal status.

> Paper account only. The trigger is set far above market so it will not fire — a real stop converts to a market order on touch.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_stop_order
```

## Source

```rust
{{#include ../../../../../examples/hello_stop_order.rs}}
```
