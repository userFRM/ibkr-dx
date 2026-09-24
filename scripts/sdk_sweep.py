"""Ask the venue everything a program written against the TWS API asks, and say what came back.

The offline suites prove each call is carried. They cannot prove the venue
answers it, and an answer of nothing looks the same as a market with nothing in
it. This runs one session through `EClient`, asks for everything the reference
client's program asks for — the requests themselves, not this client's own
helpers — and prints one line per request: what arrived under it, and what was
said about it.

It fails a request that raised, or that was answered by neither data nor a word
under its number. The streams — trades, a book, five-second bars — are held to
that only inside the contract's own liquid hours, which the venue states;
outside them a stream that says nothing is a closed market.

Nothing here places an order. The two order calls it does make are previews,
which the venue prices and does not place.

    IB_USERNAME=… IB_PASSWORD=… python scripts/sdk_sweep.py
"""

import collections
import datetime
import os
import sys
import threading
import time
import zoneinfo

from ibkr_dx import Contract, EClient, EWrapper, Order

#: Notices about a connection coming and going, which answer nothing.
CONNECTION_NOTICES = {2100, 2103, 2104, 2105, 2106, 2107, 2119, 2158}

#: How long a question is given to be answered.
ANSWER = 20


class Heard(EWrapper):
    """What arrived, by the request it arrived under, or by the callback where
    a callback names no request."""

    def __init__(self):
        super().__init__()
        self.ready = threading.Event()
        self.lock = threading.Lock()
        self.next_id = 0
        self.got = collections.defaultdict(list)
        self.said = collections.defaultdict(list)

    def _got(self, key, what):
        with self.lock:
            self.got[key].append(what)

    def next_valid_id(self, order_id):
        self.next_id = order_id
        self.ready.set()

    def connect_ack(self):
        pass

    def error(self, req_id, error_time, code, message, advanced=""):
        if code not in CONNECTION_NOTICES:
            with self.lock:
                self.said[req_id].append((code, message[:80]))

    def contract_details(self, req_id, details):
        self._got(req_id, details)

    def contract_details_end(self, req_id):
        self._got(("end", req_id), True)

    def security_definition_option_parameter(self, req_id, *rest):
        self._got(req_id, rest)

    def symbol_samples(self, req_id, descriptions):
        self._got(req_id, descriptions)

    def head_timestamp(self, req_id, stamp):
        self._got(req_id, stamp)

    def histogram_data(self, req_id, items):
        self._got(req_id, items)

    def fundamental_data(self, req_id, data):
        self._got(req_id, data)

    def historical_schedule(self, req_id, *rest):
        self._got(req_id, rest)

    def historical_news(self, req_id, *rest):
        self._got(req_id, rest)

    def historical_news_end(self, req_id, has_more):
        self._got(("end", req_id), has_more)

    def historical_data(self, req_id, bar):
        self._got(req_id, bar)

    def tick_price(self, req_id, tick_type, price, attrib):
        self._got(req_id, (tick_type, price))

    def tick_snapshot_end(self, req_id):
        self._got(("end", req_id), True)

    def tick_by_tick_all_last(self, req_id, *rest):
        self._got(req_id, rest)

    def tick_by_tick_bid_ask(self, req_id, *rest):
        self._got(req_id, rest)

    def update_mkt_depth(self, req_id, *rest):
        self._got(req_id, rest)

    def update_mkt_depth_l2(self, req_id, *rest):
        self._got(req_id, rest)

    def real_time_bar(self, req_id, *rest):
        self._got(req_id, rest)

    def pnl(self, req_id, *rest):
        self._got(req_id, rest)

    def account_summary(self, req_id, *rest):
        self._got(req_id, rest)

    def position(self, *rest):
        self._got("position", rest)

    def position_end(self):
        self._got("position_end", True)

    def managed_accounts(self, accounts):
        self._got("managed_accounts", accounts)

    def open_order(self, order_id, contract, order, state):
        self._got(order_id, state)
        self._got("open_order", order_id)

    def open_order_end(self):
        self._got("open_order_end", True)

    def completed_order(self, *rest):
        self._got("completed_order", rest)

    def completed_orders_end(self):
        self._got("completed_orders_end", True)

    def answered(self, key):
        with self.lock:
            return list(self.got.get(key, [])), list(self.said.get(key, []))


def inside_the_session(details):
    """Whether the venue's own liquid hours for a contract include now."""
    hours = getattr(details, "liquid_hours", "") or ""
    zone = getattr(details, "time_zone_id", "") or "US/Eastern"
    now = datetime.datetime.now(zoneinfo.ZoneInfo(zone))
    for span in hours.split(";"):
        if "-" not in span or "CLOSED" in span:
            continue
        opens, shuts = span.split("-", 1)
        try:
            a = datetime.datetime.strptime(opens, "%Y%m%d:%H%M").replace(tzinfo=now.tzinfo)
            b = datetime.datetime.strptime(shuts, "%Y%m%d:%H%M").replace(tzinfo=now.tzinfo)
        except ValueError:
            continue
        if a <= now <= b:
            return True
    return False


def connect(heard):
    """A session on the paper account, its dispatch running, or why not."""
    username = os.environ.get("IB_USERNAME", "")
    password = os.environ.get("IB_PASSWORD", "")
    if not username or not password:
        print("IB_USERNAME and IB_PASSWORD are unset; this needs a session.")
        return None
    client = EClient(heard)
    client.connect(
        username=username, password=password,
        host=os.environ.get("IB_HOST", "cdc1.ibllc.com"), paper=True,
    )
    threading.Thread(target=client.run, daemon=True).start()
    if not heard.ready.wait(timeout=60):
        print("no session was opened within a minute")
        client.disconnect()
        return None
    return client


def described(symbol, sec_type, exchange):
    made = Contract()
    made.symbol, made.sec_type, made.exchange, made.currency = symbol, sec_type, exchange, "USD"
    return made


def main():
    heard = Heard()
    client = connect(heard)
    if client is None:
        return 2
    raised, silent = [], []

    def wait(done, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline and not done():
            time.sleep(0.1)

    def ask(what, key, send, *, until=None, hold=None, stop=None, stream_open=None):
        """Send one request and say what came back under `key`.

        `until` is what ends the wait early; `hold` is a stream's length, held
        whole; `stop` withdraws it. A stream is excused silence only where
        `stream_open` says the contract's session is shut.
        """
        try:
            send()
        except Exception as e:  # noqa: BLE001 — the point is to report it
            print(f"  {what:26} raised {type(e).__name__}: {str(e).splitlines()[0][:70]}")
            raised.append(what)
            return []
        if hold is not None:
            time.sleep(hold)
        else:
            wait(lambda: (until() if until else heard.answered(key)[0]) or heard.answered(key)[1], ANSWER)
        if stop is not None:
            stop()
        got, said = heard.answered(key)
        print(f"  {what:26} {len(got) if got else '—'} {said[:1] if said else ''}", flush=True)
        if not got and not said:
            if stream_open is False:
                print(f"  {'':26} (the contract's session is shut, so a quiet stream is excused)")
            else:
                silent.append(what)
        return got

    print("\nreference data")
    spy_details = ask("contract details", 1, lambda: client.req_contract_details(1, described("SPY", "STK", "SMART")),
                      until=lambda: heard.answered(("end", 1))[0])
    fx_details = ask("currency pair details", 2,
                     lambda: client.req_contract_details(2, described("EUR", "CASH", "IDEALPRO")),
                     until=lambda: heard.answered(("end", 2))[0])
    if not spy_details or not fx_details:
        print("the venue named neither SPY nor EUR.USD; reference data answers at any hour")
        client.disconnect()
        return 1
    spy, fx = spy_details[0].contract, fx_details[0].contract
    open_now = inside_the_session(spy_details[0])
    print(f"  SPY's session is {'open' if open_now else 'shut'} by the venue's hours")

    ask("option chains", 3, lambda: client.req_sec_def_opt_params(3, "SPY", "", "STK", spy.con_id))
    ask("symbol search", 4, lambda: client.req_matching_symbols(4, "APP"))
    ask("head timestamp", 5, lambda: client.req_head_time_stamp(5, spy, "TRADES", 1, 1))
    ask("histogram", 6, lambda: client.req_histogram_data(6, spy, True, "3 days"))
    ask("fundamentals", 7, lambda: client.req_fundamental_data(7, spy, "ReportsFinSummary"))
    ask("trading schedule", 8,
        lambda: client.req_historical_data(8, spy, "", "7 D", "1 day", "SCHEDULE", 1, 1, False))
    ask("headlines", 9, lambda: client.req_historical_news(9, spy.con_id, "BRFG", "", "", 5),
        until=lambda: heard.answered(("end", 9))[0])

    print("\nmarket data")
    ask("bars", 10, lambda: client.req_historical_data(10, spy, "", "2 D", "1 hour", "TRADES", 1, 1, False))
    ask("bars, unqualified", 11, lambda: client.req_historical_data(
        11, described("AAPL", "STK", "SMART"), "", "1 D", "1 hour", "TRADES", 1, 1, False))
    ask("a snapshot", 12, lambda: client.req_mkt_data(12, spy, "", True, False),
        until=lambda: heard.answered(("end", 12))[0])
    ask("a currency snapshot", 13, lambda: client.req_mkt_data(13, fx, "", True, False),
        until=lambda: heard.answered(("end", 13))[0])
    ask("every trade", 14, lambda: client.req_tick_by_tick_data(14, spy, "AllLast", 0, False),
        hold=8, stop=lambda: client.cancel_tick_by_tick_data(14), stream_open=open_now)
    ask("the exchange's trades", 15, lambda: client.req_tick_by_tick_data(15, spy, "Last", 0, False),
        hold=8, stop=lambda: client.cancel_tick_by_tick_data(15), stream_open=open_now)
    ask("quote changes", 16, lambda: client.req_tick_by_tick_data(16, fx, "BidAsk", 0, False),
        hold=8, stop=lambda: client.cancel_tick_by_tick_data(16), stream_open=inside_the_session(fx_details[0]))

    print("\nsubscriptions")
    ask("depth of book", 17, lambda: client.req_mkt_depth(17, spy, 5, False),
        hold=8, stop=lambda: client.cancel_mkt_depth(17, False), stream_open=open_now)
    ask("five-second bars", 18, lambda: client.req_real_time_bars(18, spy, 5, "TRADES", False),
        hold=12, stop=lambda: client.cancel_real_time_bars(18), stream_open=open_now)
    account = client.get_account_id()
    ask("profit and loss", 19, lambda: client.req_pnl(19, account, ""), stop=lambda: client.cancel_pnl(19))

    print("\naccount")
    ask("account values", 20, lambda: client.req_account_summary(
        20, "All", "NetLiquidation,TotalCashValue,BuyingPower,AvailableFunds,ExcessLiquidity"),
        stop=lambda: client.cancel_account_summary(20))
    ask("positions", "position_end", lambda: client.req_positions())
    ask("managed accounts", "managed_accounts", lambda: client.req_managed_accts())
    ask("open orders", "open_order_end", lambda: client.req_all_open_orders())
    ask("completed orders", "completed_orders_end", lambda: client.req_completed_orders(False))

    print("\norders the venue prices and does not place")
    oid = heard.next_id

    def preview(action, quantity, price):
        order = Order()
        order.action, order.order_type, order.total_quantity, order.lmt_price = action, "LMT", quantity, price
        order.what_if = True
        return order

    ask("a share", oid, lambda: client.place_order(oid, spy, preview("BUY", 1, 1.0)))
    ask("a currency pair", oid + 1, lambda: client.place_order(oid + 1, fx, preview("BUY", 20000, 0.5)))

    client.disconnect()
    for what in raised:
        print(f"RAISED: {what}")
    for what in silent:
        print(f"SILENT: {what} was answered by neither data nor a word under its number")
    return 1 if raised or silent else 0


if __name__ == "__main__":
    sys.exit(main())
