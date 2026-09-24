# Request Contract Details

Resolve a symbol: send a partial description, collect every contract the venue
lists under it, stop when the end marker lands. This is how you turn `"AAPL"`
into a `con_id` you can quote and trade on.

## What this shows

- Building a partial `Contract` from symbol, sec_type, exchange and currency.
- `req_contract_details(req_id, &contract)`, and reading answers off the
  `contract_details` callback.
- Pumping the callbacks with `process_msgs` until `contract_details_end` fires.
- Reading `con_id`, `primary_exchange` and `trading_class` out of
  `ContractDetails`.

## What comes back

One `contract_details` callback per matching contract, then one
`contract_details_end` for the request id. A description that matches several
listings answers with all of them, so `rows.len()` above one is an ambiguous
description rather than an error.

## The shorter form

`req_contract_details` is the request-and-callback shape. Two blocking calls do
the same work without the wrapper:

```rust
let rows = client.contract_details(&aapl)?;   // every match
let one  = client.qualify_contract(&aapl)?;   // the single match
```

`qualify_contract` refuses an ambiguous description rather than handing back
whichever the venue listed first, which would be a different contract from the
one you asked about.

## Lookups by contract id

A lookup by `con_id` includes the requested exchange. Leaving the exchange
empty asks for the preferred definition. The same rule applies when a market
data request needs to name a contract given only by id.

Contract details reuse exchange rule scopes already returned during the
session. A first lookup can still need a separate request for each missing
exchange; later lookups reuse those answers. CORPACT is excluded because it
does not name a trading venue.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... cargo run --example hello_contract_details
```

## Source

```rust
{{#include ../../../../../examples/hello_contract_details.rs}}
```
