"""What the venue said about the account is what the account reports.

A figure stated in one currency and reported in another is a figure and a
currency that never went together, and a kind of account nobody stated is a
claim about the account rather than a fact about it. Both decide something: an
advisor account is not an individual's, and a euro balance is not a dollar one.
"""

import ibkr_dx


class Recording(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.multi = []
        self.summary = []
        self.reports = []
        self.values = []
        self.multi_by_req = []

    def accountUpdateMulti(self, reqId, account, modelCode, key, value, currency):
        self.multi.append((key, value, currency))
        self.multi_by_req.append((reqId, key, value, currency))

    def updateAccountValue(self, key, value, currency, account):
        self.values.append((key, value, currency))

    def accountSummary(self, reqId, account, tag, value, currency):
        self.summary.append((tag, value, currency))

    def commissionReport(self, report):
        self.reports.append(report)

    def commissionAndFeesReport(self, report):
        self.reports.append(report)

    def error(self, *a):
        pass


def _connected():
    w = Recording()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU1")
    return w, c


def test_a_figure_keeps_the_currency_the_venue_stated_it_in():
    w, c = _connected()
    c._test_note_account_value("NetLiquidation", "12345.67", "EUR")
    c.req_account_updates_multi(3, "DU1", "", False)
    c._test_dispatch_once()
    assert ("NetLiquidation", "12345.67", "EUR") in w.multi, w.multi


def test_a_figure_the_venue_never_stated_is_not_reported():
    w, c = _connected()
    c._test_note_account_value("NetLiquidation", "1.00", "USD")
    c.req_account_updates_multi(3, "DU1", "", False)
    c._test_dispatch_once()
    keys = [key for key, _, _ in w.multi]
    assert keys == ["NetLiquidation"], f"figures nobody stated were reported: {w.multi}"


def test_a_ledger_request_is_given_the_ledger_alone():
    """With ledger_and_nlv the answer is the per-currency ledger, whose
    NetLiquidationByCurrency is the net liquidation meant; the account's other
    figures are not delivered, as a gateway does not deliver them."""
    w, c = _connected()
    c._test_note_account_value("NetLiquidation", "100", "USD")
    c._test_note_account_value("TotalCashBalance", "50.00", "USD", ledger=True)
    c._test_note_account_value("NetLiquidationByCurrency", "100.00", "USD", ledger=True)
    c.req_account_updates_multi(3, "DU1", "", True)
    c._test_dispatch_once()
    assert sorted(key for key, _, _ in w.multi) == [
        "NetLiquidationByCurrency", "TotalCashBalance"], w.multi


def test_the_kind_of_account_is_the_one_the_venue_stated():
    w, c = _connected()
    c._test_note_account_value("AccountType", "ADVISOR", "")
    c._test_note_account_value("NetLiquidation", "1.00", "USD")
    # A summary waits for the download to finish rather than answering on the
    # first figure, which used to hand back the tags parsed so far.
    c._test_finish_account_download()
    c.req_account_summary(5, "All", "All")
    c._test_dispatch_once()
    kinds = [(tag, value) for tag, value, _ in w.summary if tag == "AccountType"]
    assert kinds == [("AccountType", "ADVISOR")], f"got {w.summary}"


def test_no_kind_is_claimed_when_the_venue_stated_none():
    w, c = _connected()
    c._test_note_account_value("NetLiquidation", "1.00", "USD")
    c.req_account_summary(5, "All", "All")
    c._test_dispatch_once()
    assert not [t for t, _, _ in w.summary if t == "AccountType"], w.summary


def test_a_fill_is_charged_in_the_contracts_own_currency():
    w, c = _connected()
    c._test_track_order(7, 1, "SPY", "BUY", 5.0, 10.0, 0, "EUR")
    c._test_push_fill(1, 7, "BUY", 10.0, 5, 0, 1.25)
    c._test_dispatch_once()
    assert w.reports, "no cost reached the caller"
    assert w.reports[-1].currency == "EUR", w.reports[-1].currency


def test_account_and_ledger_accrued_cash_are_separate_values():
    w, c = _connected()
    c._test_note_account_value("AccruedCash", "-2580.38", "CHF")
    c._test_note_account_value("Currency", "CHF", "CHF", ledger=True)
    c._test_note_account_value("AccruedCash", "-238.28", "CHF", ledger=True)
    c._test_finish_account_download()
    c.reqAccountUpdates(True, "DU1")
    c.reqAccountUpdatesMulti(1, "DU1", "", True)
    c.reqAccountUpdatesMulti(2, "DU1", "", False)
    c._test_dispatch_once()
    both = [("AccruedCash", "-2580.38", "CHF"), ("AccruedCash", "-238.28", "CHF")]
    assert [row for row in w.values if row[0] == "AccruedCash"] == both
    assert [row[1:] for row in w.multi_by_req if row[0] == 2 and row[1] == "AccruedCash"] == both
    assert [row[1:] for row in w.multi_by_req if row[0] == 1] == [
        ("Currency", "CHF", "CHF"), ("AccruedCash", "-238.28", "CHF")]
    w.values.clear()
    w.multi_by_req.clear()
    c._test_dispatch_once()
    assert not w.values and not w.multi_by_req

    c._test_note_account_value("AccruedCash", "-2579.70", "CHF")
    c._test_dispatch_once()
    assert w.values == [("AccruedCash", "-2579.70", "CHF")]
    assert w.multi_by_req == [(2, "AccruedCash", "-2579.70", "CHF")]
    w.values.clear()
    w.multi_by_req.clear()
    c._test_note_account_value("AccruedCash", "-238.28", "CHF", ledger=True)
    c._test_dispatch_once()
    assert not w.values and not w.multi_by_req

    c._test_note_account_value("AccruedCash", "-237.00", "CHF", ledger=True)
    c._test_dispatch_once()
    assert w.values == [("AccruedCash", "-237.00", "CHF")]
    assert sorted(w.multi_by_req) == [
        (1, "AccruedCash", "-237.00", "CHF"), (2, "AccruedCash", "-237.00", "CHF")]
