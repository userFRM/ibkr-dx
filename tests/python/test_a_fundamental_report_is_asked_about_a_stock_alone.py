"""A fundamental report is asked about a stock alone. A gateway refuses one on
a contract stated as any other type, or stating none, before looking it up,
in its words under 321, and so does this surface. A stock stated as CS, or in
lower case, is a stock."""
import ibkr_dx


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.seen.append((req_id, code, msg))


def test_a_fundamental_report_is_asked_about_a_stock_alone():
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU1")
    c._test_map_con_id(756733, 0)
    for req_id, sec_type in enumerate(["FUT", "", "cs", "stk"], start=1):
        contract = ibkr_dx.Contract()
        contract.conId, contract.secType = 756733, sec_type
        c.reqFundamentalData(req_id, contract, "ReportSnapshot", [])
    c.poll()
    assert w.seen == [
        (1, 321, "Please enter a valid security type"),
        (2, 321, "Please enter a valid security type"),
    ]
    sent = [cmd for cmd in c._test_take_commands() if cmd.startswith("FetchFundamentalData")]
    assert len(sent) == 2, sent
