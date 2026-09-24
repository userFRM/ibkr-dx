"""Retired order instructions are refused or warned under the order number."""

import pytest

import ibkr_dx


class Heard(ibkr_dx.EWrapper):
    def __init__(self):
        self.errors = []
        self.orders = []

    def error(self, req_id, error_time, code, message, advanced_order_reject_json=""):
        self.errors.append((req_id, code, message))

    def open_order(self, order_id, contract, order, state):
        self.orders.append(order)


def session(features=()):
    heard = Heard()
    client = ibkr_dx.EClient(heard)
    client._test_connect("DU1")
    client._test_map_con_id(756733, 0)
    client._test_set_enabled_features(list(features))
    contract = ibkr_dx.Contract(conId=756733, symbol="SPY", secType="STK", exchange="SMART")
    order = ibkr_dx.Order(action="BUY", totalQuantity=1, orderType="LMT", lmtPrice=100)
    client._test_take_commands()
    return client, heard, contract, order


@pytest.mark.parametrize("camel,snake,value,code,name", [
    ("eTradeOnly", "e_trade_only", True, 10268, "EtradeOnly"),
    ("firmQuoteOnly", "firm_quote_only", True, 10269, "FirmQuoteOnly"),
    ("nbboPriceCap", "nbbo_price_cap", 0.0, 10270, "NbboPriceCap"),
])
def test_retired_order_instructions_are_refused(camel, snake, value, code, name):
    client, heard, contract, order = session(["DEPRETFQNC"])
    setattr(order, camel, value)
    assert getattr(order, snake) == value
    client.placeOrder(91, contract, order)
    assert heard.errors == [(91, code, f"The '{name}' order attribute is not supported.")]
    assert not client._test_take_commands()


def test_retired_order_instructions_are_warned_and_removed():
    client, heard, contract, order = session()
    assert not order.eTradeOnly and not order.firmQuoteOnly
    assert order.nbboPriceCap == ibkr_dx.UNSET_DOUBLE
    order.eTradeOnly = order.firmQuoteOnly = True
    order.nbboPriceCap = 0.0
    order.optOutSmartRouting = True
    client.place_order(92, contract, order)
    assert any("SubmitEx" in cmd for cmd in client._test_take_commands())
    client._test_dispatch_once()
    assert [(req_id, code) for req_id, code, _ in heard.errors] == [
        (92, 2168), (92, 2169), (92, 2170), (92, 2181),
    ]
    client.reqOpenOrders()
    client._test_dispatch_once()
    assert heard.orders
    assert not heard.orders[-1].eTradeOnly and not heard.orders[-1].firmQuoteOnly
    assert heard.orders[-1].nbboPriceCap == ibkr_dx.UNSET_DOUBLE
    assert order.eTradeOnly and order.firmQuoteOnly and order.nbboPriceCap == 0.0


def test_the_option_list_is_checked_before_retired_order_instructions():
    client, heard, contract, order = session(["DEPRETFQNC"])
    order.eTradeOnly = order.firmQuoteOnly = True
    order.nbboPriceCap = 0.0
    order.optOutSmartRouting = True
    order.orderMiscOptions = [ibkr_dx.TagValue("unknown", "1")]
    client.placeOrder(93, contract, order)
    assert [code for _, code, _ in heard.errors] == [10337]
    heard.errors.clear()
    client._test_set_enabled_features(["DEPRETFQNC", "NOAPIMISCVLD"])
    client.placeOrder(93, contract, order)
    assert [code for _, code, _ in heard.errors] == [10268]
    assert not client._test_take_commands()


def test_retired_order_warnings_precede_a_later_instruction_refusal():
    client, heard, contract, order = session(["DEPRPREFBEST"])
    order.eTradeOnly = order.firmQuoteOnly = True
    order.nbboPriceCap = 0.0
    order.optOutSmartRouting = True
    client.placeOrder(94, contract, order)
    assert [(req_id, code) for req_id, code, _ in heard.errors] == [
        (94, 2168), (94, 2169), (94, 2170), (94, 10348),
    ]
    assert not client._test_take_commands()


def test_an_orders_option_list_is_checked_before_its_destination_and_conditions():
    client, heard, contract, order = session()
    order.orderMiscOptions = [ibkr_dx.TagValue("unknown", "1")]
    order.conditions = [object()]
    contract.exchange = ""
    client.placeOrder(95, contract, order)
    assert [code for _, code, _ in heard.errors] == [10337]
    heard.errors.clear()
    contract.exchange = "SMART"
    client.placeOrder(95, contract, order)
    assert [code for _, code, _ in heard.errors] == [10337]
    assert not client._test_take_commands()


def test_an_orders_manual_value_is_checked_after_its_preview_and_trail():
    client, heard, contract, order = session(["NOAPIMISCVLD", "DEPRETFQNC"])
    order.orderMiscOptions = [ibkr_dx.TagValue("manual", "2")]
    order.eTradeOnly = True
    order.whatIf = True
    order.transmit = False
    client.placeOrder(96, contract, order)
    assert heard.errors == [(96, 321, "What-If order should have transmit flag set to TRUE ")]
    heard.errors.clear()
    order.transmit = True
    order.orderType = "TRAIL"
    order.trailingPercent = 150.0
    client.placeOrder(96, contract, order)
    assert heard.errors == [(96, 321,
        "Invalid Trailing Percent value. Valid values are greater than 0 and less than 100.")]
    heard.errors.clear()
    order.trailingPercent = 1.0
    client.placeOrder(96, contract, order)
    assert heard.errors == [(96, 321,
        "Order: 'manual' has wrong value=2, expected [1 or 0]")]
    assert not client._test_take_commands()


@pytest.mark.parametrize("preview,strategy", [(True, ""), (False, "Adaptive")])
def test_retired_order_instructions_are_checked_for_previews_and_algorithms(preview, strategy):
    client, heard, contract, order = session(["DEPRETFQNC"])
    order.whatIf, order.algoStrategy, order.eTradeOnly = preview, strategy, True
    client.placeOrder(97, contract, order)
    assert heard.errors == [(97, 10268, "The 'EtradeOnly' order attribute is not supported.")]
    assert not client._test_take_commands()
