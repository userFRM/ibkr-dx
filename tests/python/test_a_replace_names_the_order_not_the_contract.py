"""A replace names the order rather than the contract.

A placement under a working order's number that names another contract is
refused under the number the venue gives that mismatch, so a caller branching
on it withdraws and places anew rather than re-sending.
"""
import ibkr_dx


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.seen.append((req_id, code))

    def orderStatus(self, *a):
        pass


def stock(con_id, symbol):
    c = ibkr_dx.Contract()
    c.conId = con_id
    c.symbol = symbol
    c.secType = "STK"
    c.exchange = "SMART"
    c.currency = "USD"
    return c


def limit_order():
    o = ibkr_dx.Order()
    o.action = "BUY"
    o.totalQuantity = 1
    o.orderType = "LMT"
    o.lmtPrice = 100.0
    o.tif = "DAY"
    return o


def test_a_replace_naming_another_contract_is_refused_as_a_mismatch():
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("T")
    c._test_map_con_id(756733, 0)
    c._test_map_con_id(265598, 1)
    c.placeOrder(85, stock(756733, "SPY"), limit_order())
    assert c._test_take_commands(), "the order went out"

    c.placeOrder(85, stock(265598, "AAPL"), limit_order())
    c.poll()
    assert w.seen == [(85, 105)], f"the replace names another contract: {w.seen}"
    assert not c._test_take_commands(), "and nothing was sent under it"
