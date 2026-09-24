"""The protocol level names what this client carries, and what it cannot apply
is refused rather than dropped.

The reference client's `serverVersion()` is the API level of the process a
program talks to. Here that process is this client, so the number is the newest
of the reference's gates whose feature is carried, and a caller comparing
against a gate is told the truth about everything below it or refused by name.
"""

import ibkr_dx


class _Recorder(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []
        self.ended = []

    def error(self, reqId, errorTime, code, msg, advanced=""):
        self.errors.append((reqId, code, msg))

    def execDetailsEnd(self, reqId):
        self.ended.append(reqId)


def test_the_level_is_the_newest_gate_carried_and_none_before_a_session():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    assert c.serverVersion() is None
    c._test_connect()
    # MIN_SERVER_VER_ADDITIONAL_ORDER_PARAMS_2 in the reference's table. The
    # exception list below it is the doc on the call.
    assert c.serverVersion() == 217
    # A lost connection being recovered is not the end of the session; the
    # reference client rides it out holding the number.
    c._test_set_connection_lost()
    c._test_dispatch_once()
    assert not c.isConnected()
    assert c.serverVersion() == 217
    c.disconnect()
    assert c.serverVersion() is None


def test_the_levels_named_above_it_are_levels_a_gateway_has():
    """A gateway announces nothing above 226, so a feature said to be absent
    at a level above that names a level there is not."""
    doc = ibkr_dx.EClient.server_version.__doc__
    assert "(227)" not in doc, doc
    assert "226 is the highest level a gateway announces" in " ".join(doc.split()), doc


class _Seen(_Recorder):
    def __init__(self):
        super().__init__()
        self.seen = []

    def execDetails(self, reqId, contract, execution):
        self.seen.append((reqId, execution.execId))


def _on_utc(monkeypatch, w):
    """A session counting days on UTC, whatever the process was told: the
    zone is a setting this client publishes, and one set around the suite
    would move the days these tests count."""
    monkeypatch.delenv("IBKR_DX_TZ", raising=False)
    c = ibkr_dx.EClient(w)
    c._test_connect()
    return c


def test_a_window_in_days_or_dates_is_applied(monkeypatch):
    """Days are selected as a gateway selects them, counted on the session's
    clock, which is UTC unless the session names another."""
    import datetime

    w = _Seen()
    c = _on_utc(monkeypatch, w)
    today = datetime.datetime.now(datetime.timezone.utc).date()
    stamp = lambda back: (today - datetime.timedelta(days=back)).strftime("%Y%m%d") + "-00:00:01"
    for back in (0, 2, 5):
        c._test_store_execution(f"back{back}", stamp(back))

    stated = ibkr_dx.ExecutionFilter()
    stated.lastNDays = 3
    c.reqExecutions(1, stated)

    dated = ibkr_dx.ExecutionFilter()
    dated.specificDates = [int((today - datetime.timedelta(days=5)).strftime("%Y%m%d"))]
    c.reqExecutions(2, dated)
    c._test_dispatch_once()

    assert not w.errors, w.errors
    assert sorted(w.seen) == [(1, "back0"), (1, "back2"), (2, "back5")], w.seen
    assert w.ended == [1, 2]


def test_a_date_a_gateway_cannot_read_refuses_the_request(monkeypatch):
    """A gateway reads each date as a whole number and then as a day of the
    calendar, and refuses the request where either fails (320). Dropped
    instead, the request asked for no window and was answered with every
    execution held."""
    w = _Seen()
    c = _on_utc(monkeypatch, w)
    c._test_store_execution("held", "20260101-00:00:01")
    for req_id, dates in ((1, [20260231]), (2, ["2026-09-19"]), (3, [None])):
        f = ibkr_dx.ExecutionFilter()
        f.specificDates = dates
        c.reqExecutions(req_id, f)
    c._test_dispatch_once()
    assert [(r, code) for r, code, _ in w.errors] == [(1, 320), (2, 320), (3, 320)], w.errors
    assert "for input string: '2026-09-19'" in w.errors[1][2], w.errors
    assert w.seen == [], "nothing answered for a refused request"
    assert w.ended == [1, 2, 3], "and each still ends, as every refusal on this surface does"


def test_dates_are_taken_as_the_reference_writes_them(monkeypatch):
    """Whatever the dates are held in is written out one by one, as the
    reference writes them; and a date named twice is one day, as a gateway
    keeps them."""
    import datetime

    w = _Seen()
    c = _on_utc(monkeypatch, w)
    today = datetime.datetime.now(datetime.timezone.utc).date()
    ymd = lambda back: int((today - datetime.timedelta(days=back)).strftime("%Y%m%d"))
    stamp = lambda back: (today - datetime.timedelta(days=back)).strftime("%Y%m%d") + "-00:00:01"
    for back in (0, 2):
        c._test_store_execution(f"back{back}", stamp(back))

    in_a_set = ibkr_dx.ExecutionFilter()
    in_a_set.specificDates = {ymd(2)}
    c.reqExecutions(1, in_a_set)
    twice = ibkr_dx.ExecutionFilter()
    twice.specificDates = [ymd(0), str(ymd(0))]
    c.reqExecutions(2, twice)
    c._test_dispatch_once()

    assert not w.errors, w.errors
    assert sorted(w.seen) == [(1, "back2"), (2, "back0"), (2, "back2")], w.seen


def test_the_reference_defaults_pass_through():
    w = _Recorder()
    c = ibkr_dx.EClient(w)
    c._test_connect()
    # UNSET_INTEGER and None, which is what a filter that states no window carries.
    c.reqExecutions(3, ibkr_dx.ExecutionFilter())
    c._test_dispatch_once()
    assert not w.errors, w.errors
    assert 3 in w.ended


def test_every_surface_answers_the_question_with_one_number():
    # "What protocol level am I talking to" is answered with the level, not
    # with the build this client states at logon — a number on another scale,
    # above every level there is, which tells a program gating a feature on it
    # that every feature exists.
    import subprocess
    import pathlib

    root = pathlib.Path(__file__).resolve().parents[2]
    stated = subprocess.run(
        ["grep", "-rn", "PROTOCOL_LEVEL", str(root / "src")],
        capture_output=True, text=True,
    ).stdout
    answering = [line for line in stated.splitlines() if "client_core::PROTOCOL_LEVEL" in line]
    assert len(answering) >= 1, f"a surface answers with something else:\n{stated}"


def test_the_most_a_beta_hedge_may_trade_is_a_field_of_an_order():
    """`hedgeMaxSize` is taken, at the reference client's unset value until
    stated, and `conditionsIncludeOvernight` is absent and says so on use."""
    order = ibkr_dx.Order()
    assert order.hedgeMaxSize == 2147483647
    order.hedgeMaxSize = 50
    assert (order.hedgeMaxSize, order.hedge_max_size) == (50, 50)
    try:
        order.conditionsIncludeOvernight = True
    except AttributeError:
        pass
    else:
        raise AssertionError("a field this client does not have took a value")
