"""A read-only session refuses to change a position, and a session that is not read-only does not."""

import ibkr_dx


def spy():
    c = ibkr_dx.Contract()
    c.symbol = "SPY"
    c.secType = "STK"
    c.exchange = "SMART"
    c.currency = "USD"
    return c


def test_a_read_only_session_refuses_to_change_a_position():
    """The reference client carries the same control. A research program wants
    the guarantee at the client rather than in its own discipline."""
    class Errors(ibkr_dx.EWrapper):
        def __init__(self):
            super().__init__()
            self.seen = []

        def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
            self.seen.append((req_id, code, msg))

    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU0000000", readonly=True)

    order = ibkr_dx.Order()
    order.action = "BUY"
    order.orderType = "MKT"
    order.totalQuantity = 1

    # Answered on the error callback, as the reference client answers a
    # request it refuses, and under the same number the other surface gives.
    for under, call in (
        (1, lambda: c.placeOrder(1, spy(), order)),
        (1, lambda: c.cancelOrder(1, "")),
        (7, lambda: c.exerciseOptions(7, spy(), 1, 1, "", 1, "", "", False)),
        (-1, lambda: c.reqGlobalCancel()),
    ):
        w.seen.clear()
        call()
        c.poll()
        assert [(r, code) for r, code, _ in w.seen] == [(under, 321)], w.seen
        assert "read-only" in w.seen[0][2]
        assert not c._test_take_commands(), "and nothing was sent"


def test_a_session_that_is_not_read_only_does_not_refuse():
    """The guard must fire on the flag, not on every order.

    A test-connected client has no venue behind it, so the order fails further
    down. What matters here is that it fails somewhere other than the guard.
    """
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU0000000")
    order = ibkr_dx.Order()
    order.action = "BUY"
    order.orderType = "MKT"
    order.totalQuantity = 1
    try:
        c.placeOrder(1, spy(), order)
    except RuntimeError as e:
        assert "read-only" not in str(e), "the guard fired on a session that is not read-only"
