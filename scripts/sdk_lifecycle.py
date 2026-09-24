"""An order's whole life through `EClient`, against the venue.

`sdk_sweep.py` asks the venue for things. This tells it to do one: place an
order, change it, and withdraw it — the three a trading program does, in the
order it does them, through the calls a program written against the reference
client makes.

The order is a buy far under the market on a paper account, so it rests and
nothing trades, and it is good for the day — so it is asked for only inside
SPY's own liquid hours, which the venue states: outside them a day order is
cancelled by the venue, which says nothing about this client. Shut, the script
prints the hours and returns. The order is withdrawn before this returns.

    IB_USERNAME=… IB_PASSWORD=… python scripts/sdk_lifecycle.py
"""

import sys
import time

from ibkr_dx import Order
from sdk_sweep import Heard, connect, described, inside_the_session


def status_of(heard, order_id):
    """The last status the venue stated for an order, or nothing."""
    with heard.lock:
        stated = [s for s in heard.got.get(("status", order_id), [])]
    return stated[-1] if stated else ""


class Watched(Heard):
    """What the sweep hears, and every order's status."""

    def order_status(self, order_id, status, *rest):
        self._got(("status", order_id), status)


def main():
    heard = Watched()
    client = connect(heard)
    if client is None:
        return 2
    client.req_contract_details(1, described("SPY", "STK", "SMART"))
    deadline = time.time() + 20
    while time.time() < deadline and not heard.answered(("end", 1))[0]:
        time.sleep(0.1)
    found, _ = heard.answered(1)
    if not found:
        print("the venue named no SPY; reference data answers at any hour")
        client.disconnect()
        return 1
    spy = found[0].contract
    if not inside_the_session(found[0]):
        print(f"SPY's session is shut by the venue's own hours ({found[0].liquid_hours}); "
              "a day order would be cancelled by the venue, so nothing is placed")
        client.disconnect()
        return 0

    oid = heard.next_id
    order = Order()
    order.action, order.order_type, order.total_quantity, order.lmt_price = "BUY", "LMT", 10, 100.0

    def settle(seconds=3.0):
        time.sleep(seconds)
        with heard.lock:
            said = list(heard.said.get(oid, []))
            heard.said[oid] = []
        return said

    client.place_order(oid, spy, order)
    try:
        print(f"placed     {status_of(heard, oid):12} {settle()}")
        order.lmt_price = 101.0
        client.place_order(oid, spy, order)
        said = settle()
        with heard.lock:
            echoed = [s for s in heard.got.get("open_order", []) if s == oid]
        print(f"changed    {status_of(heard, oid):12} echoed {len(echoed)} times {said}")
        client.cancel_order(oid, "")
        print(f"withdrawn  {status_of(heard, oid):12} {settle()}")
        return 0 if status_of(heard, oid) in ("Cancelled", "ApiCancelled") else 1
    finally:
        # Whatever went wrong, nothing is left working at the venue.
        if status_of(heard, oid) not in ("Cancelled", "ApiCancelled", "Filled"):
            client.cancel_order(oid, "")
            time.sleep(3)
        client.disconnect()


if __name__ == "__main__":
    sys.exit(main())
