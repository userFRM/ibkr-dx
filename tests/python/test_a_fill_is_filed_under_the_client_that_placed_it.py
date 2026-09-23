"""A fill's client is the one that placed the order where the report names none.

One pass announced orderStatus under the placing client and filed the same print
under client zero, so a caller replaying its own fills by client id got none of
them.
"""
import ibkr_dx


class Filed(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.clients = []

    def execDetails(self, reqId, contract, execution):
        self.clients.append(execution.clientId)

    def error(self, *a):
        pass


def test_a_report_naming_no_client_files_the_fill_under_the_placing_client():
    w = Filed()
    c = ibkr_dx.EClient(w)
    c._test_connect("T")
    c._test_set_client_id(5)
    c._test_track_order(86, 0, "SPY", "BUY", 1, 100.0)
    c._test_push_venue_order(86, "SPY", "BUY", 1, 100.0)
    c._test_push_fill(0, 86, "BUY", 100.0, 1, 0)
    c._test_dispatch_once()
    assert w.clients == [5], f"filed under the client that placed it: {w.clients}"


def test_a_fill_replayed_names_the_client_that_placed_it():
    """Stored without the client or the order's permanent number, a request
    filtered by client matched nothing and the replay reported both as zero."""
    seen = []

    class Fills(ibkr_dx.EWrapper):
        def execDetails(self, reqId, contract, execution):
            seen.append(execution)

        def error(self, *a):
            pass

    class Filter:
        def __init__(self):
            self.symbol = self.secType = self.exchange = self.side = ""
            self.acctCode = self.time = ""
            self.clientId = 3

    w = Fills()
    c = ibkr_dx.EClient(w)
    c._test_connect("DU1")
    c._test_set_client_id(3)
    c._test_track_order(7, 1, "SPY", "BUY", 5.0, 10.0, 0)
    c._test_push_fill(1, 7, "BUY", 10.0, 5, 0, 1.25)
    c._test_dispatch_once()
    assert seen, "the fill reached nothing live"
    assert seen[0].clientId == 3

    seen.clear()
    c.req_executions(9, Filter())
    c._test_dispatch_once()
    assert seen, "a request filtered by the client that placed it matched nothing"
    assert seen[0].clientId == 3
