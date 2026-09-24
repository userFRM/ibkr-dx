"""A profit request names the account whose profit it reports."""
import ibkr_dx


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.seen.append((req_id, code, msg))


def test_naming_another_held_account_carries_that_name():
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU123", accounts=["DU123", "DU999"])
    c.reqPnL(9, "DU999", "")
    c.poll()
    assert not w.seen
    sent = c._test_take_commands()
    assert any('account: "DU999"' in cmd for cmd in sent), sent
    c.disconnect()
