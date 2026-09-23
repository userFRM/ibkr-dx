"""A tick list naming the providers of a contract's headlines asks them.

`292:BRFG+DJNL` is how the reference client names the providers to ask for a
contract's headlines. Only a bare `292` was read here: the named form asked
for no headlines, left a contract given by description unnamed, and was
logged as a series the venue does not know.
"""

import ibkr_dx


def test_a_description_asking_for_headlines_by_provider_is_named_and_asked_for():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU0000000")
    # The venue's answer when the description is named, and the quotes already
    # up under another request, so the new one joins them.
    asked = c._test_peek_ask_id()
    c._test_push_contract_details(asked, 265598, "AAPL")
    c._test_push_contract_details_end(asked)
    c._test_map_con_id(265598, 3)
    c._test_map_instrument(1, 3)
    described = ibkr_dx.Contract()
    described.symbol = "AAPL"
    described.secType = "STK"
    described.exchange = "SMART"
    described.currency = "USD"

    c.req_mkt_data(2, described, "mdoff,292:BRFG+DJNL", False, False, [])

    sent = c._test_take_commands()
    assert any(cmd.startswith("FetchContractDetails") for cmd in sent), sent
    news = [cmd for cmd in sent if cmd.startswith("SubscribeNews")]
    assert len(news) == 1 and 'providers: "BRFG*DJNL"' in news[0], sent
