"""Each change of the connection is said once, in the order it happened.

The engine pushes a record as the connection goes and another as it comes
back. A read says each in its place: 1100 for the loss, 1102 for the
recovery. Nothing is reconciled from flags at the read, so a notice cannot be
lost to a full channel, said twice, or collapsed with the one after it.
`isConnected()` reads true throughout, as a program connected to a gateway
reads it: the gateway's socket stays up while its connection to the venue is
down.
"""

import ibkr_dx


class Notices(ibkr_dx.EWrapper):
    def __init__(self):
        super().__init__()
        self.codes = []

    def error(self, reqId, errorTime, errorCode, errorString, advancedOrderRejectJson=""):
        self.codes.append(errorCode)


def _connected():
    w = Notices()
    c = ibkr_dx.EClient(w)
    c._test_connect("T")
    return w, c


def test_a_loss_is_announced():
    w, c = _connected()
    c._test_set_connection_lost()
    c.poll()
    assert w.codes == [1100], f"the loss left the caller uninformed: {w.codes}"
    assert c.isConnected(), "a gateway's socket stays up through the outage"
    assert c.conn_state == c.CONNECTED
    assert not c.session_over()


def test_a_restore_is_announced():
    w, c = _connected()
    c._test_set_connection_lost()
    c.poll()
    assert w.codes == [1100], w.codes

    c._test_set_connection_restored()
    c.poll()
    assert w.codes == [1100, 1102], f"the restore left the caller down: {w.codes}"
    assert c.isConnected()


def test_an_outage_one_pass_spans_is_the_loss_then_the_restore():
    """A pass slow enough to span the whole outage says both, in order.

    The loss is its own record, so the restore that follows it in the same
    pass is a recovery from a loss the caller is told about first.
    """
    w, c = _connected()
    assert c.isConnected()

    c._test_set_connection_lost()
    c._test_set_connection_restored()
    c.poll()

    assert w.codes == [1100, 1102], w.codes
    assert c.isConnected(), "and the session reads as up, which it is"
