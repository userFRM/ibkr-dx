"""Attached fields on Order reach the placement checks.

The attached fields are read with the request, so a gateway refuses a bad pair
as a request it could not read (320); their numbers are checked after the
order has been validated."""

import pytest

import ibkr_dx


class Recorder(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []

    def error(self, reqId, errorTime, code, msg, advanced=""):
        self.errors.append((reqId, code, msg))


@pytest.mark.parametrize(
    "fields, code, message",
    [
        ({"slOrderId": 9402}, 320, "Error reading request: Invalid value for Stop Loss order-id or order-type"),
        ({"ptOrderId": 9402}, 320, "Error reading request: Invalid value for Profit Taker order-id or order-type"),
        ({"slOrderType": "PRESET"}, 320, "Error reading request: Invalid value for Stop Loss order-id or order-type"),
        ({"ptOrderType": "PRESET"}, 320, "Error reading request: Invalid value for Profit Taker order-id or order-type"),
        ({"slOrderId": 0, "slOrderType": "PRESET"}, 10149, "Invalid order id: 0"),
        ({"ptOrderId": 0, "ptOrderType": "PRESET"}, 10149, "Invalid order id: 0"),
    ],
)
def test_attached_field_checks_use_the_parent_request(fields, code, message):
    wrapper = Recorder()
    client = ibkr_dx.EClient(wrapper)
    client._test_connect()
    contract = ibkr_dx.Contract()
    contract.conId = 756733
    contract.secType = "STK"
    contract.exchange = "SMART"
    order = ibkr_dx.Order()
    order.action = "BUY"
    order.totalQuantity = 1
    order.orderType = "LMT"
    order.lmtPrice = 100
    order.tif = "DAY"
    for name, value in fields.items():
        setattr(order, name, value)
    client.placeOrder(9401, contract, order)
    client.poll()
    assert wrapper.errors == [(9401, code, message)]
    client.disconnect()


def test_a_contract_date_is_checked_before_the_order_numbers():
    wrapper = Recorder()
    client = ibkr_dx.EClient(wrapper)
    client._test_connect()
    contract = ibkr_dx.Contract()
    contract.conId = 756733
    contract.secType = "STK"
    contract.exchange = "SMART"
    contract.lastTradeDateOrContractMonth = "2026-07"
    order = ibkr_dx.Order()
    order.action = "BUY"
    order.totalQuantity = 1
    order.orderType = "LMT"
    order.lmtPrice = 100
    order.tif = "DAY"
    client.placeOrder(0, contract, order)
    client.poll()
    assert [(req, code) for req, code, _ in wrapper.errors] == [(0, 10372)]
    assert wrapper.errors[0][2].startswith("lastTradeDateOrContractMonth: The date entered is invalid.")
    client.disconnect()
