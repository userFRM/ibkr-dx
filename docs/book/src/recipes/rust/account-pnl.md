# Request Account PnL

Subscribe to the account-level PnL stream, take the first update, cancel and
disconnect.

## What this shows

- Reading the account name off `client.account_id`, which `connect` populates.
- `req_pnl(req_id, account, model_code)`.
- Reading `daily_pnl`, `unrealized_pnl` and `realized_pnl` off the `pnl`
  callback.
- `cancel_pnl(req_id)` before disconnecting.

`account` empty means the account this session opened under.

## What comes back

The venue states what each holding was worth at midnight and what it has
realised since. The three figures on the `pnl` callback are worked out from
those against the prices this session is being told, so they move as the
session's quotes move.

## Limits

`account` is checked as a gateway checks it, in its order and words: a blank
one is refused (*Account must not be empty*, 321); one the login does not hold
is refused (*Invalid account code*, 321), and so is `All` on a login holding one
account or where the logon says the login may not ask for every account; `All`
on a login the venue adds accounts to is refused (*This API request for All is
not supported for Dynamic Account Addition*, 321). Another account the login
holds, and `All` where a gateway would take it, are refused too, on both
clients: the figures are worked out from one account's midnight seeds and
holdings, the one this session opened under. Nothing is subscribed for a
refused request.

`model_code` is taken and not applied. There is no model portfolio to name here,
so pass `""`.

`cancel_pnl` stops the updates. The venue has no message withdrawing the
subscription itself, on a gateway as here, so the updates stopping is what the
call does.

An account with no position reports zeros. Pair this with the limit-order recipe
if you want to watch the numbers move.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_pnl
```

## Source

```rust
{{#include ../../../../../examples/hello_pnl.rs}}
```
