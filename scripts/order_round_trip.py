"""An order's life on the paper account through `EClient`: placed, changed, withdrawn.

The paper suite's order phases name US shares, so they wait for the New York
session and skip outside it — which is most of the hours anyone works in. This
asks the same question of a contract that trades nearly around the clock, so
the order path can be checked at three in the morning as well as at noon.

A limit far below the market, so it rests. What is checked is the round trip:
that the venue takes each step, that this client's record of the order — the
order it echoes back on `openOrder` — moves with it, and that nothing is left
working. Nothing is meant to be bought; one that trades is sold back, and the
round trip is not whole.

    IB_USERNAME=… IB_PASSWORD=… python scripts/order_round_trip.py
"""

import os
import sys
import time

from ibkr_dx import Contract, Order
from sdk_sweep import Heard, connect

#: The smaller S&P future, which trades nearly around the clock: the front
#: month the venue lists, or the month named by `IBKR_DX_RT_EXPIRY`. A month
#: that has expired is refused by name, which is a clear failure rather than a
#: quiet one.
#:
#: A share can be named instead, which is worth doing during a session or in the
#: hours either side of one — the order path is the same and the venues are not:
#:
#:     IBKR_DX_RT_SYMBOL=SPY IBKR_DX_RT_SEC_TYPE=STK IBKR_DX_RT_EXCHANGE=SMART \
#:     IBKR_DX_RT_PRICE=400 IBKR_DX_RT_OUTSIDE_RTH=1 python scripts/order_round_trip.py
SYMBOL = os.environ.get("IBKR_DX_RT_SYMBOL", "MES")
SEC_TYPE = os.environ.get("IBKR_DX_RT_SEC_TYPE", "FUT")
EXCHANGE = os.environ.get("IBKR_DX_RT_EXCHANGE", "CME")
EXPIRY = os.environ.get("IBKR_DX_RT_EXPIRY", "")

#: Far under the market, so it rests, and on the contract's own increment.
#: Stated rather than read off a quote: this check is about the order path, and
#: asking for a quote makes it need an entitlement it does not otherwise use.
#: No fixed price is below every market, so one that trades anyway is sold
#: back and the round trip reported as not whole.
RESTS_AT = float(os.environ.get("IBKR_DX_RT_PRICE", "6000"))

#: Whether the order may work outside the regular session. A share resting
#: before the bell needs this said; a future does not have the distinction.
OUTSIDE_RTH = os.environ.get("IBKR_DX_RT_OUTSIDE_RTH", "") not in ("", "0")

#: How long the venue is given to answer each step.
ANSWER = 20


class Watched(Heard):
    """What the sweep hears, every order's status, and the orders echoed back."""

    def order_status(self, order_id, status, *rest):
        self._got(("status", order_id), status)

    def open_order(self, order_id, contract, order, state):
        super().open_order(order_id, contract, order, state)
        self._got(("echo", order_id), order.lmt_price)


def main():
    heard = Watched()
    client = connect(heard)
    if client is None:
        return 2

    def settle(what, order_id, wanted):
        """Wait for the order to reach one of `wanted`, and say what happened."""
        deadline = time.time() + ANSWER
        while time.time() < deadline:
            stated = heard.answered(("status", order_id))[0]
            if stated and stated[-1] in wanted:
                print(f"{what}: {stated[-1]}", flush=True)
                return stated[-1]
            time.sleep(0.2)
        stated = heard.answered(("status", order_id))[0]
        print(f"{what}: nothing within {ANSWER}s, still {stated[-1] if stated else 'unstated'} "
              f"{heard.answered(order_id)[1]}", flush=True)
        return None

    described = Contract()
    described.symbol, described.sec_type, described.exchange = SYMBOL, SEC_TYPE, EXCHANGE
    described.currency = "USD"
    if SEC_TYPE == "FUT":
        described.last_trade_date_or_contract_month = EXPIRY
    client.req_contract_details(1, described)
    deadline = time.time() + ANSWER
    while time.time() < deadline and not heard.answered(("end", 1))[0]:
        time.sleep(0.1)
    found, said = heard.answered(1)
    # Every month the venue lists, where none was named: the front one is the
    # one that expires first.
    if SEC_TYPE == "FUT" and not EXPIRY and found:
        found = [min(found, key=lambda d: d.contract.last_trade_date_or_contract_month)]
    if len(found) != 1:
        print(f"the venue named {len(found)} contracts for {SYMBOL} {SEC_TYPE} {EXPIRY}: {said}")
        client.disconnect()
        return 1
    contract = found[0].contract
    print(f"contract: conId={contract.con_id} {contract.local_symbol}", flush=True)

    oid = heard.next_id
    order = Order()
    order.action, order.order_type, order.total_quantity = "BUY", "LMT", 1
    order.lmt_price, order.tif = RESTS_AT, "GTC"
    order.outside_rth = OUTSIDE_RTH
    client.place_order(oid, contract, order)

    def sold_back():
        """A limit meant to rest that traded leaves a holding, which is sold
        back. Whether it had to be."""
        if "Filled" not in heard.answered(("status", oid))[0]:
            return False
        back = Order()
        back.action, back.order_type, back.total_quantity = "SELL", "MKT", 1
        back.outside_rth = OUTSIDE_RTH
        client.place_order(oid + 1, contract, back)
        settle("sold back", oid + 1, {"Filled"})
        return True

    placed = settle("placed   ", oid, {"Submitted", "PreSubmitted"})
    if placed is None:
        sold_back()
        client.disconnect()
        return 1

    # The same order at a new price, which is what a replace is.
    moved_to = RESTS_AT - 1.0
    order.lmt_price = moved_to
    client.place_order(oid, contract, order)
    time.sleep(5)
    echoed = heard.answered(("echo", oid))[0]
    held = echoed[-1] if echoed else None
    print(f"changed  : this client holds {held}, asked for {moved_to}", flush=True)

    client.cancel_order(oid, "")
    withdrawn = settle("withdrawn", oid, {"Cancelled", "ApiCancelled"})

    # Nothing of it is left working at the venue, which is the half a status
    # does not answer.
    with heard.lock:
        heard.got.pop("open_order", None)
        heard.got.pop("open_order_end", None)
    client.req_open_orders()
    deadline = time.time() + ANSWER
    while time.time() < deadline and not heard.answered("open_order_end")[0]:
        time.sleep(0.1)
    left = heard.answered("open_order")[0]
    print(f"left working: {left}", flush=True)
    traded = sold_back()
    client.disconnect()

    whole = bool(placed) and bool(withdrawn) and held == moved_to and oid not in left and not traded
    print("the round trip is whole" if whole else "the round trip is not whole", flush=True)
    return 0 if whole else 1


if __name__ == "__main__":
    sys.exit(main())
