"""Every request keeps its outcome behind the session's earlier records."""
import inspect

import ibkr_dx
import pytest


REQUESTS = sorted(
    name for name in dir(ibkr_dx.EClient)
    if name.startswith(("req_", "cancel_", "calculate_", "exercise_", "place_"))
    or name in ("request_fa", "replace_fa", "set_server_log_level", "update_config_proto_buf")
)


@pytest.mark.parametrize("name", REQUESTS)
def test_a_request_never_delivers_ahead_of_the_sessions_earlier_record(name):
    class Heard(ibkr_dx.EWrapper):
        def __init__(self):
            super().__init__()
            self.codes = []

        def error(self, req_id, time, code, message, advanced=""):
            self.codes.append(code)

    w = Heard()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU1")
    c._test_set_connection_lost()
    c._test_end_session()
    class ConfigRequest:
        reqId = 9

        def HasField(self, field):
            return field == "reqId"

    values = dict(
        config_request_proto=ConfigRequest(), update_config_request_proto=ConfigRequest(),
        req_id=9, order_id=9, perm_id=9, con_id=756733,
        contract=ibkr_dx.Contract(), order=ibkr_dx.Order(),
        subscription=ibkr_dx.ScannerSubscription(), scan=ibkr_dx.SpreadScan(),
        option_price=1.0, under_price=100.0, volatility=0.2,
        exercise_action=1, exercise_quantity=1, account="DU1", _override=0,
        fa_data_type=1, cxml="<List/>", group_name="All", tags="NetLiquidation",
        subscribe=True, model_code="", sec_type="STK", exchange="SMART",
        start_date="20260101", end_date="20260201", b_auto_bind=False,
        report_type="ReportsFinSummary", what_to_show="TRADES", use_rth=True,
        time_period="1 day", end_date_time="20260901-00:00:00", duration_str="1 D",
        bar_size_setting="1 min", provider_codes="BRFG", start_date_time="",
        total_results=1, market_data_type=1, market_rule_id=1, pattern="SPY",
        provider_code="BRFG", article_id="BRFG$1", underlying_symbol="SPY",
        fut_fop_exchange="", underlying_sec_type="STK", underlying_con_id=756733,
        bbo_exchange="a6", tick_type="Last",
    )
    method = getattr(c, name)
    args = [values[p.name] for p in inspect.signature(method).parameters.values()
            if p.default is inspect.Parameter.empty]
    method(*args)
    assert w.codes == [], name
    c.poll()
    assert w.codes[0] == 1100, (name, w.codes)
    c.disconnect()
