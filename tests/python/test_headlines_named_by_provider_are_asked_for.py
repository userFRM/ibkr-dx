"""A tick list naming the providers of a contract's headlines asks them.

`292:BRFG+DJNL` is how the reference client names the providers to ask for a
contract's headlines. Only a bare `292` was read here: the named form asked
for no headlines, left a contract given by description unnamed, and was
logged as a series the venue does not know.
"""

import ibkr_dx


def test_a_description_asking_for_headlines_by_provider_asks_them():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU0000000")
    described = ibkr_dx.Contract()
    described.symbol = "AAPL"
    described.secType = "STK"
    described.exchange = "SMART"
    described.currency = "USD"

    c.req_mkt_data(2, described, "mdoff,292:BRFG+DJNL", False, False, [])

    # The providers ride the request to the engine, which names the contract
    # and asks the venue for its headlines from them.
    sent = c._test_take_commands()
    asked = [cmd for cmd in sent if cmd.startswith("Subscribe")]
    assert len(asked) == 1 and 'news: Some("BRFG*DJNL")' in asked[0], sent
