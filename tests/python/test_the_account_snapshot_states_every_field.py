"""The account snapshot states every field the account carries.

Two were left out, and one of them is the only home of the plain maintenance
margin, so a caller reading the maintenance margin read the full spelling
alone.
"""

import ibkr_dx


def test_the_account_snapshot_states_every_field():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("T")
    c._test_finish_account_download()
    snapshot = c.account_snapshot()
    assert snapshot is not None
    assert {"accrued_cash", "margin_used"} <= set(snapshot), sorted(snapshot)
