"""A single-position profit request carries the account named."""
import ibkr_dx


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.seen.append((req_id, code, msg))


def test_req_pnl_single_carries_the_named_account():
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("T", accounts=["T", "DU999"])
    c.reqPnLSingle(7, "DU999", "", 265598)
    c.poll()
    assert not w.seen
    assert any('account: "DU999"' in cmd for cmd in c._test_take_commands())
    c.disconnect()
