"""A contract-details request carries what the caller's contract states.

`includeExpired` is part of the contract a caller hands `reqContractDetails`,
and a gateway states it on every lookup by description. The request had no
field for it, so a caller asking for a future that had already expired was
answered as though it had not.
"""

import ibkr_dx


def _asked(**stated):
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("DU1")
    c.reqContractDetails(1, ibkr_dx.Contract(symbol="ES", sec_type="FUT", exchange="CME", **stated))
    sent = [cmd for cmd in c._test_take_commands() if "FetchContractDetails" in cmd]
    assert len(sent) == 1, sent
    return sent[0]


def test_an_expired_contract_is_in_scope_where_the_contract_says_so():
    assert "include_expired: true" in _asked(include_expired=True)
    assert "include_expired: false" in _asked()
