"""A counter seeded from the shared id numbers requests the session carries.

A program numbering orders and requests out of one counter, seeded past every
order id, has no request it can number on an account the venue has given an
order id wider than a request. The shared id is one past the widest id a
request can carry.
"""

import ibkr_dx


def test_the_shared_id_is_one_a_request_can_carry_past_a_wide_order_id():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU0000000")
    c._test_push_venue_order(700, "AAPL", "BUY", 1.0, 100.0)
    c._test_push_venue_order(5_000_000_000, "AAPL", "BUY", 1.0, 100.0)

    assert c.next_shared_id() == 701
    assert c.next_shared_id() == 701, "a read, not a counter"
    assert c.next_order_id() > 2**32, "orders still count past the wide one"
