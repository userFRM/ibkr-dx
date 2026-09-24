"""A request for historical ticks that asks to leave out size-only changes says so.

`reqHistoricalTicks` takes `ignoreSize`, and a gateway turns it into the query's
size filter on bid/ask ticks. Taken and dropped here, a caller who asked for
price changes only was handed every size change as well, with nothing to say
the flag had not been applied.
"""

import ibkr_dx


def _asked(what_to_show, ignore_size):
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU1")
    contract = ibkr_dx.Contract(con_id=756733, symbol="SPY", sec_type="STK", exchange="SMART")
    c.reqHistoricalTicks(1, contract, "", "20260101 16:00:00", 100, what_to_show, 1, ignore_size, [])
    sent = [cmd for cmd in c._test_take_commands() if "FetchHistoricalTicks" in cmd]
    assert len(sent) == 1, sent
    return sent[0]


def test_the_flag_reaches_the_request():
    assert "ignore_size: true" in _asked("BID_ASK", True)


def test_a_request_that_did_not_ask_says_so():
    assert "ignore_size: false" in _asked("BID_ASK", False)
