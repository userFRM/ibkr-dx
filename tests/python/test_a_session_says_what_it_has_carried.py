"""A session says what it has sent and received on the venue's connections.

What a TWS client reads as its connection's statistics: bytes and messages,
each way, since the session opened. Nought where nothing has crossed.
"""

import ibkr_dx


KEYS = {"bytes_sent", "bytes_received", "messages_sent", "messages_received"}


def test_no_session_has_carried_nothing():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    assert c.traffic() == dict.fromkeys(KEYS, 0)


def test_a_session_states_each_count():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU1")
    counts = c.traffic()
    assert set(counts) == KEYS
    assert all(isinstance(v, int) and v >= 0 for v in counts.values()), counts
