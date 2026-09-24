"""An account request a gateway refuses is refused here, in its words.

A gateway checks what an account request names before it asks the venue for
anything: a summary names a group and tags, a subscription to the account's
figures on a login holding several names one of them, and a profit request
names an account the login holds. Refused in its words, a program written
against it reads the same answer here.
"""

import ibkr_dx


class Errors(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.seen = []

    def error(self, req_id, error_time, code, msg, advanced_order_reject_json=""):
        self.seen.append((req_id, code, msg))


def _client(**kw):
    w = Errors()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU1", **kw)
    return w, c


def test_an_account_summary_names_a_group_and_tags():
    w, c = _client()
    c.reqAccountSummary(1, "", "NetLiquidation")
    c.reqAccountSummary(2, "All", "")
    c.reqAccountSummary(3, "Nope", "NetLiquidation")
    assert w.seen == [
        (1, 321, "Group name cannot be null"),
        (2, 321, "Tags cannot be null"),
        (3, 321, "Group name is invalid"),
    ]


def test_a_third_account_summary_is_refused_under_322():
    w, c = _client()
    for req_id in (1, 2, 3):
        c.reqAccountSummary(req_id, "All", "NetLiquidation")
    assert w.seen == [(3, 322, "Maximum number of account summary requests exceeded; "
                              "desubscribe to previous request first")]


def test_an_account_code_on_a_login_holding_several_names_one_of_them():
    w, c = _client(accounts=["DU1", "DU2"])
    c.reqAccountUpdates(True, "")
    c.req_account_updates(True, acct_code="U9")
    assert w.seen == [
        (-1, 321, "The account code is required for this operation."),
        (-1, 321, "Invalid account code 'U9'."),
    ]
    assert not any("RefreshAccount" in cmd for cmd in c._test_take_commands())


def test_an_account_code_on_a_login_holding_one_is_ignored():
    w, c = _client()
    c.req_account_updates(True, acct_code="X")
    assert w.seen == []
    assert any("RefreshAccount" in cmd for cmd in c._test_take_commands())


def test_a_profit_request_names_an_account_the_login_holds():
    w, c = _client()
    c.reqPnL(1, "", "")
    c.reqPnLSingle(2, "DU9", "", 265598)
    assert w.seen == [(1, 321, "Account must not be empty"), (2, 321, "Invalid account code")]


def test_every_account_on_a_login_holding_several_is_taken_and_said_to_be_this_ones():
    """A gateway takes `All` on a login holding several; answered here with the
    account this session opened under, the caller is told whose figures they
    are rather than handed one account's as every account's."""
    w, c = _client(accounts=["DU1", "DU2"])
    c.reqAccountUpdates(True, "All")
    c.reqAccountUpdates(True, "AllNonProp")
    c.reqAccountSummary(5, "All", "NetLiquidation")
    said = ("was named and the figures that follow are DU1's, which is the account "
            "this session opened under")
    assert w.seen == [
        (-1, 321, "All " + said),
        (-1, 321, "Invalid account code 'AllNonProp'."),
        (5, 321, "All " + said),
    ], w.seen
    assert sum("RefreshAccount" in cmd for cmd in c._test_take_commands()) == 1


def test_a_profit_request_is_refused_in_a_gateways_words():
    w, c = _client()
    c.reqPnL(1, " ", "")
    c.reqPnL(2, "All", "")
    assert w.seen == [(1, 321, "Account must not be empty"), (2, 321, "Invalid account code")]
