# Getting started

## What you need

* An Interactive Brokers account, paper or live. No IB software, and not the
  `ibapi` package either.
* Rust 1.89 or newer. Both install routes compile the engine.
* Python 3.11 or newer, for the Python surface.

## Install

Both routes build from the repository.

### Rust

```toml
[dependencies]
ibkr-dx = { git = "https://github.com/userFRM/ibkr-dx" }
```

### Python

```bash
pip install "git+https://github.com/userFRM/ibkr-dx"
```

The package imports as `ibkr_dx`. The build backend is
[maturin](https://www.maturin.rs/), which compiles the Rust core into the
extension module, and `pyproject.toml` names the features it needs, so there is
nothing to pass. It builds for CPython 3.11 to 3.14 and the free-threaded
3.14t; the free-threaded 3.13t is not supported.

Either route compiles the engine, so it needs Rust 1.89 or newer and, on Linux,
the OpenSSL headers and `pkg-config` (`libssl-dev` on Debian and Ubuntu,
`openssl-devel` on Fedora and RHEL).

### From the source

Working on the client itself, build it in place:

```bash
git clone https://github.com/userFRM/ibkr-dx
cd ibkr-dx
uv venv .venv
source .venv/bin/activate          # .venv\Scripts\activate on Windows
uv pip install maturin
maturin develop
```

`maturin develop` replaces the installed module. A local `pytest` run tests
whatever was built last, so rebuild before running one.

## Feature flags

`default = []`. The Rust client needs nothing turned on.

| Feature | What it adds |
| --- | --- |
| `python` | The PyO3 bindings |
| `extension-module` | Tells PyO3 not to link libpython. Right for the wheel, wrong for `cargo test`. maturin sets it; you do not |
| `dev-tools` | The binaries under `src/bin` — the benchmarks, and the capture tools this repository is developed with. Most of them read credentials and open a session |

The features not in that table exist for this repository's own test suites.
They are off by default and should stay off in anything you install.

## Credentials

The credentials go in the connect call. There is no configuration file and no
process holding a login on your behalf.

```python
client.connect(username="your_user", password="your_pass", paper=True)
```

```rust
let client = EClient::connect(&Config {
    username: "your_user".into(),
    password: "your_pass".into(),
    paper: true,
    ..Default::default()
})?;
```

**Do not name a host.** Left empty, the client knocks on one of the venue's
regional doors and the venue answers by naming the server this account actually
lives on; the session moves there. A host is worth stating only to knock at a
particular region.

**`paper`.** `true` skips the live second-factor approval gate — a paper
session presents no second factor. `false` enters that gate on connect, and
that call blocks until it is answered.

Which factor an account presents is stated by the venue when the gate opens,
and this client handles two: an authenticator code, which has nothing to fall
back on and needs a `code_provider`, and an IBKey push, which does not. Nothing
below has been exercised against a live account from here, so treat the timing
and the shape of the prompt as the venue's to state rather than as described.

Use a paper account while you are writing something. A live account is a live
account.

**One program per login.** Each program opens its own session, and a login
holds one session at a time: a second program on the same login takes the
first one's session, and the venue names the host that took it. Give each
program that runs at the same time a login of its own.

One place reads the environment instead of the call: the programs under
`examples/` read `IB_USERNAME` and `IB_PASSWORD`:

```bash
export IB_USERNAME="your_username"
export IB_PASSWORD="your_password"
```

## Pick a surface

Every surface below drives the same engine. Pick by the program you have, not
by capability.

| Language | Surface | Pick it when |
| --- | --- | --- |
| Python | `ibkr_dx.EClient` / `EWrapper` | Your program is written against `ibapi`. Change `ibapi` to `ibkr_dx` in its imports, and its connect call to the one above. |
| Rust | `EClient` / `Wrapper` | You are porting a TWS API program and want its callbacks. |

> For ib_async, use [ib_async-dx](https://github.com/userFRM/ib_async-dx), which runs it on this engine.

## Hello, world

Both files below are the ones in the repository, included rather than copied,
so this page cannot drift from what actually runs. The Rust one is compiled by
CI along with the rest of `examples/`. Each subscribes to SPY for five seconds
and prints the last quote it saw.

### Rust

```rust
{{#include ../../../examples/hello_tick_data.rs}}
```

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_tick_data
```

### Python

```python
{{#include ../../../examples/hello_tick_data.py}}
```

```bash
IB_USERNAME=... IB_PASSWORD=... python examples/hello_tick_data.py
```

## Next steps

* [Login](./recipes/python/login.md) — connect, take the first order id, disconnect
* [Streaming ticks](./recipes/python/tick-data.md) · [L2 depth](./recipes/rust/streaming-l2.md)
* [Order lifecycle](./recipes/python/order-lifecycle.md) — place, modify, cancel, fill
* [Limits](./reference/limits.md) — read this before you depend on a call

For an existing installation, [Moving to 0.2](./reference/migration-0.2.md)
describes the request signatures, ordered callbacks, shutdown result and
removed registration timeout.
