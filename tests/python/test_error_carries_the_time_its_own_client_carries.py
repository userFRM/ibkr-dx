"""`error` has the parameter the reference client's wrapper has, where it has it.

That wrapper's signature is
``error(reqId, errorTime, errorCode, errorString, advancedOrderRejectJson="")``
and every one of its call sites passes five. The arity does not vary: only the
value does, and its decoder passes zero for a session speaking a protocol older
than the one that added the field. This client says it speaks one that has it,
and a gateway speaking that stamps every error it sends.

Given four, a handler written that way bound `errorTime` to the code and
`errorCode` to the message, silently — which is why a handler answering 1100 by
reconnecting never fired.
"""

import pytest

from ibkr_dx import EClient, EWrapper


class Records(EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []

    def error(self, reqId, errorTime, errorCode, errorString, advancedOrderRejectJson=""):
        self.errors.append((reqId, errorTime, errorCode, errorString))


def _not_connected(client):
    client.req_current_time()


def _venue_stated(client):
    client._test_connect("T")
    client._test_push_historical_error(7, 162, "no data")
    client._test_dispatch_once()


@pytest.mark.parametrize(
    "happen, said",
    [
        # That client's own `NOT_CONNECTED` goes out as
        # ``error(NO_VALID_ID, currentTimeMillis(), code, message)``.
        (_not_connected, (-1, 504, "Not connected")),
        # A gateway stamps what the venue stated as well.
        (_venue_stated, (7, 162, "no data")),
    ],
)
def test_every_error_carries_a_clock_reading(happen, said):
    wrapper = Records()
    client = EClient(wrapper)
    happen(client)

    (req_id, error_time, code, message), = wrapper.errors
    assert (req_id, code, message) == said, "the code is in the slot the code goes in"
    assert error_time > 1_700_000_000_000, (
        f"a clock reading in milliseconds, got {error_time}"
    )


def test_the_base_wrapper_takes_the_five_the_other_client_takes():
    EWrapper().error(-1, 0, 504, "test", "")
