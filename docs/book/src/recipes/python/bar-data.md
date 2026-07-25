# Historical Bars

Fetch one trading day of 5-minute SPY bars and print the first and last.

## What this shows

- Building a `Contract` with `con_id` so the request is unambiguous.
- Calling `req_historical_data` with duration / bar size / what-to-show.
- Driving the callback loop on a daemon thread with `EClient.run`.
- Reading OHLCV out of the bar object.

## Run it

```bash
IB_USERNAME=... IB_PASSWORD=... python examples/hello_bar_data.py
```

## Source

```python
{{#include ../../../../../examples/hello_bar_data.py}}
```
