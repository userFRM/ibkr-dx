"""What one surface answered and the other lacked is answered on both.

The Rust surface held a session's end, a contract's slot, the frames the venue
sent, the withdrawal of a headlines query, a scan, the corporate-events
calendar, the account's holdings and an order's preview as calls of their own;
a Python program had no way to ask any of them. And the reference client's four
verification calls and two quiet withdrawals were absent from both.
"""

import pytest

import ibkr_dx
from conftest import NotConnectedProbe
from ibkr_dx import Contract, Order


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
    c._test_map_con_id(756733, 0)
    return w, c


def _spy():
    c = Contract()
    c.conId, c.symbol, c.secType, c.exchange, c.currency = 756733, "SPY", "STK", "SMART", "USD"
    return c


def test_a_session_that_ended_says_so():
    _, c = _client()
    assert c.session_over() is False
    c._test_end_session()
    assert c.sessionOver() is True
    c.disconnect()
    assert c.session_over() is True


def test_a_contract_names_the_slot_it_holds():
    _, c = _client()
    assert c.instrument_of(756733) == 0
    assert c.instrumentOf(265598) is None


def test_no_frames_are_kept_unless_asked_for(monkeypatch):
    monkeypatch.delenv("IBKR_DX_CAPTURE_WIRE", raising=False)
    _, c = _client()
    assert c.unread_wire() == []


def test_a_headlines_query_is_withdrawn():
    _, c = _client()
    c.cancel_historical_news(9)
    assert any("CancelHistoricalNews { req_id: 9 }" in cmd for cmd in c._test_take_commands())


def test_the_holdings_are_handed_back():
    _, c = _client()
    c._test_set_position(756733, 3.0, 412.5)
    c._test_finish_account_download()
    held = c.positions()
    assert [(a, pos, cost) for a, _, pos, cost in held] == [("DU1", 3.0, 412.5)]
    assert held[0][1].conId == 756733


def test_a_preview_is_answered_with_what_the_venue_states():
    _, c = _client()
    handed = c.next_order_id()
    asked_under = c._test_peek_ask_id()
    c._test_push_what_if(asked_under, 0, 100.0, 90.0, 1000.0, 150.0, 140.0, 990.0, 1.25)
    order = Order()
    order.action, order.orderType, order.totalQuantity, order.lmtPrice = "BUY", "LMT", 1, 1.0
    state = c.what_if_order(_spy(), order)
    assert float(state.init_margin_after) == 150.0
    assert order.what_if is False, "the caller's order is not changed"
    commands = c._test_take_commands()
    assert any("what_if: true" in cmd for cmd in commands), commands
    # A preview reaches nothing, so its number is not left reading as a
    # working order's.
    c.cancel_order(asked_under)
    assert not [cmd for cmd in c._test_take_commands() if "Cancel" in cmd]
    # And the preview's number is the question's own: the numbers handed out
    # go on from where they were.
    assert c.next_order_id() == handed + 1


def test_a_refused_preview_raises_with_the_venues_words():
    _, c = _client()
    asked_under = c._test_peek_ask_id()
    c._test_push_order_inactive(asked_under, 201, "Order rejected - reason:no margin")
    order = Order()
    order.action, order.orderType, order.totalQuantity, order.lmtPrice = "BUY", "LMT", 1, 1.0
    with pytest.raises(RuntimeError, match=r"no margin \(201\)"):
        c.what_if_order(_spy(), order)


def test_what_is_said_about_a_preview_is_the_calls():
    """A preview's number is the call's own, so nothing said about the
    preview reaches the program under it: not a notice, and not a refusal this
    client makes, which the call raises with its words."""
    w, c = _client()
    asked_under = c._test_peek_ask_id()
    c._test_push_what_if(asked_under, 0, 100.0, 90.0, 1000.0, 150.0, 140.0, 990.0, 1.25)
    order = Order()
    order.action, order.orderType, order.totalQuantity, order.lmtPrice = "BUY", "LMT", 1, 1.0
    order.optOutSmartRouting = True
    assert float(c.what_if_order(_spy(), order).init_margin_after) == 150.0
    c._test_dispatch_once()
    assert not [e for e in w.seen if e[0] == asked_under], w.seen

    asked_under = c._test_peek_ask_id()
    order.optOutSmartRouting = False
    order.transmit = False
    with pytest.raises(RuntimeError, match=r"transmit flag set to TRUE.*\(321\)"):
        c.what_if_order(_spy(), order)
    c._test_dispatch_once()
    assert not [e for e in w.seen if e[0] == asked_under], w.seen


def test_a_scan_is_answered_and_withdrawn():
    _, c = _client()
    asked_under = c._test_peek_ask_id()
    c._test_push_scanner_data(asked_under, [756733, 265598])
    rows = c.scan("STK", "STK.US.MAJOR", "TOP_PERC_GAIN", 10)
    assert [(rank, details.contract.conId) for rank, details, *_ in rows] == [(0, 756733), (1, 265598)]
    commands = c._test_take_commands()
    assert any("SubscribeScanner" in cmd and "TOP_PERC_GAIN" in cmd for cmd in commands), commands
    assert any(f"CancelScanner {{ req_id: {asked_under} }}" in cmd for cmd in commands), commands


def test_a_scan_the_venue_will_not_run_raises_with_its_words():
    _, c = _client()
    asked_under = c._test_peek_ask_id()
    c._test_push_scanner_data(asked_under, [], "Scanner type is not allowed")
    with pytest.raises(RuntimeError, match="Scanner type is not allowed"):
        c.scan("STK", "STK.US.MAJOR", "NOPE", 10)


def test_the_calendar_is_answered_as_the_venue_wrote_it():
    _, c = _client()
    asked_under = c._test_peek_ask_id()
    c._test_push_calendar(asked_under, '{"meta": 1}')
    assert c.calendar_schema() == '{"meta": 1}'
    asked_under = c._test_peek_ask_id()
    c._test_push_calendar(asked_under, '{"events": []}', events=True)
    assert c.calendar_events(265598) == '{"events": []}'
    assert any("con_id: Some(265598)" in cmd for cmd in c._test_take_commands())


BAD_MESSAGE = "Bad message  Intent to authenticate needs to be expressed during initial connect request."


def test_the_verification_requests_are_answered_as_the_reference_client_answers_them():
    w, c = _client()
    c.verifyRequest("app", "1")
    c.verifyAndAuthRequest("app", "1", "key")
    c.verifyMessage("data")
    c.verifyAndAuthMessage("data", "response")
    c.cancelContractData(5)
    c.cancelHistoricalTicks(6)
    c.poll()
    assert w.seen == [(-1, 508, BAD_MESSAGE), (-1, 508, BAD_MESSAGE)]
    assert c._test_take_commands() == [], "nothing is sent for any of them"


def test_without_a_session_each_is_answered_under_504():
    probe = NotConnectedProbe()
    c = ibkr_dx.EClient(probe)
    c.verify_request("app", "1")
    c.verify_and_auth_request("app", "1", "key")
    c.verify_message("data")
    c.verify_and_auth_message("data", "response")
    c.cancel_contract_data(5)
    c.cancel_historical_ticks(6)
    assert [(r, code) for r, code, _ in probe.errors] == [
        (-1, 504), (-1, 504), (-1, 504), (-1, 504), (5, 504), (6, 504),
    ]


def test_a_dispatch_pass_leaves_an_answering_call_its_answer():
    """A program's own loop reads the session beside these calls, and what
    arrives under a call's number is the call's, not the loop's."""
    w, c = _client()
    asked_under = c._test_peek_ask_id()
    c._test_push_what_if(asked_under, 0, 100.0, 90.0, 1000.0, 150.0, 140.0, 990.0, 1.25)
    c._test_dispatch_once()
    order = Order()
    order.action, order.orderType, order.totalQuantity, order.lmtPrice = "BUY", "LMT", 1, 1.0
    assert float(c.what_if_order(_spy(), order).init_margin_after) == 150.0

    asked_under = c._test_peek_ask_id()
    c._test_push_scanner_data(asked_under, [756733])
    c._test_dispatch_once()
    assert [rank for rank, *_ in c.scan("STK", "STK.US.MAJOR", "TOP_PERC_GAIN", 10)] == [0]

    asked_under = c._test_peek_ask_id()
    c._test_push_order_inactive(asked_under, 201, "Order rejected - reason:no margin")
    c._test_dispatch_once()
    with pytest.raises(RuntimeError, match=r"\(201\)"):
        c.what_if_order(_spy(), order)
    assert not [e for e in w.seen if e[0] == asked_under], "the refusal was the call's"
