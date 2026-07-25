# Request Contract Details

Resolve a symbol on a paper session: connect, fire `req_contract_details` for AAPL, collect every matching contract, then disconnect when `contract_details_end` lands.

## What this shows

- Building a partial `Contract` (symbol + sec_type + exchange + currency).
- Pumping callbacks with `process_msgs` until the end-of-stream marker arrives.
- Reading `con_id`, primary exchange, and trading class out of `ContractDetails`.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_contract_details
```

## Source

```rust
{{#include ../../../../../examples/hello_contract_details.rs}}
```
