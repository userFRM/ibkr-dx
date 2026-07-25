# Request Contract Details

Resolve a symbol on a paper session: connect, call `req_contract_details` for AAPL, collect every matching contract, then disconnect when `contract_details_end` lands.

## What this shows

- Building a partial `Contract` (symbol + sec_type + exchange + currency).
- Driving the callback loop on a daemon thread with `EClient.run`.
- Reading `con_id`, primary exchange, and trading class out of the returned `ContractDetails`.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... python examples/hello_contract_details.py
```

## Source

```python
{{#include ../../../../../examples/hello_contract_details.py}}
```
