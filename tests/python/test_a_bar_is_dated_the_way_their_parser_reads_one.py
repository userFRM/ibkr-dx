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


@pytest.mark.parametrize("format_date", [1, 2])
@pytest.mark.parametrize("size,stated,end,day", [
    ("1 day", "20260924-13:30:00", "20260924-20:00:00", "20260924"),
    ("1 day", "20260923-22:00:00", "20260924-21:00:00", "20260924"),
    ("1 week", "20260921", "20260926", "20260921"),
    ("1 month", "20260901", "20261001", "20260901"),
])
def test_daily_and_longer_bars_keep_their_dates(format_date, size, stated, end, day):
    import ibkr_dx
    from ib_async.util import parseIBDatetime

    seen = []

    class W(ibkr_dx.EWrapper):
        def historical_data(self, req_id, bar):
            seen.append((req_id, bar.date))

    c = ibkr_dx.EClient(W())
    c._test_connect("T")
    contract = ibkr_dx.Contract()
    contract.conId = 756733
    contract.secType, contract.exchange = "STK", "SMART"
    c.req_historical_data(1, contract, "", "1 Y", size, "TRADES", 1, format_date)
    c._test_push_historical_data(
        1, [(stated, 1.0, 2.0, 0.5, 1.5, 100)], True, "US/Eastern", [end]
    )
    c._test_dispatch_once()
    assert seen == [(1, day)]
    assert parseIBDatetime(day).strftime("%Y%m%d") == day
