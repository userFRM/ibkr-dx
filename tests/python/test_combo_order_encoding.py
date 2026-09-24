"""The two legs of a SPY vertical call spread, quoted one at a time.

What this actually does, which is not what it used to say: it names each leg
and subscribes to its quote, so a leg the venue cannot resolve is caught. It
builds no BAG, no ComboLeg and no order, and the note claiming ComboLeg
population was unimplemented is out of date besides — the order path reads legs
off the contract and refuses one it cannot read.

Run: pytest tests/python/test_combo_order_encoding.py -v -s
"""

import os, threading, time
import pytest

from conftest import declined, next_option_expiry
from ibkr_dx import EWrapper, EClient, Contract

pytestmark = pytest.mark.skipif(
    not (os.environ.get("IB_USERNAME") and os.environ.get("IB_PASSWORD")),
    reason="IB_USERNAME and IB_PASSWORD not set",
)

SPY_CON_ID = 756733


class SpreadWrapper(EWrapper):
    def __init__(self):
        super().__init__()
        self.connected = threading.Event()
        self.lock = threading.Lock()
        self.ticks = {}  # req_id -> {bid, ask, last}
        self.got_tick = {}  # req_id -> Event
        self.underlying_price = 0.0
        self.got_underlying = threading.Event()
        self.errors = []

    def next_valid_id(self, order_id):
        self.next_order_id = order_id
        self.connected.set()

    def managed_accounts(self, accounts_list):
        pass

    def connect_ack(self):
        pass

    def tick_price(self, req_id, tick_type, price, attrib):
        if price > 0:
            with self.lock:
                if req_id == 1000:
                    if tick_type in (1, 2, 4):
                        self.underlying_price = price
                        self.got_underlying.set()
                else:
                    self.ticks.setdefault(req_id, {})
                    if tick_type == 1:
                        self.ticks[req_id]["bid"] = price
                    elif tick_type == 2:
                        self.ticks[req_id]["ask"] = price
                    elif tick_type == 4:
                        self.ticks[req_id]["last"] = price
                    ev = self.got_tick.get(req_id)
                    if ev:
                        ev.set()

    def tick_size(self, req_id, tick_type, size):
        pass

    def error(self, req_id, error_time, error_code, error_string, advanced_order_reject_json=""):
        self.errors.append((req_id, error_code, error_string))
        if error_code not in (2104, 2106, 2119, 2158, 460, 202):
            print(f"  [error] reqId={req_id} code={error_code}: {error_string}")


class TestComboSpread:
    """Option spread leg market data."""

    @pytest.fixture(autouse=True)
    def setup_connection(self):
        self.wrapper = SpreadWrapper()
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

    def _said(self, *req_ids):
        """Anything said under one of these requests, or a data connection
        dropping."""
        return [e for e in self.wrapper.errors if e[0] in req_ids] or declined(self.wrapper.errors, *req_ids)

    def test_vertical_spread_legs(self):
        """Get market data for two option legs of a bull call spread.

        The legs are named by the venue before they are quoted, so a quote is
        never asked of a contract nobody listed. After that, silence fails: a
        listed option carries a price at any hour, or the venue says why not.
        """
        spy = Contract()
        spy.con_id = SPY_CON_ID
        spy.symbol = "SPY"
        spy.sec_type = "STK"
        spy.exchange = "SMART"
        spy.currency = "USD"

        self.client.req_mkt_data(1000, spy, "", False)
        got = self.wrapper.got_underlying.wait(timeout=30)
        self.client.cancel_mkt_data(1000)
        # Outside the session a stock still carries a bid, an ask and the close.
        if not got and declined(self.wrapper.errors, 1000):
            pytest.skip(f"the venue declined the quote or its connection dropped: {declined(self.wrapper.errors, 1000)[0]}")
        assert got, f"a quote on SPY delivered no price: {self._said(1000)}"

        price = self.wrapper.underlying_price
        chains = self.client.option_chains("SPY", "", "STK", SPY_CON_ID)
        expiries = sorted({e for c in chains for e in c.expirations if e >= next_option_expiry()})
        assert expiries, "the venue listed no expiries for SPY"
        expiry = expiries[0]
        strikes = sorted({k for c in chains if expiry in c.expirations for k in c.strikes})
        atm_strike = min(strikes, key=lambda k: abs(k - price))
        otm_strike = min((k for k in strikes if k >= atm_strike + 3), default=None)
        assert otm_strike is not None, f"no strike listed three above {atm_strike} on {expiry}"
        print(f"  SPY: {price}, buy leg strike: {atm_strike}, sell leg strike: {otm_strike}")

        def named(strike):
            leg = Contract()
            leg.symbol = "SPY"
            leg.sec_type = "OPT"
            leg.exchange = "SMART"
            leg.currency = "USD"
            leg.right = "C"
            leg.strike = float(strike)
            leg.last_trade_date_or_contract_month = expiry
            leg.multiplier = "100"
            found = self.client.contract_details(leg)
            assert found, f"the venue named no SPY {strike} call on {expiry}"
            return found[0].contract

        buy_leg, sell_leg = named(atm_strike), named(otm_strike)

        # Subscribe to both legs
        self.wrapper.got_tick[3001] = threading.Event()
        self.wrapper.got_tick[3002] = threading.Event()

        self.client.req_mkt_data(3001, buy_leg, "", False)
        self.client.req_mkt_data(3002, sell_leg, "", False)

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and not self._said(3001, 3002) and not (
            self.wrapper.got_tick[3001].is_set() and self.wrapper.got_tick[3002].is_set()
        ):
            time.sleep(0.1)
        got_buy = self.wrapper.got_tick[3001].is_set()
        got_sell = self.wrapper.got_tick[3002].is_set()

        # Wait for bid/ask to populate
        time.sleep(5)

        self.client.cancel_mkt_data(3001)
        self.client.cancel_mkt_data(3002)

        if not (got_buy and got_sell) and declined(self.wrapper.errors, 3001, 3002):
            pytest.skip(f"the venue declined a leg's quote or its connection dropped: {declined(self.wrapper.errors, 3001, 3002)[0]}")
        assert got_buy and got_sell, (
            f"a listed option gave no price within 30s: buy {got_buy}, sell {got_sell}: {self._said(3001, 3002)}"
        )

        buy_data = self.wrapper.ticks.get(3001, {})
        sell_data = self.wrapper.ticks.get(3002, {})

        print(f"  Buy leg ({atm_strike}C): {buy_data}")
        print(f"  Sell leg ({otm_strike}C): {sell_data}")

        # Verify buy leg is more expensive than sell leg (bull call spread)
        if "bid" in buy_data and "bid" in sell_data:
            spread_bid = buy_data["bid"] - sell_data.get("ask", sell_data["bid"])
            spread_ask = buy_data.get("ask", buy_data["bid"]) - sell_data["bid"]
            print(f"  Spread value: {spread_bid:.2f} - {spread_ask:.2f}")
            assert buy_data["bid"] > sell_data["bid"], \
                f"ATM call should be more expensive than OTM call"
