"""A quote in a slot past the size the tables are made with reaches its caller.

The slot tables were made once at four thousand and ninety-six contracts and
never grew, so a session holding more — orders, fills and holdings each take a
slot, and nothing bounds how many a session has — was refused the next contract
while the venue would have served it. They grow now, and a caller reads a slot
past that size as it reads any other.
"""

from ibkr_dx import EClient, EWrapper


class _Quotes(EWrapper):
    def __init__(self):
        super().__init__()
        self.prices = []

    def tick_price(self, req_id, tick_type, price, attrib=None):
        self.prices.append((req_id, tick_type, price))


def test_a_quote_past_the_first_four_thousand_slots_reaches_its_caller():
    w = _Quotes()
    c = EClient(w)
    c._test_connect("DU000000", False)
    slot = 5000
    c._test_set_instrument_count(slot + 1)
    # Handed out and not yet ticked, it reads as any slot below that size does:
    # the empty quote, not the answer for a slot never handed out.
    held = c.quoteByInstrument(slot)
    assert held is not None and held["bid"] == 0.0, held
    c._test_map_con_id(756733, slot)
    c._test_map_instrument(900, slot)
    c._test_push_quote(slot, 100.0, 101.0, 100.5, 1, 1, 1, 10, 100.0, 101.0, 99.0, 100.0)
    c._test_dispatch_once()
    assert (900, 1, 100.0) in w.prices, w.prices
