"""A bar is dated the way ib_async's parser reads one.

Skipped where ib_async is not installed: it is not a dependency of this
package, and is not vendored.
"""

import pytest

pytest.importorskip("ib_async")


def test_a_bar_is_dated_the_way_their_parser_reads_one():
    """Their own first documented example is `util.df(bars)`.

    ib_async parses the date itself and decides the shape from the string: a
    day from eight digits, a moment in seconds from all digits, and an aware
    moment from a date, a time and a zone separated by single spaces.

    The venue stamps a bar in UTC and names the exchange's zone beside it. Put
    together by writing one after the other, their parser reads the UTC time as
    a time on the exchange's clock, and every bar is out by whatever that zone
    is from UTC.
    """
    from ib_async.util import parseIBDatetime

    import ibkr_dx

    seen = []

    class W(ibkr_dx.EWrapper):
        def historical_data(self, req_id, bar):
            seen.append(bar.date)

        def error(self, *a):
            pass

    c = ibkr_dx.EClient(W())
    c._test_connect("T")
    c._test_push_historical_data(
        1, [("20260812-13:30:00", 1.0, 2.0, 0.5, 1.5, 100)], True, "US/Eastern"
    )
    c._test_dispatch_once()

    read = parseIBDatetime(seen[0])
    assert read.tzinfo is not None, "a naive moment cannot be converted to a zone"
    assert read.isoformat() == "2026-08-12T09:30:00-04:00", seen[0]
    # The same instant the venue stamped, which is the whole point.
    assert int(read.timestamp()) == 1786541400
