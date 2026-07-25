# Request Account PnL

Subscribe to the account-level live PnL stream, take the first update, then cancel and disconnect.

## What this shows

- Reading the managed account name from the client (`get_account_id`).
- Subscribing with `req_pnl(req_id, account)`.
- Reading `daily_pnl`, `unrealized_pnl`, `realized_pnl` from the `pnl` callback.
- Cancelling cleanly with `cancel_pnl` before disconnecting.

> Some fields stay zero until you have a position. Pair with the limit-order recipe if you want to see non-zero values.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... python examples/hello_pnl.py
```

## Source

```python
{{#include ../../../../../examples/hello_pnl.py}}
```
