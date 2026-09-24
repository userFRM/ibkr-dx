"""Shared fixtures for the compatibility tests."""

from ibkr_dx import EWrapper


class NotConnectedProbe(EWrapper):
    """Captures what a call reports when it is made before connecting.

    The reference client answers such a call on the error callback and returns
    normally rather than raising, so that is what these tests assert.
    """

    NOT_CONNECTED = 504

    def __init__(self):
        super().__init__()
        self.errors = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.errors.append((req_id, code, msg))

    @property
    def not_connected(self):
        return any(code == self.NOT_CONNECTED for _, code, _ in self.errors)


def next_option_expiry(at_least_days=2) -> str:
    """The next Friday a weekly option on a big US name expires, as yyyymmdd.

    Written out as a fixed date, a test stops exercising anything the moment
    that date passes: the contract stops existing, the quote never arrives, and
    the test skips itself as "market closed" for ever after. Computed, it keeps
    naming a contract that exists.
    """
    import datetime

    day = datetime.date.today() + datetime.timedelta(days=at_least_days)
    day += datetime.timedelta(days=(4 - day.weekday()) % 7)   # 4 is Friday
    return day.strftime("%Y%m%d")


def wait_for(held, timeout=30.0):
    """True once `held()` is true, False if it never is inside `timeout`.

    An account with anything working at all replays those orders the moment a
    session opens, and every one of them is an order status. A test that waits
    on "a status arrived" is therefore released before its own order has even
    reached the venue, and reads an empty list under its own order id. What a
    test is waiting for is its own order, so that is what it waits on.
    """
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if held():
            return True
        time.sleep(0.05)
    return bool(held())


def inside_the_session(client, contract):
    """Whether the venue says this contract's own session is open right now.

    Asked of the venue rather than worked out from a clock here: it states the
    liquid hours on the contract, in the contract's own zone, and those are the
    hours a trade stream carries prints for. A quote is no evidence — outside
    the session a contract carries a bid, an ask and the price of the last trade
    there ever was.

    The contract is looked up with the call that answers, so this reads nothing
    off a test's own wrapper. A contract the venue does not name is a failure,
    not a closed session: reference data is answered at any hour.
    """
    import datetime
    import zoneinfo

    found = client.contract_details(contract)
    assert found, f"the venue named no contract for {contract.symbol}"
    details = found[0]
    hours = getattr(details, "liquid_hours", "") or ""
    zone = getattr(details, "time_zone_id", "") or "US/Eastern"
    now = datetime.datetime.now(zoneinfo.ZoneInfo(zone))
    for span in hours.split(";"):
        if "-" not in span or "CLOSED" in span:
            continue
        opens, shuts = span.split("-", 1)
        try:
            a = datetime.datetime.strptime(opens, "%Y%m%d:%H%M")
            b = datetime.datetime.strptime(shuts, "%Y%m%d:%H%M")
        except ValueError:
            continue
        if a.replace(tzinfo=now.tzinfo) <= now <= b.replace(tzinfo=now.tzinfo):
            return True
    return False


def liquid_hours(client, contract):
    """The hours the venue states for a contract, to name in a skip."""
    found = client.contract_details(contract)
    return getattr(found[0], "liquid_hours", "") if found else ""


# What the engine says, under no request, when one of its data connections
# drops: market data, then historical.
FEED_DOWN = (2103, 2105)
# What it reports the venue declining a request under: data the account is not
# subscribed to (354), and the historical service's refusal (162). A
# subscription the venue refused is reported under 200, and a request riding
# beside a quote under 321, each in words saying the venue refused it.
VENUE_DECLINED = (354, 162)


def declined(errors, *req_ids):
    """The venue declining one of these requests, or a data connection
    dropping: the only reasons a live test skips rather than fails.

    `errors` holds (req_id, code, message). A refusal this client makes itself
    -- 320, 321, 10337, 504 and the rest -- is none of these, so a regression
    that refuses a request fails the test rather than reading as the venue's
    choice.
    """
    return [
        (r, code, msg) for r, code, msg in errors
        if (r == -1 and code in FEED_DOWN)
        or (r in req_ids and (code in VENUE_DECLINED or str(msg).startswith("the venue refused")))
    ]


def give_back(client, statuses, bought, sold, contract, sell):
    """Leave the account as a test found it after buying one share.

    A buy that has not filled is withdrawn, and a share is sold back only once
    the buy says it filled: sold regardless, a buy that was refused or had not
    filled left the account short, and one that filled after the sale traded
    twice. `statuses(order_id)` lists the statuses an order has reported.
    """
    if "Filled" not in statuses(bought):
        client.cancel_order(bought, "")
        wait_for(lambda: {"Filled", "Cancelled", "ApiCancelled", "Inactive"} & set(statuses(bought)), 20)
    if "Filled" in statuses(bought):
        client.place_order(sold, contract, sell)
        assert wait_for(lambda: "Filled" in statuses(sold), 30), (
            f"the share bought was not sold back: {statuses(sold)}"
        )
