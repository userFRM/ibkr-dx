"""The venue's option model, and the calls answered from it (AAPL).

Subscribe to an at-the-money call and put with the model's ticks, and read
`tickOptionComputation` for the implied volatility and the greeks. Then ask the
two calculations of the model the venue published, withdraw two that are still
waiting on one, and exercise and lapse an option the account does not hold.

The options are looked up before anything is asked of them: a contract the venue
does not name is a failure, not a closed market, because reference data is
answered at any hour. After that the only reasons to skip are the ones the venue
or this client states — a refusal under the request, quoted, or the notice that
the market-data connection dropped — or the venue's own liquid hours for the
option. Silence fails.

Run: pytest tests/python/test_option_greeks_stream.py -v -s
"""

import os, threading, time
import pytest

from conftest import FEED_DOWN, declined, inside_the_session, liquid_hours, next_option_expiry, wait_for
from ibkr_dx import EWrapper, EClient, Contract

pytestmark = pytest.mark.skipif(
    not (os.environ.get("IB_USERNAME") and os.environ.get("IB_PASSWORD")),
    reason="IB_USERNAME and IB_PASSWORD not set",
)

AAPL_CON_ID = 265598
#: The model's own computation, as opposed to the bid's, the ask's and the last's.
MODEL = 13
#: What a calculation asked of the model is answered under.
ASKED = 53


def make_aapl():
    c = Contract()
    c.con_id = AAPL_CON_ID
    c.symbol = "AAPL"
    c.sec_type = "STK"
    c.exchange = "SMART"
    c.currency = "USD"
    return c


def described_option(right, strike, expiry):
    c = Contract()
    c.symbol = "AAPL"
    c.sec_type = "OPT"
    c.exchange = "SMART"
    c.currency = "USD"
    c.right = right
    c.strike = strike
    c.last_trade_date_or_contract_month = expiry
    c.multiplier = "100"
    return c


class OptionsWrapper(EWrapper):
    def __init__(self):
        super().__init__()
        self.connected = threading.Event()
        self.lock = threading.Lock()
        self.next_id = 0
        self.underlying_price = 0.0
        self.got_underlying = threading.Event()
        self.greeks = {}  # req_id -> list of computations
        self.errors = []  # (req_id, code, message)

    def next_valid_id(self, order_id):
        self.next_id = order_id
        self.connected.set()

    def managed_accounts(self, accounts_list):
        pass

    def connect_ack(self):
        pass

    def tick_price(self, req_id, tick_type, price, attrib):
        if price > 0 and req_id == 1000 and tick_type in (1, 2, 4, 9):
            with self.lock:
                self.underlying_price = price
            self.got_underlying.set()

    def tick_size(self, req_id, tick_type, size):
        pass

    def tick_option_computation(self, req_id, tick_type, tick_attrib,
                                implied_vol, delta, opt_price, pv_dividend,
                                gamma, vega, theta, und_price):
        with self.lock:
            self.greeks.setdefault(req_id, []).append({
                "tick_type": tick_type, "implied_vol": implied_vol, "delta": delta,
                "gamma": gamma, "vega": vega, "theta": theta,
                "und_price": und_price, "opt_price": opt_price,
            })

    def error(self, req_id, error_time, error_code, error_string, advanced_order_reject_json=""):
        with self.lock:
            self.errors.append((req_id, error_code, error_string))

    def said_under(self, req_id):
        with self.lock:
            return [(code, msg) for r, code, msg in self.errors if r == req_id]

    def feed_down(self):
        with self.lock:
            return [(code, msg) for r, code, msg in self.errors if r == -1 and code in FEED_DOWN]

    def declined(self, req_id):
        """The venue declining the request, or a data connection dropping."""
        with self.lock:
            return declined(self.errors, req_id)

    def computations(self, req_id, tick_type=None):
        with self.lock:
            return [g for g in self.greeks.get(req_id, [])
                    if tick_type is None or g["tick_type"] == tick_type]


def _stated(value):
    """A figure the venue stated: not its marker for none, not the unset maximum."""
    return value not in (-1.0, -2.0) and abs(value) < 1e300


class TestOptionsGreeks:
    """The venue's option model, and what is asked of it."""

    @pytest.fixture(autouse=True)
    def setup_connection(self):
        self.wrapper = OptionsWrapper()
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

    def _underlying_price(self):
        """The underlying's price. Outside the session a stock still carries a
        bid, an ask and the close, so silence here fails whatever the hour."""
        self.client.req_mkt_data(1000, make_aapl(), "", False)
        got = self.wrapper.got_underlying.wait(timeout=30)
        self.client.cancel_mkt_data(1000)
        if not got and self.wrapper.feed_down():
            pytest.skip(f"the market-data connection dropped: {self.wrapper.feed_down()[0]}")
        assert got, f"a quote on AAPL delivered no price: {self.wrapper.said_under(1000)}"
        return self.wrapper.underlying_price

    def _named(self, contract):
        """The option as the venue names it; one it does not name is a failure."""
        found = self.client.contract_details(contract)
        assert found, f"the venue named no {contract.right} at {contract.strike} {contract.last_trade_date_or_contract_month}"
        return found[0].contract

    def _options(self):
        price = self._underlying_price()
        # The first expiry the venue lists from the coming week's on, and the
        # strike nearest the price among those it lists for it.
        chains = self.client.option_chains("AAPL", "", "STK", AAPL_CON_ID)
        expiries = sorted({e for chain in chains for e in chain.expirations if e >= next_option_expiry()})
        assert expiries, "the venue listed no expiries for AAPL"
        expiry = expiries[0]
        strikes = {s for chain in chains if expiry in chain.expirations for s in chain.strikes}
        strike = min(strikes, key=lambda s: abs(s - price))
        call = self._named(described_option("C", strike, expiry))
        put = self._named(described_option("P", strike, expiry))
        return call, put

    def _model(self, req_id, option, timeout=30.0):
        """The venue's model for a subscribed option, or a skip on a stated
        reason, or a failure on silence inside the option's hours."""
        wait_for(lambda: self.wrapper.computations(req_id, MODEL)
                 or self.wrapper.said_under(req_id) or self.wrapper.feed_down(), timeout)
        model = [g for g in self.wrapper.computations(req_id, MODEL)
                 if _stated(g["opt_price"]) and _stated(g["und_price"])]
        if model:
            return model[-1]
        if self.wrapper.declined(req_id):
            pytest.skip(f"the venue declined the option's model or quote: {self.wrapper.declined(req_id)[0]}")
        assert not self.wrapper.said_under(req_id), self.wrapper.said_under(req_id)
        if not inside_the_session(self.client, option):
            pytest.skip(
                "the venue's own liquid hours say the option's session is shut "
                f"({liquid_hours(self.client, option)})"
            )
        pytest.fail(f"the option is trading and its model said nothing within {timeout:.0f}s")

    def test_option_greek_streaming(self):
        """The model's greeks, on a call and on a put."""
        call, put = self._options()
        self.client.req_mkt_data(2001, call, "100,101,104,106", False)
        self.client.req_mkt_data(2002, put, "100,101,104,106", False)
        g = self._model(2001, call)
        pg = self._model(2002, put)
        self.client.cancel_mkt_data(2001)
        self.client.cancel_mkt_data(2002)

        if _stated(g["delta"]):
            assert 0 < g["delta"] < 1, f"Call delta {g['delta']} out of range"
        if _stated(g["implied_vol"]):
            assert g["implied_vol"] > 0, f"IV {g['implied_vol']} should be positive"
        if _stated(pg["delta"]):
            assert -1 < pg["delta"] < 0, f"Put delta {pg['delta']} out of range"

    def test_the_calculations_are_answered_from_the_venues_model(self):
        """A volatility from the model's price, and the price back from that
        volatility, to the cent — which is what solving against the venue's
        model has been measured to reproduce."""
        call, put = self._options()
        self.client.req_mkt_data(2001, call, "", False)
        g = self._model(2001, call)

        self.client.calculate_implied_volatility(2101, call, g["opt_price"], g["und_price"])
        assert wait_for(lambda: self.wrapper.computations(2101, ASKED), 10), (
            f"no volatility within 10s: {self.wrapper.said_under(2101)}"
        )
        iv = self.wrapper.computations(2101, ASKED)[-1]["implied_vol"]
        assert iv > 0, iv

        self.client.calculate_option_price(2102, call, iv, g["und_price"])
        assert wait_for(lambda: self.wrapper.computations(2102, ASKED), 10), (
            f"no price within 10s: {self.wrapper.said_under(2102)}"
        )
        priced = self.wrapper.computations(2102, ASKED)[-1]["opt_price"]
        # To the cent: two prices a hair apart can round to different cents.
        assert abs(priced - g["opt_price"]) < 0.005, (priced, g["opt_price"])
        self.client.cancel_mkt_data(2001)

        # Asked of an option nobody is watching, each opens a watch and waits
        # for the model; withdrawn at once, neither is answered.
        self.client.calculate_option_price(2103, put, 0.3, g["und_price"])
        self.client.cancel_calculate_option_price(2103)
        self.client.calculate_implied_volatility(2104, put, g["opt_price"], g["und_price"])
        self.client.cancel_calculate_implied_volatility(2104)
        time.sleep(15)
        for req_id in (2103, 2104):
            assert not self.wrapper.computations(req_id), f"{req_id} was answered after its withdrawal"
            assert not self.wrapper.said_under(req_id), self.wrapper.said_under(req_id)

    def test_an_option_not_held_is_neither_exercised_nor_lapsed(self):
        """The venue answers an instruction on an option the account does not
        hold with its own refusal, under the instruction's number."""
        call, _ = self._options()
        account = self.client.get_account_id()
        # Stated with override, so held it would really be exercised: the
        # test's premise is checked before anything is sent.
        held = [h for h in self.client.positions() if h[1].con_id == call.con_id and h[2] != 0]
        assert not held, f"the account holds the option this test exercises: {held}"
        exercised, lapsed = self.wrapper.next_id, self.wrapper.next_id + 1
        self.client.exercise_options(exercised, call, 1, 1, account, 1)
        self.client.exercise_options(lapsed, call, 2, 1, account, 1)
        for req_id in (exercised, lapsed):
            assert wait_for(lambda: self.wrapper.said_under(req_id), 20), (
                f"the venue said nothing to instruction {req_id} within 20s"
            )
            assert self.wrapper.said_under(req_id)[0][0] == 399, self.wrapper.said_under(req_id)
