"""A request returns once the engine has it, and what it has not finished is counted.

A TWS call returns once its message is written. Here the message is a command
handed to the engine's loop, and the channel it goes down is unbounded: a call
never waits for the loop to make room, however far behind the loop is.
`backlog()` counts every command handed over that the engine has not finished
with, so a caller can bound what it has handed over without being blocked by it.
"""

import threading

import ibkr_dx


def test_ten_thousand_requests_return_and_are_counted():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("T")
    # Nothing takes from the channel behind a test session.
    admitting = threading.Thread(
        target=lambda: [c.cancelHistoricalData(req_id) for req_id in range(1, 10_001)],
        daemon=True,
    )
    admitting.start()
    admitting.join(10)
    assert not admitting.is_alive(), "a request waited for an engine that was not taking"
    assert c.backlog() == 10_000


def test_nothing_is_counted_before_a_session():
    assert ibkr_dx.EClient(ibkr_dx.EWrapper()).backlog() == 0
