"""A malformed contract expiry is refused before the request reaches the venue."""

import pytest

import ibkr_dx
from ibkr_dx import Contract


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, message, advanced_order_reject_json=""):
        self.seen.append((req_id, code, message))


def client_and_contract(expiry="20260230", con_id=756733):
    wrapper = Errors()
    client = ibkr_dx.EClient(wrapper)
    client._test_connect("DU123")
    client._test_map_con_id(756733, 0)
    contract = Contract()
    contract.conId = con_id
    contract.symbol = "SPY"
    contract.secType = "STK"
    contract.exchange = "SMART"
    contract.currency = "USD"
    contract.lastTradeDateOrContractMonth = expiry
    return wrapper, client, contract


REQUESTS = [
    ("market data", lambda c, ct: c.reqMktData(71, ct, "", False, False)),
    ("market data with explicit mode", lambda c, ct: c.req_mkt_data_ex(71, ct, "", False, False, 1)),
    ("market data with news", lambda c, ct: c.reqMktData(71, ct, "292", False, False)),
    ("depth", lambda c, ct: c.reqMktDepth(71, ct, 5, False)),
    ("tick by tick", lambda c, ct: c.reqTickByTickData(71, ct, "Last", 0, False)),
    ("real time bars", lambda c, ct: c.reqRealTimeBars(71, ct, 5, "TRADES", 1)),
    ("contract details", lambda c, ct: c.reqContractDetails(71, ct)),
    ("historical bars", lambda c, ct: c.reqHistoricalData(
        71, ct, "", "1 D", "1 hour", "TRADES", 1, 1, False)),
    ("historical schedule", lambda c, ct: c.reqHistoricalSchedule(71, ct, "", "1 D", True)),
    ("schedule through historical bars", lambda c, ct: c.reqHistoricalData(
        71, ct, "", "1 D", "1 day", "SCHEDULE", 1, 1, False)),
    ("head timestamp", lambda c, ct: c.reqHeadTimeStamp(71, ct, "TRADES", 1, 1)),
    ("histogram", lambda c, ct: c.reqHistogramData(71, ct, True, "1 D")),
    ("historical ticks", lambda c, ct: c.reqHistoricalTicks(
        71, ct, "20260923 10:00:00", "", 10, "TRADES", 1, False)),
    ("implied volatility", lambda c, ct: c.calculateImpliedVolatility(71, ct, 1.0, 100.0)),
    ("option price", lambda c, ct: c.calculateOptionPrice(71, ct, 0.2, 100.0)),
    ("exercise", lambda c, ct: c.exerciseOptions(71, ct, 1, 1, "DU123", 1)),
]


@pytest.mark.parametrize("name,ask", REQUESTS, ids=[r[0] for r in REQUESTS])
@pytest.mark.parametrize("con_id", [0, 756733])
def test_contract_expiry_is_refused_under_the_request_id_without_sending(name, ask, con_id):
    wrapper, client, contract = client_and_contract(con_id=con_id)
    ask(client, contract)
    client._test_dispatch_once()
    assert wrapper.seen == [(71, 10372,
        "lastTradeDateOrContractMonth: The date entered is invalid. The correct format is "
        "yyyyMM for a contract month or yyyyMMdd for a date. E.g.: 202607 or 20260724.")]
    assert client._test_take_commands() == []


@pytest.mark.parametrize("expiry", ["", "noexp", "197801", "30001231", "20240229"])
def test_contract_expiry_keeps_the_value_the_caller_gave(expiry):
    wrapper, client, contract = client_and_contract(expiry)
    client.reqContractDetails(71, contract)
    assert wrapper.seen == []
    sent = client._test_take_commands()
    assert len(sent) == 1
    assert "FetchContractDetails" in sent[0]
    assert f'last_trade_date_or_contract_month: "{expiry}"' in sent[0]


def test_contract_expiry_is_not_checked_on_a_fundamental_report():
    wrapper, client, contract = client_and_contract()
    client.reqFundamentalData(71, contract, "ReportSnapshot", [])
    assert wrapper.seen == []
    assert any("FetchFundamentalData" in cmd for cmd in client._test_take_commands())


def test_depth_requires_an_exchange_before_checking_expiry():
    wrapper, client, contract = client_and_contract()
    contract.exchange = ""
    client.reqMktDepth(71, contract, 5, False)
    client._test_dispatch_once()
    assert [(request, code, message) for request, code, message in wrapper.seen] == [
        (71, 321, "Please enter exchange.")
    ]
    assert client._test_take_commands() == []


def test_order_fields_are_checked_before_contract_expiry():
    wrapper, client, contract = client_and_contract()
    client.placeOrder(71, contract, ibkr_dx.Order())
    client._test_dispatch_once()
    assert len(wrapper.seen) == 1
    assert wrapper.seen[0][0:2] == (71, 321)
    assert client._test_take_commands() == []
