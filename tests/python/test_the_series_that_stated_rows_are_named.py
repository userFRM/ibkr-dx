"""The series that stated rows for a subscription are named by its number.

Figures, numbered figures and pairs each have a call that names the series
holding them, so a caller can read what arrived without knowing every series by
heart. Rows had none: a program had to ask each series it could think of.
"""

import ibkr_dx


def test_the_series_that_stated_rows_are_named():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU1")
    c._test_map_instrument(1, 0)
    assert c.stated_rows_series(1) == []
    c._test_note_stated_rows(0, 547, [(10.0, 99.5, 100.5)])
    c._test_note_stated_rows(0, 320, [(1.0, 12345.0, 2.0)])
    assert c.stated_rows_series(1) == [320, 547]
    assert c.stated_rows_series(2) == [], "a number naming no subscription"
