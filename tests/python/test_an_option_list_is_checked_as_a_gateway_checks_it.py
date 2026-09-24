"""Every request that carries a free-form option list has it checked as a
gateway checks it.

A gateway takes one key on each of these, `manual`, and it changes nothing the
gateway sends. Any other key is refused under 10337 and names the request, a
value other than nought or one under 10338, and an entry not written
`key=value` under 320. Here the list was taken and thrown away, so a program
that a gateway would have told was told nothing and its request went out.

The two option calculations take no key at all, so a gateway refuses any
under 10337 and names none it takes. A fundamental report is the other
exception: a gateway reads no list on that request at all, so nothing in one is
checked there or here.
"""

import re

import pytest

import ibkr_dx
from ibkr_dx import Contract, ScannerSubscription, TagValue


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.seen.append((req_id, code, msg))


def _client():
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU1")
    # The contract already holds a slot, so a request that is not refused
    # goes to the channel without waiting on an engine this session lacks.
    c._test_map_con_id(756733, 0)
    return w, c


def _spy():
    c = Contract()
    c.conId, c.symbol, c.secType, c.exchange, c.currency = 756733, "SPY", "STK", "SMART", "USD"
    return c


def _scan():
    s = ScannerSubscription()
    s.instrument, s.locationCode, s.scanCode = "STK", "STK.US.MAJOR", "TOP_PERC_GAIN"
    return s


# (the request as a gateway names it, a call that sends it under `n` with `options`)
REQUESTS = [
    ("ReqMktData(1)", lambda c, n, o: c.reqMktData(n, _spy(), "", False, False, o)),
    ("ReqMktDepth(10)", lambda c, n, o: c.reqMktDepth(n, _spy(), 5, False, o)),
    ("ReqHistoricalData(20)", lambda c, n, o: c.reqHistoricalData(
        n, _spy(), "", "1 D", "1 hour", "TRADES", 1, 1, False, o)),
    ("ReqScannerSubscription(22)", lambda c, n, o: c.reqScannerSubscription(n, _scan(), o, [])),
    ("ReqRealTimeBars(50)", lambda c, n, o: c.reqRealTimeBars(n, _spy(), 5, "TRADES", 1, o)),
    ("ReqNewsArticle(84)", lambda c, n, o: c.reqNewsArticle(n, "BRFG", "x", o)),
    ("ReqHistoricalNews(86)", lambda c, n, o: c.reqHistoricalNews(n, 756733, "BRFG", "", "", 5, o)),
    ("ReqHistoricalTicks(96)", lambda c, n, o: c.reqHistoricalTicks(
        n, _spy(), "20260102 10:00:00", "", 10, "TRADES", 1, False, o)),
]


@pytest.mark.parametrize("name,call", REQUESTS, ids=[r[0] for r in REQUESTS])
def test_a_key_the_request_does_not_take_is_refused_and_nothing_is_sent(name, call):
    w, c = _client()
    call(c, 1, [TagValue("foo", "1")])
    assert w.seen == [(1, 10337, f"Misc options key=foo is invalid in {name} request. Valid keys are: manual")]
    assert c._test_take_commands() == []


@pytest.mark.parametrize("name,call", REQUESTS, ids=[r[0] for r in REQUESTS])
def test_a_value_manual_does_not_take_is_refused(name, call):
    w, c = _client()
    call(c, 1, [TagValue("manual", "2")])
    assert w.seen == [(1, 10338, f"Misc options value=2 is invalid for key=manual in {name} request. "
                                 "Valid values are: 0, 1")]
    assert c._test_take_commands() == []


@pytest.mark.parametrize("name,call", REQUESTS, ids=[r[0] for r in REQUESTS])
def test_an_entry_that_is_not_key_value_cannot_be_read(name, call):
    w, c = _client()
    call(c, 1, [TagValue("manual", "")])
    assert w.seen == [(1, 320, "Error reading request:Please use 'Key=Value' format for Misc Options")]


@pytest.mark.parametrize("value", ["1", "0"])
@pytest.mark.parametrize("name,call", REQUESTS, ids=[r[0] for r in REQUESTS])
def test_manual_is_taken_and_changes_nothing(name, call, value):
    # Two sessions, so the second request is not answered by what the first
    # already holds.
    plain_w, plain = _client()
    call(plain, 1, [])
    w, c = _client()
    call(c, 1, [TagValue("manual", value)])
    assert w.seen == plain_w.seen, (w.seen, plain_w.seen)
    # Each session counts what it asks for from its own start, so the count
    # is the one thing two identical requests may not share.
    sent = lambda client: [re.sub(r"issued: \d+", "", cmd) for cmd in client._test_take_commands()]
    assert sent(c) == sent(plain)


CALCULATIONS = [
    ("ReqCalcImpliedVolatility(54)", lambda c, n, o: c.calculateImpliedVolatility(n, _spy(), 1.0, 100.0, o)),
    ("ReqCalcOptionPrice(55)", lambda c, n, o: c.calculateOptionPrice(n, _spy(), 0.2, 100.0, o)),
]


@pytest.mark.parametrize("name,call", CALCULATIONS, ids=[r[0] for r in CALCULATIONS])
def test_a_request_that_takes_no_key_refuses_every_one(name, call):
    w, c = _client()
    call(c, 5, [TagValue("manual", "1")])
    assert w.seen == [(5, 10337, f"Misc options key=manual is invalid in {name} request. Valid keys are: ")]
    assert c._test_take_commands() == []


def test_a_fundamental_report_reads_no_list():
    w, c = _client()
    c.reqFundamentalData(1, _spy(), "ReportSnapshot", [TagValue("foo", "1")])
    assert w.seen == []
    assert any("FetchFundamentalData" in cmd for cmd in c._test_take_commands())


def test_an_entry_without_a_tag_and_value_is_written_as_its_text():
    """The reference client writes each entry as its text, so an object that
    is not a tag and a value reaches a gateway as whatever it prints, and is
    refused there as any entry it cannot read is."""
    w, c = _client()
    c.reqMktData(1, _spy(), "", False, False, [object()])
    assert [(r, code) for r, code, _ in w.seen] == [(1, 320)], w.seen
    assert c._test_take_commands() == []

    class Printed:
        def __str__(self):
            return "manual=1;"

    w, c = _client()
    c.reqMktData(1, _spy(), "", False, False, [Printed()])
    assert _refusals(w) == []


def _refusals(w):
    # This session has no engine, so a request that goes out ends in the
    # registration's timeout; what matters is that nothing refused it.
    return [e for e in w.seen if e[1] != -1]


def test_no_list_is_an_empty_one():
    w, c = _client()
    c.reqMktData(1, _spy(), "", False, False, None)
    c.reqHistoricalData(2, _spy(), "", "1 D", "1 hour", "TRADES", 1, 1, False, None)
    assert _refusals(w) == []
