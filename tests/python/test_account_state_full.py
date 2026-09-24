"""Full account state — positions, P&L, portfolio, histogram, 90 tags.

Comprehensive account interrogation: reqPositions, reqPnL, reqPnLSingle,
reqAccountUpdates (90 tags), updatePortfolio, reqHistogramData.

Run: pytest tests/python/test_account_state_full.py -v -s
"""

import os, threading, time
import pytest
from conftest import declined, give_back, inside_the_session, liquid_hours, wait_for
from ibkr_dx import EWrapper, EClient, Contract, Order

pytestmark = pytest.mark.skipif(
    not (os.environ.get("IB_USERNAME") and os.environ.get("IB_PASSWORD")),
    reason="IB_USERNAME and IB_PASSWORD not set",
)

SPY_CON_ID = 756733


class AccountDeepWrapper(EWrapper):
    def __init__(self):
        super().__init__()
        self.connected = threading.Event()
        self.lock = threading.Lock()
        self.account_id = ""

        # Positions
        self.positions = []
        self.got_position_end = threading.Event()

        # Account values (90+ tags)
        self.account_values = {}
        self.got_account_download_end = threading.Event()

        # Portfolio
        self.portfolio = []
        self.got_portfolio = threading.Event()

        # P&L
        self.pnl_data = None
        self.got_pnl = threading.Event()
        # Not `pnl_single`: an attribute of that name shadows the callback
        # of that name, and the venue's answer lands on a dict rather than a
        # method.
        self.pnl_single_data = {}  # req_id -> data
        self.got_pnl_single = threading.Event()

        # Histogram
        self.histogram = []
        self.got_histogram = threading.Event()

        self.next_id = 0
        self.statuses = {}  # order_id -> [status, ...]
        self.errors = []  # (req_id, code, message)

    def next_valid_id(self, order_id):
        self.next_id = order_id
        self.connected.set()

    def order_status(self, order_id, status, filled, remaining, avg_fill_price, perm_id,
                     parent_id, last_fill_price, client_id, why_held, mkt_cap_price):
        self.statuses.setdefault(order_id, []).append(status)

    def managed_accounts(self, accounts_list):
        self.account_id = accounts_list

    def connect_ack(self):
        pass

    def position(self, account, contract, pos, avg_cost):
        with self.lock:
            self.positions.append({
                "account": account,
                "symbol": contract.symbol,
                "sec_type": contract.sec_type,
                "con_id": contract.con_id,
                "position": pos,
                "avg_cost": avg_cost,
            })

    def position_end(self):
        self.got_position_end.set()

    def update_account_value(self, key, value, currency, account_name):
        self.account_values[key] = (value, currency)

    def update_portfolio(self, contract, position, market_price, market_value,
                         average_cost, unrealized_pnl, realized_pnl, account_name):
        with self.lock:
            self.portfolio.append({
                "symbol": contract.symbol,
                "con_id": contract.con_id,
                "position": position,
                "market_price": market_price,
                "market_value": market_value,
                "average_cost": average_cost,
                "unrealized_pnl": unrealized_pnl,
                "realized_pnl": realized_pnl,
            })
        self.got_portfolio.set()

    def update_account_time(self, timestamp):
        pass

    def account_download_end(self, account):
        self.got_account_download_end.set()

    def pnl(self, req_id, daily_pnl, unrealized_pnl, realized_pnl):
        self.pnl_data = {
            "daily_pnl": daily_pnl,
            "unrealized_pnl": unrealized_pnl,
            "realized_pnl": realized_pnl,
        }
        self.got_pnl.set()

    def pnl_single(self, req_id, pos, daily_pnl, unrealized_pnl, realized_pnl, value):
        self.pnl_single_data[req_id] = {
            "pos": pos,
            "daily_pnl": daily_pnl,
            "unrealized_pnl": unrealized_pnl,
            "realized_pnl": realized_pnl,
            "value": value,
        }
        self.got_pnl_single.set()

    def histogram_data(self, req_id, items):
        with self.lock:
            self.histogram = items
        self.got_histogram.set()

    def error(self, req_id, error_time, error_code, error_string, advanced_order_reject_json=""):
        self.errors.append((req_id, error_code, error_string))
        if error_code not in (2104, 2106, 2158):
            print(f"  [error] reqId={req_id} code={error_code}: {error_string}")


class TestAccountDeep:
    """Comprehensive account state interrogation."""

    @pytest.fixture(autouse=True)
    def setup_connection(self):
        self.wrapper = AccountDeepWrapper()
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

    def test_positions(self):
        """ReqPositions returns all holdings."""
        self.client.req_positions()
        assert self.wrapper.got_position_end.wait(timeout=15), "position_end not received"

        print(f"  Positions: {len(self.wrapper.positions)}")
        for p in self.wrapper.positions:
            print(f"    {p['symbol']} ({p['sec_type']}): qty={p['position']} "
                  f"avg_cost={p['avg_cost']:.2f} conId={p['con_id']}")

    def test_account_values_carry_the_tags_the_wire_states(self):
        """The account figures the venue states, and the ones a caller needs.

        Named after ninety tags and asserting fifteen, this read as a check on
        a surface it does not cover: what arrives here is what the account
        stream carries, and the rest of the published table is assembled by the
        counterpart from messages this client does not yet read. Named after
        what it checks instead, and the critical tags below are what it is
        actually protecting.
        """
        self.client.req_account_updates(True, "")
        assert self.wrapper.got_account_download_end.wait(timeout=30), \
            "account_download_end not received"
        self.client.req_account_updates(False, "")

        tag_count = len(self.wrapper.account_values)
        print(f"  Account tags: {tag_count}")
        assert tag_count >= 15, f"Expected 15+ tags, got {tag_count}"

        # Verify critical tags
        critical = ["NetLiquidation", "TotalCashValue", "BuyingPower",
                     "EquityWithLoanValue", "Cushion"]
        for key in critical:
            assert key in self.wrapper.account_values, f"Missing critical tag: {key}"
            val = self.wrapper.account_values[key][0]
            print(f"    {key}: {val}")

        # AccountType requires Gateway-internal UT message (not on direct CCP wire).
        # assert "AccountType" in self.wrapper.account_values

    def test_portfolio(self):
        """UpdatePortfolio fires for each holding with market prices."""
        self.client.req_account_updates(True, "")
        self.wrapper.got_account_download_end.wait(timeout=30)
        self.client.req_account_updates(False, "")

        print(f"  Portfolio items: {len(self.wrapper.portfolio)}")
        for p in self.wrapper.portfolio:
            print(f"    {p['symbol']}: pos={p['position']} mktPrice={p['market_price']:.2f} "
                  f"mktVal={p['market_value']:.2f} unrealPnL={p['unrealized_pnl']:.2f}")

        # Verify positions match
        self.client.req_positions()
        self.wrapper.got_position_end.wait(timeout=15)

        pos_conids = {p["con_id"] for p in self.wrapper.positions if p["position"] != 0}
        port_conids = {p["con_id"] for p in self.wrapper.portfolio if p["position"] != 0}
        if pos_conids:
            assert pos_conids == port_conids, \
                f"Position/portfolio mismatch: pos={pos_conids} port={port_conids}"

    def test_whole_account_pnl(self):
        """ReqPnL returns daily, unrealized, and realized P&L."""
        acct = self.client.get_account_id()
        self.client.req_pnl(8001, acct)

        got = self.wrapper.got_pnl.wait(timeout=15)
        self.client.cancel_pnl(8001)

        # Reported whether or not the account holds anything: a flat account
        # states its figures at nought. A refusal is this client's to explain.
        refused = [e for e in self.wrapper.errors if e[0] == 8001]
        assert not refused, f"the profit request was refused: {refused}"
        assert got, "the account was asked for its running profit and stated none"

        pnl = self.wrapper.pnl_data
        print(f"  Account P&L: daily={pnl['daily_pnl']:.2f} "
              f"unrealized={pnl['unrealized_pnl']:.2f} "
              f"realized={pnl['realized_pnl']:.2f}")
        assert isinstance(pnl["daily_pnl"], float)

    def test_pnl_single_per_position(self):
        """ReqPnLSingle on a holding the test makes for itself: one share of
        SPY, bought inside the venue's hours and sold back afterwards."""
        spy = Contract()
        spy.con_id, spy.symbol, spy.sec_type, spy.exchange, spy.currency = SPY_CON_ID, "SPY", "STK", "SMART", "USD"
        if not inside_the_session(self.client, spy):
            pytest.skip(
                "a market order fills inside the venue's liquid hours, and they say "
                f"SPY's session is shut ({liquid_hours(self.client, spy)})"
            )

        def market(action):
            o = Order()
            o.action, o.order_type, o.total_quantity = action, "MKT", 1
            return o

        acct = self.client.get_account_id()
        self.client.req_positions()
        bought, sold = self.wrapper.next_id, self.wrapper.next_id + 1
        self.client.place_order(bought, spy, market("BUY"))
        try:
            assert wait_for(lambda: "Filled" in self.wrapper.statuses.get(bought, []), 30), (
                f"one share bought at market inside the session did not fill: "
                f"{self.wrapper.statuses.get(bought)}"
            )
            assert wait_for(lambda: any(
                p["con_id"] == SPY_CON_ID and p["position"] >= 1 for p in self.wrapper.positions
            ), 15), "the holding the fill made was not reported"
            self.client.req_pnl_single(8100, acct, "", SPY_CON_ID)
            answered = wait_for(lambda: 8100 in self.wrapper.pnl_single_data, 15)
            self.client.cancel_pnl_single(8100)
            refused = [e for e in self.wrapper.errors if e[0] == 8100]
            assert not refused, f"the position's profit request was refused: {refused}"
            assert answered, "the account holds SPY and no profit was reported on it"
            data = self.wrapper.pnl_single_data[8100]
            print(f"    SPY: pos={data['pos']} daily={data['daily_pnl']:.2f} "
                  f"unrealized={data['unrealized_pnl']:.2f} value={data['value']:.2f}")
            assert data["pos"] >= 1
        finally:
            give_back(self.client, lambda oid: self.wrapper.statuses.get(oid, []),
                      bought, sold, spy, market("SELL"))
            self.client.cancel_positions()

    def test_histogram_data(self):
        """ReqHistogramData for SPY (1 week)."""
        spy = Contract()
        spy.con_id = SPY_CON_ID
        spy.symbol = "SPY"
        spy.sec_type = "STK"
        spy.exchange = "SMART"
        spy.currency = "USD"

        self.client.req_histogram_data(8200, spy, False, "1 week")

        wait_for(lambda: self.wrapper.got_histogram.is_set()
                 or [e for e in self.wrapper.errors if e[0] == 8200], 30)
        got = self.wrapper.got_histogram.is_set()
        self.client.cancel_histogram_data(8200)

        # A historical-service question, answered at any hour: only the
        # venue declining it is a reason to have no answer.
        refused = declined(self.wrapper.errors, 8200)
        if not got and refused:
            pytest.skip(f"the venue declined the histogram: {refused[0]}")
        assert got, f"no histogram within 30s: {[e for e in self.wrapper.errors if e[0] == 8200]}"

        print(f"  Histogram: {len(self.wrapper.histogram)} price points")
        assert len(self.wrapper.histogram) > 0, "Should have histogram data"
