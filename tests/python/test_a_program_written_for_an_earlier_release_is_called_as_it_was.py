"""A wrapper is called with the shape the release it was written for calls.

The reference client's `error` has taken three shapes: `reqId, errorCode,
errorString`; then with `advancedOrderRejectJson` after them; now with
`errorTime` second as well. A charge was reported on `commissionReport` before
the fees were reported beside the commission. A program moved here unchanged
declares the shape its release had, and called with any other, its handler
raised: the refusals of its orders and their charges reached nobody, and only a
log line said so.
"""

import pytest

import ibkr_dx


class Recorder(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []
        self.charges = []


class Oldest(Recorder):
    """Written for a release before the advanced reject."""

    def error(self, reqId, errorCode, errorString):
        self.errors.append((reqId, errorCode, errorString))

    def commissionReport(self, commissionReport):
        self.charges.append(commissionReport.commission)


class WithoutTime(Recorder):
    """Written for a release with the advanced reject and no time."""

    def error(self, reqId, errorCode, errorString, advancedOrderRejectJson=""):
        self.errors.append((reqId, errorCode, errorString, advancedOrderRejectJson))

    def commissionReport(self, commissionReport):
        self.charges.append(commissionReport.commission)


class Current(Recorder):
    """Written for the current release, taking whatever it is given."""

    def error(self, *args):
        self.errors.append((args[0], *args[2:]))

    def commissionAndFeesReport(self, commissionAndFeesReport):
        self.charges.append(commissionAndFeesReport.commissionAndFees)


class Both(Current):
    """Written for the current release, keeping the former charge name too."""

    def commissionReport(self, commissionReport):
        self.charges.append(("former", commissionReport.commission))


@pytest.mark.parametrize(
    "wrapper, error",
    [
        (Oldest, (7, 162, "no data")),
        (WithoutTime, (7, 162, "no data", "")),
        (Current, (7, 162, "no data", "")),
        (Both, (7, 162, "no data", "")),
    ],
)
def test_error_and_the_charge_reach_the_shape_the_wrapper_declares(wrapper, error):
    w = wrapper()
    c = ibkr_dx.EClient(w)
    c._test_connect("T")
    c._test_push_historical_error(7, 162, "no data")
    c._test_push_fill(0, 77, "BUY", 150.0, 10, 0, 1.25)
    c._test_dispatch_once()
    c._test_dispatch_once()

    assert w.errors == [error], w.errors
    assert w.charges == [1.25], w.charges
