"""A spread scan is asked for from Python as it is from Rust.

The request and the class it takes existed only on the Rust client, so a
Python program could read the strategies a scan found and had no way to ask for
one.
"""

import ibkr_dx


class Heard(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.errors.append((req_id, code))


def aapl(con_id=265598):
    con = ibkr_dx.Contract()
    con.conId = con_id
    con.symbol = "AAPL"
    con.secType = "STK"
    con.exchange = "SMART"
    con.currency = "USD"
    return con


def test_a_scan_takes_its_fields_by_name_and_refuses_one_it_does_not_have():
    scan = ibkr_dx.SpreadScan(version=6, account="DU1234567", min_delta=0.25)
    assert (scan.version, scan.request, scan.min_delta, scan.max_delta) == (6, 0, 0.25, None)
    scan.allowed_strategies = "VS"
    assert scan.allowed_strategies == "VS"
    try:
        ibkr_dx.SpreadScan(minDelta=0.25)
    except RuntimeError as refused:
        assert "minDelta" in str(refused)
    else:
        raise AssertionError("a field the scan does not carry was accepted")


def test_a_scan_is_asked_for_beside_the_series_it_is_answered_on():
    heard = Heard()
    c = ibkr_dx.EClient(heard)
    c._test_connect("DU0000000")
    # Already watched under another request, so this one joins the quotes that
    # are up and asks only for the series the scan is answered on.
    c._test_map_con_id(265598, 3)
    c._test_map_instrument(1, 3)

    c.req_spread_scan(2, aapl(), ibkr_dx.SpreadScan(version=6, account="DU0000000"))

    sent = c._test_take_commands()
    assert any(
        cmd.startswith("Subscribe") and "[481]" in cmd and "spread_scan: Some" in cmd
        for cmd in sent
    ), sent
    c.poll()
    assert heard.errors == [], heard.errors


def test_a_scan_naming_no_contract_is_refused_on_error():
    heard = Heard()
    c = ibkr_dx.EClient(heard)
    c._test_connect("DU0000000")

    c.req_spread_scan(3, aapl(con_id=0), ibkr_dx.SpreadScan(version=6))

    c.poll()
    assert heard.errors == [(3, 321)]
    assert c._test_take_commands() == [], "nothing is asked for a scan of nothing"
