# Login

The smallest possible IBX program: connect, wait for `next_valid_id`, disconnect.

Two flavors are shown below — **paper** for everyday testing and **live** for read-only
validation against your real account.

> Per the project rules, never send orders from a live account. Use live only for
> read-only checks (login, contract details).

## What this shows

- Reading credentials from environment variables.
- Calling `EClient.connect(...)` with `paper=True` (paper) or `paper=False` (live).
- Receiving `next_valid_id` — the signal that the session is fully established and ready for requests.

## Paper

### Run it

```bash
IB_USERNAME=... IB_PASSWORD=... python examples/hello_login.py
```

### Source

```python
{{#include ../../../../../examples/hello_login.py}}
```

## Live

The live login may trigger a second-factor push. Approve it on your mobile
authenticator when prompted — `connect()` blocks until the gate clears.

### Run it

```bash
IB_LIVE_USERNAME=... IB_LIVE_PASSWORD=... python examples/hello_login_live.py
```

### Source

```python
{{#include ../../../../../examples/hello_login_live.py}}
```
