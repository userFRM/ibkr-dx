"""`reqHistoricalNews` takes `totalResults` as the TWS API's `int`, as a gateway does.

A gateway asks the venue for no more than three hundred headlines, and passes a
smaller number on as stated, below nought included. Refused below nought, a
request a gateway would have sent was answered with an error instead.
"""

import ibkr_dx


def _asked(total_results):
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU1")
    c.reqHistoricalNews(1, 265598, "BRFG", "", "", total_results, [])
    sent = [cmd for cmd in c._test_take_commands() if "FetchHistoricalNews" in cmd]
    assert len(sent) == 1, sent
    return sent[0]


def test_more_than_three_hundred_asks_for_three_hundred():
    assert "max_results: 300" in _asked(500)


def test_a_smaller_number_is_passed_on():
    assert "max_results: 7" in _asked(7)


def test_a_number_below_nought_is_passed_on():
    assert "max_results: -5" in _asked(-5)
