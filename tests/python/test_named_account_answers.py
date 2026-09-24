"""Account requests keep the account they name throughout the answer."""
import ibkr_dx


class Heard(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.errors = []
        self.figures = []
        self.holdings = []
        self.profits = []
        self.ends = []

    def error(self, req_id, time, code, message, advanced=""):
        self.errors.append((req_id, code, message))

    def accountSummary(self, req_id, account, key, value, currency):
        self.figures.append(("summary", req_id, account, key, value))

    def accountSummaryEnd(self, req_id):
        self.ends.append(("summary", req_id))

    def accountUpdateMulti(self, req_id, account, model, key, value, currency):
        self.figures.append(("multi", req_id, account, key, value))
        assert model == ""

    def updateAccountValue(self, key, value, currency, account):
        self.figures.append(("updates", 0, account, key, value))

    def positionMulti(self, req_id, account, model, contract, position, avg_cost):
        self.holdings.append((req_id, account, position))
        assert model == ""

    def pnl(self, req_id, daily, unrealized, realized):
        self.profits.append((req_id, daily))


def client():
    heard = Heard()
    c = ibkr_dx.EClient(heard)
    c._test_connect("DU1", accounts=["DU1", "DU2"])
    for account, net, daily, position in [("DU1", 100, 10, 3), ("DU2", 200, 20, 7)]:
        c._test_set_account(net_liquidation=net, daily_pnl=daily, account=account)
        c._test_set_position(756733, position, 30, account=account)
        c._test_finish_account_download(account)
    return c, heard


def test_named_figures_and_holdings_arrive_under_the_account_requested():
    c, heard = client()
    c.reqAccountUpdates(True, "DU2")
    c.reqAccountUpdatesMulti(2, "DU2", "", False)
    c.reqPositionsMulti(3, "DU2", "")
    c.reqPositionsMulti(4, "DU1", "")
    c.poll()
    assert ("updates", 0, "DU2", "NetLiquidation", "200.00") in heard.figures
    assert ("multi", 2, "DU2", "NetLiquidation", "200.00") in heard.figures
    assert (3, "DU2", 7.0) in heard.holdings
    assert (4, "DU1", 3.0) in heard.holdings
    assert not heard.errors
    heard.holdings.clear()
    c._test_set_position(756733, 9, 30, account="DU2")
    c.poll()
    assert heard.holdings == [(3, "DU2", 9.0)]
    c.disconnect()


def test_all_summary_and_concurrent_profit_requests_keep_accounts_separate():
    c, heard = client()
    c.reqAccountSummary(1, "All", "NetLiquidation")
    c.reqPnL(2, "DU1", "")
    c.reqPnL(3, "DU2", "")
    c.reqPnL(4, "DU1", "")
    c.poll()
    assert sorted(heard.figures) == [
        ("summary", 1, "DU1", "NetLiquidation", "100.00"),
        ("summary", 1, "DU2", "NetLiquidation", "200.00"),
    ]
    assert heard.ends == [("summary", 1)]
    assert sorted(heard.profits) == [(2, 10.0), (3, 20.0), (4, 10.0)]
    assert not heard.errors
    c.reqPnL(2, "DU2", "")
    c.poll()
    assert [(req_id, code) for req_id, code, _ in heard.errors] == [(2, 102)]
    c.cancelPnL(2)
    heard.profits.clear()
    c._test_set_account(daily_pnl=30, account="DU1")
    c.poll()
    assert heard.profits == [(4, 30.0)]
    c.disconnect()


def test_an_accounts_retirement_discards_the_values_already_polled():
    c, heard = client()
    c.reqAccountUpdates(True, "DU2")
    c.poll()
    assert heard.figures
    heard.figures.clear()
    c._test_set_account(net_liquidation=300, account="DU2")
    c.reqAccountUpdates(False, "DU2")
    c.poll()
    assert heard.figures == []
    c.disconnect()


def test_each_single_position_profit_keeps_its_account_and_request():
    c, heard = client()
    updates = []
    heard.pnlSingle = lambda req_id, position, daily, unrealized, realized, value: updates.append((req_id, position, daily))
    c._test_map_con_id(756733, 0)
    c._test_push_quote(0, last=40)
    c.reqPnLSingle(1, "DU1", "", 756733)
    c.reqPnLSingle(2, "DU2", "", 756733)
    c.poll()
    assert sorted(updates) == [(1, 3.0, 30.0), (2, 7.0, 70.0)]
    c.reqPnLSingle(1, "DU2", "", 756733)
    c.poll()
    assert [(req_id, code) for req_id, code, _ in heard.errors] == [(1, 102)]
    c.cancelPnLSingle(1)
    c.reqPnLSingle(1, "DU2", "", 756733)
    updates.clear()
    c.poll()
    assert updates == [(1, 7.0, 70.0)]
    c.disconnect()


def test_a_repeated_positions_number_keeps_each_answers_account():
    c, heard = client()
    c.reqPositionsMulti(1, "DU1", "")
    c.reqPositionsMulti(1, "DU2", "")
    c.poll()
    assert heard.holdings == [(1, "DU1", 3.0), (1, "DU2", 7.0)]
    c.disconnect()


def test_unapplied_account_selections_keep_the_existing_answer():
    c, heard = client()
    c.reqPnL(1, "All", "")
    c.reqPositionsMulti(2, "AllNonProp", "")
    c.reqAccountUpdatesMulti(3, "AllNonProp", "", False)
    c.poll()
    assert heard.profits == [(1, 10.0)]
    assert heard.holdings == [(2, "DU1", 3.0)]
    assert ("multi", 3, "DU1", "NetLiquidation", "100.00") in heard.figures
    assert not heard.errors
    c.disconnect()


def test_an_exercise_checks_the_named_holding_and_waits_for_its_figure():
    c, heard = client()
    contract = ibkr_dx.Contract()
    contract.conId = 756733
    contract.symbol = "SPY"
    contract.secType = "OPT"
    contract.exchange = "SMART"
    contract.currency = "USD"
    contract.lastTradeDateOrContractMonth = "20261218"
    contract.strike = 600
    contract.right = "C"
    contract.multiplier = "100"
    c.exerciseOptions(1, contract, 1, 9, "DU2", 0)
    assert c._test_take_commands() == []
    c._test_push_stated_figures(0, 493, [-0.5, 1.0, 0.2])
    c.poll()
    assert [(req_id, code) for req_id, code, _ in heard.errors] == [(1, 322)]
    c.disconnect()
    c, heard = client()
    c.exerciseOptions(2, contract, 1, 9, "DU2", 1)
    assert c._test_take_commands() == []
    c._test_push_stated_figures(0, 493, [-0.5, 1.0, 0.2])
    commands = c._test_take_commands()
    assert len(commands) == 1
    assert "exercise_action: 1" in commands[0]
    assert "qty: 700000000" in commands[0]
    assert 'account: "DU2"' in commands[0]
    c.disconnect()


def test_multi_answers_echo_the_model_label_on_initial_and_later_rows():
    c, heard = client()
    labels = []
    heard.positionMulti = lambda req_id, account, model, *rest: labels.append((req_id, model))
    heard.accountUpdateMulti = lambda req_id, account, model, *rest: labels.append((req_id, model))
    c.reqPositionsMulti(1, "DU2", "M1")
    c.reqAccountUpdatesMulti(2, "DU2", "M2", False)
    c.poll()
    assert (1, "M1") in labels and (2, "M2") in labels
    labels.clear()
    c._test_set_position(756733, 9, 30, account="DU2")
    c._test_set_account(net_liquidation=210, account="DU2")
    c.poll()
    assert (1, "M1") in labels and (2, "M2") in labels
    assert all(model == {1: "M1", 2: "M2"}[req_id] for req_id, model in labels)
    c.disconnect()
