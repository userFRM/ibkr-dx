"""Where the request ids this client keeps for itself begin.

A program that routes answers by request id needs to know which ids are never
its own: an error carrying one answers no request it made.
"""

import ibkr_dx


def test_the_first_reserved_request_id_is_published():
    assert ibkr_dx.FIRST_RESERVED_REQUEST_ID == 0xC000_0000


def test_no_32_bit_request_id_reaches_it():
    assert 2**31 - 1 < ibkr_dx.FIRST_RESERVED_REQUEST_ID
