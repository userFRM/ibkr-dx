# Streaming L2 Market Depth

End-to-end recipe: subscribe to Level-2 market depth for two tickers (AAPL + TSLA), maintain per-ticker bid/ask books from `update_mkt_depth_l2` callbacks, and print top-of-book on shutdown.

## What this shows

- Subscribing to L2 with `req_mkt_depth` for multiple contracts.
- Building an in-memory book from `update_mkt_depth_l2` events (`insert` / `update` / `delete` ops).
- Cleanly cancelling subscriptions and disconnecting.

> Paper accounts typically grant only one concurrent depth subscription. Expect one of the two tickers to receive zero updates.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... python examples/hello_l2.py
```

Optional:

```bash
DURATION_SECS=30 python examples/hello_l2.py
```

## Source

```python
{{#include ../../../../../examples/hello_l2.py}}
```
