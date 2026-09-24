"""A refused execution request still delivers its end.

A refused request that returned without the end left a caller waiting on it
waiting for good. Every request on this surface reports its refusal and still
ends — here one naming an account a login holding several does not hold.

Run: pytest tests/python/test_a_refused_execution_request_still_ends.py -v
"""

from ibkr_dx import EWrapper, EClient


class Recorder(EWrapper):
    def __init__(self):
        self.errors = []
        self.ended = []

    def error(self, req_id, error_time, code, message, advanced_order_reject_json=""):
        self.errors.append((req_id, code))

    def exec_details_end(self, req_id):
        self.ended.append(req_id)


class Filter:
    def __init__(self):
        self.client_id = 0
        self.acctCode = "U9"
        self.time = ""
        self.symbol = ""
        self.sec_type = ""
        self.exchange = ""
        self.side = ""
        self.lastNDays = 0
        self.specificDates = None


def test_a_refused_execution_request_still_ends():
    w = Recorder()
    c = EClient(w)
    c._test_connect("DU1", accounts=["DU1", "DU2"])
    c.req_executions(9, Filter())
    c._test_dispatch_once()
    assert w.errors == [(9, 321)], w.errors
    assert w.ended == [9], "the end still comes"
