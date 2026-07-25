# Login

The smallest possible IBX program: connect, wait for `next_valid_id`, disconnect.

Two flavors are shown below — **paper** for everyday testing and **live** for read-only
validation against your real account.

> Per the project rules, never send orders from a live account. Use live only for
> read-only checks (login, contract details).

## What this shows

- Reading credentials from environment variables.
- Building an `EClientConfig` with `paper: true` (paper) or `paper: false` (live).
- Receiving `next_valid_id` — the signal that the session is fully established and ready for requests.

## Paper

### Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_login
```

### Source

```rust
{{#include ../../../../../examples/hello_login.rs}}
```

## Live

The live login may trigger a second-factor push. Approve it on your mobile
authenticator when prompted — the `connect` call blocks until the gate clears.

### Run it

```bash
IB_LIVE_USERNAME=... IB_LIVE_PASSWORD=... cargo run --example hello_login_live
```

### Source

```rust
{{#include ../../../../../examples/hello_login_live.rs}}
```
