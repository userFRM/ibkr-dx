"""A scan that states `scannerSettingPairs` is taken, not refused.

A gateway reads the pairs and keeps them as the scan's settings. Refused here, a
scan a gateway runs was answered with an error instead. They are not carried to
the venue, which is said once in the log, and the scan goes as its filters
state it.
"""

import ibkr_dx


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []

    def error(self, reqId, errorTime, errorCode, errorString, advancedOrderRejectJson=""):
        self.errors.append((errorCode, errorString))


def test_a_scan_stating_settings_pairs_goes():
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("T")
    sub = ibkr_dx.ScannerSubscription()
    sub.instrument = "STK"
    sub.locationCode = "STK.US.MAJOR"
    sub.scanCode = "TOP_PERC_GAIN"
    sub.scannerSettingPairs = "Annual,true"
    c.reqScannerSubscription(7, sub, [], [])
    c.poll()
    assert w.errors == [], w.errors
    sent = [cmd for cmd in c._test_take_commands() if "SubscribeScanner" in cmd]
    assert len(sent) == 1, sent
    assert "Annual" not in sent[0], "the pairs are not carried"
