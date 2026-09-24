"""A price of nought, and a price below it, are read as a gateway reads them.

A gateway refuses a discretionary amount below nought, and a combination priced
on every leg that states a price of its own as well, or that is not one it
prices by its legs. Taken here, the first went out without the discretion the
caller named and the others went out priced as a gateway never sends them, and
each was reported as placed.
"""

from ibkr_dx import ComboLeg, Contract, EClient, EWrapper, Order, OrderComboLeg


class _Recorder(EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []

    def error(self, req_id, error_time, code, msg, advanced=""):
        self.errors.append((req_id, code, msg))


def _session():
    recorder = _Recorder()
    client = EClient(recorder)
    client._test_connect("DU0000000")
    client._test_map_con_id(756733, 0)
    client._test_map_con_id(28812380, 1)
    return recorder, client


def _limit(price):
    order = Order()
    order.action, order.orderType, order.totalQuantity, order.lmtPrice = "BUY", "LMT", 1, price
    return order


def _spy():
    contract = Contract()
    contract.symbol, contract.secType, contract.exchange, contract.currency = "SPY", "STK", "SMART", "USD"
    contract.conId = 756733
    return contract


def _combo():
    contract = Contract()
    contract.symbol, contract.secType, contract.exchange, contract.currency = "IBKR,MCD", "BAG", "SMART", "USD"
    contract.conId = 28812380
    contract.comboLegs = []
    for con_id, action in [(43645865, "BUY"), (9408, "SELL")]:
        leg = ComboLeg()
        leg.conId, leg.ratio, leg.action, leg.exchange = con_id, 1, action, "SMART"
        contract.comboLegs.append(leg)
    return contract


def _priced_on_every_leg(price):
    order = _limit(price)
    order.orderComboLegs = []
    for leg_price in [10.0, 11.0]:
        leg = OrderComboLeg()
        leg.price = leg_price
        order.orderComboLegs.append(leg)
    return order


def test_a_discretionary_amount_below_nought_is_refused_under_168():
    recorder, client = _session()
    order = _limit(10.0)
    order.discretionaryAmt = -0.01
    client.place_order(1, _spy(), order)
    assert recorder.errors == [(1, 168, "Discretionary amount does not conform to the minimum "
                                        "price variation for this contract")]


def test_a_combination_priced_on_every_leg_and_on_itself_is_refused_under_10054():
    recorder, client = _session()
    client.place_order(2, _combo(), _priced_on_every_leg(21.0))
    assert recorder.errors == [(2, 10054, "Can't specify combo price when using per-leg prices.")]


def test_a_combination_priced_on_every_leg_alone_is_refused_under_10058():
    # A gateway prices a combination by its legs only where it is a
    # non-guaranteed one of two legs routed by SMART, and this client carries
    # nothing that makes one non-guaranteed.
    recorder, client = _session()
    client.place_order(3, _combo(), _priced_on_every_leg(0.0))
    assert recorder.errors == [(3, 10058, "Combo per-leg prices are only supported for "
                                          "non-guaranteed smart combo with two legs and feature "
                                          "\"IECOMBOPERLEGPRICE\" enabled.")]
