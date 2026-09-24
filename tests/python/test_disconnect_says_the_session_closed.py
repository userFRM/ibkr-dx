"""`disconnect()` says the session closed before it returns, and once.

The reference client calls `connectionClosed` inside its own `disconnect()`,
and nothing the session had queued is delivered after it. A program waiting on
`connectionClosed` to know it may exit hears it without another pass, and a
pass after says nothing more. A client that never connected is told nothing.
"""

import ibkr_dx


class Heard(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.said = []

    def connectionClosed(self):
        self.said.append("closed")

    def orderStatus(self, orderId, *rest):
        self.said.append(f"status {orderId}")


def test_disconnect_says_the_session_closed_before_it_returns():
    w = Heard()
    c = ibkr_dx.EClient(w)
    c._test_connect("T")
    c._test_push_order_update(88, 0, "Submitted", 0.0, 1.0)
    c.disconnect()
    assert w.said == ["closed"], w.said
    c.poll()
    c.poll()
    assert w.said == ["closed"], f"said once, and nothing queued after it: {w.said}"


def test_a_client_that_never_connected_is_not_told_a_session_closed():
    w = Heard()
    c = ibkr_dx.EClient(w)
    c.disconnect()
    c.poll()
    assert w.said == []
