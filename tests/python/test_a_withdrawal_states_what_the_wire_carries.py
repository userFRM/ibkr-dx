"""A withdrawal takes the object the reference client states it with.

That client's `cancelOrder` takes an `OrderCancel` and its `reqGlobalCancel`
takes one too — when a person entered the withdrawal, on whose authority, and
whether a person entered it at all. Passing one raised here, because the second
argument was a bare time string.

Who is withdrawing it and whether a person entered it travel on the cancel, as
a gateway writes them: from the withdrawal, not from the placement. The time
does not, because a gateway sends it only where the venue has turned that
record on for the login and this client does not read whether it has; the
order still comes back and the caller is told the time did not go with it. A
time a gateway cannot read is refused as a gateway refuses it, and the order
keeps working.
"""

import ibkr_dx


class Heard(ibkr_dx.EWrapper):
    def __init__(self):
        self.refusals = []

    def error(self, *said):
        # Kept whole: the reference client's callback carries five arguments
        # and this one only reads the sentence out of them.
        self.refusals.append(said)


def _client():
    heard = Heard()
    client = ibkr_dx.EClient(heard)
    client._test_connect("DU0000000")
    # A withdrawal names an order this client is working, or it is answered
    # rather than sent. What is under test here is what the withdrawal
    # carries, so the orders it names are placed first.
    for order_id in (1, 2):
        client._test_track_order(order_id, 0, "SPY", "BUY", 1.0, 100.0)
    return client, heard


def _said(client, heard):
    client._test_dispatch_once()
    return [
        held for said in heard.refusals for held in said
        if isinstance(held, str) and "withdrawal" in held
    ]


def _withdrew(client):
    return [cmd for cmd in client._test_take_commands() if "Cancel" in cmd]


def test_a_withdrawal_that_states_nothing_goes_through():
    client, heard = _client()
    client.cancelOrder(1, ibkr_dx.OrderCancel())
    sent = _withdrew(client)
    assert sent, "the order comes back"
    assert 'ext_operator: ""' in sent[0] and "manual_order_indicator: 2147483647" in sent[0], sent
    assert not _said(client, heard)


def test_the_object_is_taken_where_a_bare_time_was_taken_before():
    # The spelling this client had is still answered, so nothing that worked
    # stops working.
    client, heard = _client()
    client.cancelOrder(1, "")
    client.cancel_order(2)
    assert len(_withdrew(client)) == 2
    assert not _said(client, heard)


def test_the_operator_and_who_entered_it_travel_on_the_cancel():
    client, heard = _client()
    withdrawal = ibkr_dx.OrderCancel()
    withdrawal.extOperator = "someone"
    withdrawal.manualOrderIndicator = 1
    client.cancelOrder(1, withdrawal)
    sent = _withdrew(client)
    assert len(sent) == 1, sent
    assert 'ext_operator: "someone"' in sent[0], sent
    assert "manual_order_indicator: 1 " in sent[0], sent
    assert not _said(client, heard), "nothing is said about what travelled"


def test_a_time_the_wire_does_not_carry_is_said_and_the_order_still_comes_back():
    client, heard = _client()
    withdrawal = ibkr_dx.OrderCancel()
    withdrawal.manualOrderCancelTime = "20260902-14:30:00"
    withdrawal.extOperator = "someone"
    client.cancelOrder(1, withdrawal)

    sent = _withdrew(client)
    assert sent, "a record with nowhere to go does not keep an order working"
    assert 'ext_operator: "someone"' in sent[0], "the rest still travels"
    said = [t for t in _said(client, heard) if "withdrawal states a time" in t]
    assert said, heard.refusals
    assert "20260902-14:30:00" in said[0]


def test_the_unset_indicator_states_nothing():
    client, heard = _client()
    # The number an integer nobody set carries is not a statement.
    left_alone = ibkr_dx.OrderCancel()
    assert left_alone.manualOrderIndicator == ibkr_dx.UNSET_INTEGER
    client.cancelOrder(1, left_alone)
    assert "manual_order_indicator: 2147483647" in _withdrew(client)[0]
    assert not _said(client, heard)


def test_the_global_withdrawal_carries_the_same_and_no_time():
    client, heard = _client()
    client._test_finish_order_replay()
    client._test_set_instrument_count(1)
    withdrawal = ibkr_dx.OrderCancel()
    withdrawal.manualOrderCancelTime = "20260902-14:30:00"
    withdrawal.extOperator = "someone"
    withdrawal.manualOrderIndicator = 0
    client.reqGlobalCancel(withdrawal)
    sent = _withdrew(client)
    assert sent and all('ext_operator: "someone"' in cmd for cmd in sent), sent
    assert all("manual_order_indicator: 0 " in cmd for cmd in sent), sent
    # The reference client writes no time on a withdrawal of everything, so a
    # gateway never reads one; nothing is said of it.
    assert not _said(client, heard)
    assert not [said for said in heard.refusals if 321 in said], heard.refusals


def _codes(client, heard):
    client._test_dispatch_once()
    return [said[2] for said in heard.refusals]


def test_a_time_a_gateway_cannot_read_withdraws_nothing():
    client, heard = _client()
    for unread in ("garbage", "2026-09-24 14:30:00", "20260924 14:30", "   "):
        withdrawal = ibkr_dx.OrderCancel()
        withdrawal.manualOrderCancelTime = unread
        client.cancelOrder(1, withdrawal)
    assert not _withdrew(client), "the order keeps working, as through a gateway"
    codes = _codes(client, heard)
    assert codes == [10301] * 4, heard.refusals
    assert heard.refusals[0][3].startswith("Manual Order Cancel Time: The date, time, or time-zone entered is invalid.")


def test_the_forms_a_gateway_reads_are_withdrawn():
    client, heard = _client()
    for read in ("20260924-14:30:00", "20260924 14:30:00", "20260924 14:30:00 US/Eastern", "14:30:00"):
        client.cancelOrder(1, read)
    assert len(_withdrew(client)) == 4


def test_each_is_written_as_its_text_and_the_indicator_read_as_a_number():
    client, heard = _client()
    withdrawal = ibkr_dx.OrderCancel()
    withdrawal.extOperator = 42
    withdrawal.manualOrderIndicator = 1
    client.cancelOrder(1, withdrawal)
    sent = _withdrew(client)
    assert 'ext_operator: "42"' in sent[0], sent

    withdrawal.manualOrderIndicator = "yes"
    client.cancelOrder(2, withdrawal)
    assert not _withdrew(client)
    assert _codes(client, heard) == [320], heard.refusals
    assert "Unable to parse field: 'Manual Order Indicator' for input string: 'yes'" in heard.refusals[0][3]


def test_an_operator_that_would_split_the_cancel_withdraws_nothing():
    client, heard = _client()
    client._test_finish_order_replay()
    client._test_set_instrument_count(1)
    withdrawal = ibkr_dx.OrderCancel()
    withdrawal.extOperator = "someone\x0111=7"
    client.cancelOrder(1, withdrawal)
    client.reqGlobalCancel(withdrawal)
    assert not _withdrew(client)
    assert _codes(client, heard) == [321, 321], heard.refusals
