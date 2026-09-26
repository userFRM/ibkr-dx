"""A head timestamp is written the way the caller asked, as the other surface
writes it: the form goes with the request, and its answer is written in it.
Handed back as the venue wrote it, a caller asking for seconds since the epoch
read a date string."""
import ibkr_dx


def spy():
    c = ibkr_dx.Contract()
    c.conId = 756733
    c.symbol = "SPY"
    c.secType = "STK"
    c.exchange = "SMART"
    c.currency = "USD"
    return c


def test_a_head_timestamp_is_written_as_asked():
    c = ibkr_dx.EClient(ibkr_dx.EWrapper())
    c._test_connect("T")
    c._test_map_con_id(756733, 0)
    c.reqHeadTimeStamp(7, spy(), "TRADES", 1, 2)
    sent = [cmd for cmd in c._test_take_commands() if cmd.startswith("FetchHeadTimestamp")]
    assert len(sent) == 1 and "format_date: 2" in sent[0], sent
