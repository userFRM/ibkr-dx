"""ADJUSTED_LAST is served on the callback API, not refused before it is sent.

The venue carries no adjusted series: what it serves is raw trades, and an
adjusted series is those trades folded with the contract's own corporate
actions. The engine asks for the actions first, then for the raw bars along the
ids they state the contract traded under, and folds them; the waiting call
`historical_data` hands the series back in one piece and the callback path
delivers it bar by bar. A caller on the callback
API — which is the API — could not get an adjusted series at all before, and
can now.

The fold itself is exercised at the engine, where the two replies land and are
correlated, so both surfaces deliver the same adjusted bars. Here is the
callback surface's own half: it accepts the request rather than refusing it,
and it refuses what a gateway refuses before asking the venue — the series kept
up to date, an end date, and a bar longer than a day.
"""

import ibkr_dx


class _Recorder(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []

    def error(self, reqId, errorTime, code, msg, advanced=""):
        self.errors.append((reqId, code, msg))


def _client():
    w = _Recorder()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU0000000")
    return w, c


def _spy():
    c = ibkr_dx.Contract()
    c.symbol, c.secType, c.exchange, c.currency = "SPY", "STK", "SMART", "USD"
    c.conId = 756733
    return c


def test_adjusted_last_is_accepted_on_the_callback_path():
    """A qualified contract asked for adjusted is not refused: the request is
    made, and the fold happens as its bars arrive."""
    w, c = _client()
    c.req_historical_data(
        1, _spy(), end_date_time="", duration_str="1 Y",
        bar_size_setting="1 day", what_to_show="ADJUSTED_LAST", use_rth=1,
    )
    assert w.errors == [], w.errors


def test_adjusted_last_without_the_venue_id_is_sent():
    """The reference client forwards the contract description and the data type
    it was given. A contract stated by description is one the venue can resolve
    itself, so the request goes and the venue answers it."""
    w, c = _client()
    unqualified = ibkr_dx.Contract()
    unqualified.symbol, unqualified.secType = "SPY", "STK"
    unqualified.exchange, unqualified.currency = "SMART", "USD"
    c.req_historical_data(
        2, unqualified, end_date_time="", duration_str="1 Y",
        bar_size_setting="1 day", what_to_show="ADJUSTED_LAST", use_rth=1,
    )
    assert not [e for e in w.errors if "venue's id" in e[2]], w.errors


def test_adjusted_last_kept_up_to_date_is_refused():
    """A gateway keeps no bar current for the adjusted series, and refuses it
    kept up to date in these words."""
    w, c = _client()
    c.req_historical_data(
        3, _spy(), end_date_time="", duration_str="1 D",
        bar_size_setting="5 mins", what_to_show="ADJUSTED_LAST", use_rth=0,
        keep_up_to_date=True,
    )
    c.poll()
    assert (3, 321, "Source price not supported with live updates") in w.errors, w.errors


def test_what_a_gateway_refuses_before_asking_is_refused_in_its_words():
    """An end date or a bar longer than a day on the adjusted series, and an end
    date, a combination or a series with no live bar on a request kept up to
    date: each refused with the gateway's reason, as on the other surface."""
    w, c = _client()
    combo = _spy()
    combo.secType = "BAG"
    cases = [
        (4, _spy(), "20250101 00:00:00", "1 day", "ADJUSTED_LAST", False,
         "End date not supported with adjusted last"),
        (5, _spy(), "", "1 week", "ADJUSTED_LAST", False,
         "Multi day bar size not supported with adjusted last"),
        (6, _spy(), "20250101 00:00:00", "5 mins", "TRADES", True,
         "End date not supported with live updates"),
        (7, combo, "", "5 mins", "TRADES", True, "Live updates for combos are not supported"),
        (8, _spy(), "", "5 mins", "BID_ASK", True, "Source price not supported with live updates"),
    ]
    for req_id, contract, end, size, series, keep, reason in cases:
        c.req_historical_data(
            req_id, contract, end_date_time=end, duration_str="1 M",
            bar_size_setting=size, what_to_show=series, use_rth=1, keep_up_to_date=keep,
        )
        c.poll()
        assert (req_id, 321, reason) in w.errors, (reason, w.errors)
    # A week and a month are kept up to date.
    for req_id, size in [(9, "1 week"), (10, "1 month")]:
        c.req_historical_data(
            req_id, _spy(), end_date_time="", duration_str="1 Y",
            bar_size_setting=size, what_to_show="TRADES", use_rth=1, keep_up_to_date=True,
        )
        assert not [e for e in w.errors if e[0] == req_id], w.errors
