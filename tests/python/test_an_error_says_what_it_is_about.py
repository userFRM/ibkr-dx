"""An error says what it is about.

The number `error` carries can be a request's, an order's, a lookup's this
client made for itself, or none, and does not say which. `error_from` is told
the same error with its origin: a request and whether nothing more follows for
it, an order and the operation on it, a request that carries no number, the
session, or an internal lookup. A subclass of `EWrapper` that overrides only
`error` is told exactly what it was told before; a wrapper that is not one is
called on `error`, as the reference client calls it. A refusal the program
makes itself, with the origin it states, takes its place among them.
"""

import time

import pytest

import ibkr_dx

INTERNAL = 0xC000_0001


class Origins(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.said = []

    def error_from(self, origin, error_time, code, msg, advanced_order_reject_json=""):
        self.said.append(
            (origin.kind, origin.id, origin.ends, origin.op, origin.question, code),
        )

    def openOrder(self, order_id, contract, order, state):
        self.said.append(("open_order", order_id))

    def openOrderEnd(self):
        self.said.append(("open_order_end",))

    def execDetails(self, req_id, contract, execution):
        self.said.append(("exec_details", req_id))

    def execDetailsEnd(self, req_id):
        self.said.append(("exec_details_end", req_id))


class OnlyError(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.said = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.said.append((req_id, code))


class NotASubclass:
    """A wrapper written against the reference client, with nothing of ours."""

    def __init__(self):
        self.said = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.said.append((req_id, code))


def spy():
    c = ibkr_dx.Contract()
    c.conId = 756733
    c.symbol = "SPY"
    c.secType = "STK"
    c.exchange = "SMART"
    c.currency = "USD"
    return c


def unsendable():
    o = ibkr_dx.Order()
    o.action = "BUY"
    o.totalQuantity = 1
    o.orderType = "NOT A TYPE"
    o.lmtPrice = 100.0
    o.tif = "DAY"
    return o


def connected(w, **kwargs):
    c = ibkr_dx.EClient(w)
    c._test_connect("T", **kwargs)
    return c


def test_an_orders_error_is_heard_under_a_number_a_finished_lookup_had():
    """What answers a lookup of this client's own under a number from the band
    those lookups take, once nobody is waiting on it, is heard by nobody. An
    order can be numbered there, and is heard."""
    w = Origins()
    c = connected(w)
    c._test_push_historical_error(INTERNAL, 200, "no security definition")
    c._test_push_order_inactive(INTERNAL, 201, "refused")
    c.poll()
    assert w.said == [("Order", INTERNAL, None, "Venue", None, 201)], w.said


def test_a_wrapper_overriding_only_error_is_told_what_it_was_told_before():
    for w in (OnlyError(), NotASubclass()):
        c = connected(w)
        c._test_push_order_inactive(INTERNAL, 201, "refused")
        c._test_push_historical_error(7, 162, "no data")
        c.poll()
        assert w.said == [(INTERNAL, 201), (7, 162)], (type(w), w.said)


def test_a_refused_new_order_and_a_refused_modify_of_one_id_carry_place_and_modify():
    w = Origins()
    c = connected(w)
    c._test_map_con_id(756733, 0)
    c.placeOrder(9, spy(), unsendable())
    # The venue is working it now.
    c._test_track_order(9, 0, "SPY", "BUY", 1, 100.0)
    c.placeOrder(9, spy(), unsendable())
    c.poll()
    assert [(kind, id_, op) for kind, id_, _, op, _, _ in w.said] == [
        ("Order", 9, "Place"),
        ("Order", 9, "Modify"),
    ], w.said


def test_the_executions_notice_does_not_end_the_request_and_its_answer_follows():
    import time

    w = Origins()
    c = connected(w)
    c._test_store_execution("e1", "")
    # What this session holds starts now, so the days before are not held.
    c._test_hold_executions_from(int(time.time()))
    f = ibkr_dx.ExecutionFilter()
    f.lastNDays = 2
    c.reqExecutions(7, f)
    c.poll()
    assert w.said == [
        ("Request", 7, False, None, None, 321),
        ("exec_details", 7),
        ("exec_details_end", 7),
    ], w.said


def test_the_open_orders_notice_does_not_end_the_question_and_its_answer_follows():
    w = Origins()
    c = connected(w, replay_done=False)
    c._test_begin_order_replay()
    c._test_track_order(3, 0, "SPY", "BUY", 1, 100.0)
    c.reqOpenOrders()
    # Held until the naming's bound has passed, then answered with a notice.
    deadline = time.monotonic() + 6
    while not w.said and time.monotonic() < deadline:
        c.poll()
        time.sleep(0.02)
    notice, *rest = w.said
    assert notice[:5] == ("Question", -1, False, None, "OpenOrders"), w.said
    assert rest == [("open_order", 3), ("open_order_end",)], w.said


def test_a_global_cancel_refused_is_the_sessions():
    w = Origins()
    c = connected(w, readonly=True)
    c.reqGlobalCancel()
    c.poll()
    assert w.said and w.said[0][:3] == ("Session", -1, None), w.said


@pytest.mark.parametrize(
    "ask, question",
    [
        (lambda c: c.reqPositions(), "Positions"),
        (lambda c: c.reqAccountUpdates(True, ""), "AccountUpdates"),
        (lambda c: c.reqManagedAccts(), "ManagedAccounts"),
        (lambda c: c.reqCurrentTime(), "CurrentTime"),
        (lambda c: c.reqCurrentTimeInMillis(), "CurrentTimeInMillis"),
        (lambda c: c.reqNewsProviders(), "NewsProviders"),
        (lambda c: c.reqMarketRule(26), "MarketRule(26)"),
        (lambda c: c.reqFamilyCodes(), "FamilyCodes"),
        (lambda c: c.reqMktDepthExchanges(), "MktDepthExchanges"),
        (lambda c: c.reqScannerParameters(), "ScannerParameters"),
        (lambda c: c.requestFA(1), "Fa"),
        (lambda c: c.reqOpenOrders(), "OpenOrders"),
        (lambda c: c.reqAllOpenOrders(), "AllOpenOrders"),
        (lambda c: c.reqCompletedOrders(False), "CompletedOrders"),
    ],
)
def test_a_question_refused_says_which_question_it_ends(ask, question):
    w = Origins()
    c = connected(w)
    c._test_end_session()
    ask(c)
    c.poll()
    assert w.said and w.said[0][:5] == ("Question", -1, True, None, question), w.said


def test_withdrawing_the_accounts_figures_or_holdings_is_the_sessions():
    for withdraw in (lambda c: c.reqAccountUpdates(False, ""), lambda c: c.cancelPositions()):
        w = Origins()
        c = connected(w)
        c._test_end_session()
        withdraw(c)
        c.poll()
        assert w.said and w.said[0][:3] == ("Session", -1, None), w.said


def test_a_refusal_the_program_makes_arrives_in_the_order_it_was_made():
    """Said at the call, a program's own refusal reached the wrapper ahead of
    the venue's refusal of a request made before it. Put into the session's
    order, it arrives where a gateway's would."""
    w = OnlyError()
    c = connected(w)
    c._test_push_historical_error(1, 162, "the venue's, of the request before")
    c.refuse(ibkr_dx.ErrorOrigin("Request", 2), 320, "the program's own")
    c._test_push_historical_error(3, 162, "the venue's, of the request after")
    c.poll()
    assert w.said == [(1, 162), (2, 320), (3, 162)], w.said


@pytest.mark.parametrize(
    "stated",
    [
        ("Request", 7, False, None, None),
        ("Order", 9, None, "Place", None),
        ("Order", 9, None, "Modify", None),
        ("Order", 9, None, "Cancel", None),
        ("Order", 9, None, "Exercise", None),
        ("Order", 9, None, "Venue", None),
        ("Question", -1, True, None, "MarketRule(26)"),
        ("Question", -1, False, None, "OpenOrders"),
        ("Session", -1, None, None, None),
        ("Internal", INTERNAL, None, None, None),
    ],
)
def test_an_origin_is_made_from_what_it_reads_back(stated):
    kind, id_, ends, op, question = stated
    made = ibkr_dx.ErrorOrigin(kind, id_, True if ends is None else ends, op, question)
    assert (made.kind, made.id, made.ends, made.op, made.question) == stated
