"""A second cancel of one order leaves it cancelled once and listed nowhere.

The test makes its own order: one share of SPY, far under the market, good till
cancelled and allowed outside regular hours, which the venue takes at any hour.
Once it is working it is cancelled twice, back to back. Both cancels go to the
venue, the second naming itself apart from the first. The order ends cancelled
once. The second cancel is answered by the venue's reject — under 10147 where
the venue no longer holds the order, 10148 for any other reason — or by nothing
further, and which of those the venue does is what a run records. Either way
the order is gone from the open orders afterwards.

Run: pytest tests/python/test_a_second_cancel_leaves_one_order_cancelled_once.py -v
"""

import os
import threading
import time

import pytest
from conftest import wait_for
from ibkr_dx import Contract, EClient, EWrapper, Order

pytestmark = pytest.mark.skipif(
    not (os.environ.get("IB_USERNAME") and os.environ.get("IB_PASSWORD")),
    reason="IB_USERNAME and IB_PASSWORD not set",
)

#: What the venue's reject of a cancel is reported under: the order is no
#: longer held there, or the cancel failed for another reason.
CANCEL_REJECTED = (10147, 10148)
#: This client's own answer to a cancel of an order it has already seen end,
#: where the first cancel's end arrived before the second was sent.
NOT_CANCELLABLE = 161


class CollectorWrapper(EWrapper):
    def __init__(self):
        super().__init__()
        self.connected = threading.Event()
        self.next_id = 0
        self.open_orders_batch = []
        self.got_open_order_end = threading.Event()
        self.statuses = []
        self.errors = []

    def next_valid_id(self, order_id):
        self.next_id = order_id
        self.connected.set()

    def managed_accounts(self, accounts_list):
        pass

    def connect_ack(self):
        pass

    def open_order(self, order_id, contract, order, order_state):
        self.open_orders_batch.append((order_id, contract.symbol, order_state.status))

    def open_order_end(self):
        self.got_open_order_end.set()

    def order_status(self, order_id, status, filled, remaining, avg_fill_price,
                     perm_id, parent_id, last_fill_price, client_id, why_held, mkt_cap_price):
        self.statuses.append((order_id, status))

    def error(self, req_id, error_time, error_code, error_string, advanced_order_reject_json=""):
        self.errors.append((req_id, error_code, error_string))


class TestASecondCancel:
    @pytest.fixture(autouse=True)
    def setup_connection(self):
        self.wrapper = CollectorWrapper()
        self.client = EClient(self.wrapper)
        self.client.connect(
            username=os.environ["IB_USERNAME"],
            password=os.environ["IB_PASSWORD"],
            host=os.environ.get("IB_HOST", "cdc1.ibllc.com"),
            paper=True,
        )
        self.thread = threading.Thread(target=self.client.run, daemon=True)
        self.thread.start()
        assert self.wrapper.connected.wait(timeout=15), "Connection failed"
        yield
        self.client.disconnect()
        self.thread.join(timeout=5)

    def _open_ids(self):
        self.wrapper.open_orders_batch = []
        self.wrapper.got_open_order_end.clear()
        self.client.req_open_orders()
        assert self.wrapper.got_open_order_end.wait(timeout=15), "open_order_end never fired"
        return {o[0] for o in self.wrapper.open_orders_batch}

    def test_a_second_cancel_leaves_the_order_cancelled_once(self):
        spy = Contract()
        spy.con_id, spy.symbol, spy.sec_type, spy.exchange, spy.currency = 756733, "SPY", "STK", "SMART", "USD"
        order = Order()
        order.action, order.order_type, order.total_quantity, order.lmt_price = "BUY", "LMT", 1, 1.0
        order.tif, order.outside_rth = "GTC", True
        oid = self.wrapper.next_id

        self.client.place_order(oid, spy, order)
        try:
            working = wait_for(lambda: any(
                o == oid and s in ("Submitted", "PreSubmitted") for o, s in self.wrapper.statuses
            ), 20)
            assert working, (
                f"the order was not working within 20s: "
                f"{[s for o, s in self.wrapper.statuses if o == oid]} "
                f"{[e for e in self.wrapper.errors if e[0] == oid]}"
            )

            said_before = len(self.wrapper.errors)
            self.client.cancel_order(oid, "")
            self.client.cancel_order(oid, "")
            assert wait_for(lambda: any(o == oid and s == "Cancelled" for o, s in self.wrapper.statuses), 20), (
                f"the order was cancelled twice and never ended: "
                f"{[s for o, s in self.wrapper.statuses if o == oid]}"
            )
            time.sleep(5)  # the second cancel's answer, where there is one

            ended = [s for o, s in self.wrapper.statuses if o == oid and s == "Cancelled"]
            assert len(ended) == 1, f"the order ended {len(ended)} times"
            # What was said under the order after the cancels, warnings aside.
            answered = [e for e in self.wrapper.errors[said_before:]
                        if e[0] == oid and not 2100 <= e[1] < 2200]
            print(f"  the second cancel was answered by: {answered or 'nothing'}")
            assert all(code in CANCEL_REJECTED + (NOT_CANCELLABLE,) for _, code, _ in answered), answered
            assert oid not in self._open_ids(), "a cancelled order is still listed as open"
        finally:
            # A GTC order left working stays on the account; withdrawn here
            # whatever stopped the test.
            if not any(o == oid and s == "Cancelled" for o, s in self.wrapper.statuses):
                self.client.cancel_order(oid, "")
