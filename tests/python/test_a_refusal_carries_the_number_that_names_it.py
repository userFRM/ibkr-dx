"""Each refusal the catalogue names reaches the compat caller under its own number.

Every shared validator answered in prose, and prose is stamped with the general
validation number on the way out -- so a caller branching on the number for an
unpermitted security type or a combination with no legs took the same branch it
takes for a typo in a field name.
"""

import ibkr_dx


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.seen.append((code, msg))


def spy():
    c = ibkr_dx.Contract()
    c.conId = 756733
    c.symbol = "SPY"
    c.secType = "STK"
    c.exchange = "SMART"
    c.currency = "USD"
    return c


def order(order_type):
    o = ibkr_dx.Order()
    o.action = "BUY"
    o.totalQuantity = 1
    o.orderType = order_type
    o.lmtPrice = 100.0
    o.tif = "DAY"
    return o


def test_a_combination_with_no_legs_is_refused_under_314():
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("T")
    bag = spy()
    bag.secType = "BAG"

    c.placeOrder(1, bag, order("LMT"))

    c.poll()
    assert [code for code, _ in w.seen] == [314], (
        f"a combination that names no legs: {w.seen}"
    )


def test_a_log_level_that_is_not_one_is_refused_under_319():
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("T")

    c.setServerLogLevel(9)

    c.poll()
    assert [code for code, _ in w.seen] == [319], (
        f"a level outside 1 to 5 is refused, not substituted: {w.seen}"
    )
