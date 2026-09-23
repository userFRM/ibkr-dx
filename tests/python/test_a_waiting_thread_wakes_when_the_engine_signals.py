"""A thread that waits on the engine wakes when it signals.

A program driving its own loop reads the session with `poll`, and before this
had nothing to wait on between polls but a sleep of its own choosing: too long
and a fill waits for it, too short and a core spins. `wait_for_data` is the
wait `run` already made, handed to the caller.
"""

import threading
import time

import pytest

import ibkr_dx


def connected():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU0000000")
    return c


def test_a_wait_nothing_answers_runs_out():
    assert connected().wait_for_data(0.02) is False


def test_a_waiting_thread_wakes_when_the_engine_signals():
    c = connected()
    woke = []
    waiter = threading.Thread(target=lambda: woke.append(c.wait_for_data(30.0)))
    began = time.monotonic()
    waiter.start()
    time.sleep(0.05)
    # What the engine does when a lost connection comes back, which signals as
    # the end of each of its passes does.
    c._test_set_connection_restored()
    waiter.join(timeout=30)
    assert woke == [True], "the signal was not seen"
    assert time.monotonic() - began < 10, "the wait ended only when it ran out"


def test_a_wait_with_no_session_is_refused():
    with pytest.raises(RuntimeError):
        ibkr_dx.EClient(ibkr_dx.EWrapper()).wait_for_data(0.01)


def test_a_wait_that_is_not_a_length_of_time_is_refused():
    with pytest.raises(ValueError):
        connected().wait_for_data(-1.0)
