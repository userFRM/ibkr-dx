"""A program written against the reference client imports these on line one.

Its samples fill in a plain object and hand it back — an execution filter, a
scanner subscription, a combination leg — name the constant an unset field
carries, and annotate every callback with an alias. All of that is evaluated
before the program does anything, so a name absent here is an ImportError or a
NameError at class-definition time and nothing runs at all.

These are read by attribute on the way to the venue, so the shape is the whole
contract: an object of any type carrying the same attribute names is already
accepted, and these are what a caller reaches for when they have no reason to
write their own.
"""

import ibkr_dx


def test_the_objects_a_caller_fills_in_exist_and_are_read():
    execution_filter = ibkr_dx.ExecutionFilter()
    execution_filter.acctCode = "DU1"
    execution_filter.side = "BUY"
    assert execution_filter.clientId == 0
    assert execution_filter.symbol == ""

    scan = ibkr_dx.ScannerSubscription()
    scan.instrument = "STK"
    scan.locationCode = "STK.US.MAJOR"
    scan.scanCode = "TOP_PERC_GAIN"
    # An unset bound is the largest number there is, which is what tells this
    # client not to send it. Sent, it would empty the scan rather than widen it.
    assert scan.abovePrice == ibkr_dx.UNSET_DOUBLE
    assert scan.aboveVolume == ibkr_dx.UNSET_INTEGER
    assert scan.numberOfRows == -1

    leg = ibkr_dx.ComboLeg()
    leg.conId = 265598
    leg.ratio = 1
    leg.action = "BUY"
    leg.exchange = "SMART"
    assert leg.exemptCode == -1, "the reference leaves it at minus one, not nought"

    hedge = ibkr_dx.DeltaNeutralContract()
    hedge.conId = 756733
    hedge.delta = 0.5
    hedge.price = 100.0

    ibkr_dx.OrderComboLeg()
    ibkr_dx.OrderCancel()
    ibkr_dx.WshEventData()


def test_a_filter_a_caller_filled_in_reaches_the_request():
    # The point of the object: this client reads it by attribute, so what the
    # caller set is what the request asks for.
    client = ibkr_dx.EClient(ibkr_dx.EWrapper())
    execution_filter = ibkr_dx.ExecutionFilter()
    execution_filter.symbol = "SPY"
    execution_filter.side = "BUY"
    client.reqExecutions(1, execution_filter)

    scan = ibkr_dx.ScannerSubscription()
    scan.instrument = "STK"
    scan.locationCode = "STK.US.MAJOR"
    scan.scanCode = "TOP_PERC_GAIN"
    client.reqScannerSubscription(2, scan, [], [])


def test_the_constants_an_unset_field_carries():
    assert ibkr_dx.UNSET_INTEGER == 2**31 - 1
    assert ibkr_dx.UNSET_LONG == 2**63 - 1
    assert ibkr_dx.UNSET_DOUBLE == __import__("sys").float_info.max
    assert str(ibkr_dx.UNSET_DECIMAL) == str(2**127 - 1)
    assert ibkr_dx.DOUBLE_INFINITY == float("inf")
    assert ibkr_dx.INFINITY_STR == "Infinity"
    assert ibkr_dx.NO_VALID_ID == -1
    assert ibkr_dx.MAX_MSG_LEN == 0xFFFFFF


def test_the_aliases_a_callback_annotation_names():
    # Evaluated when the class body runs, so a program with annotated
    # overrides — which the reference's own sample has — needs them present
    # before it has done anything.
    assert (ibkr_dx.TickerId, ibkr_dx.OrderId, ibkr_dx.TickType) == (int, int, int)
    assert ibkr_dx.TagValueList is list
    assert ibkr_dx.SetOfString is set and ibkr_dx.SetOfFloat is set
    assert ibkr_dx.SmartComponentMap is dict
    assert ibkr_dx.ListOfContractDescription is list
    assert ibkr_dx.ListOfOrder is list
    assert ibkr_dx.HistogramDataList is list


def test_the_marker_every_override_carries():
    # It marks and does nothing else; a program will not import without it.
    @ibkr_dx.iswrapper
    def answered(a, b):
        return a + b

    assert answered(1, 2) == 3


def test_a_figure_is_written_for_a_person_and_an_unset_one_is_not():
    assert ibkr_dx.intMaxString(7) == "7"
    assert ibkr_dx.intMaxString(ibkr_dx.UNSET_INTEGER) == ""
    assert ibkr_dx.floatMaxString(1.5) == "1.5"
    assert ibkr_dx.floatMaxString(ibkr_dx.UNSET_DOUBLE) == ""
    assert ibkr_dx.longMaxString(ibkr_dx.UNSET_LONG) == ""
    assert ibkr_dx.decimalMaxString(ibkr_dx.UNSET_DECIMAL) == ""


def test_the_account_figures_are_named():
    tags = ibkr_dx.AccountSummaryTags
    assert tags.NetLiquidation == "NetLiquidation"
    assert tags.SMA == "SMA"
    named = tags.AllTags.split(",")
    assert "NetLiquidation" in named and "DayTradesRemaining" in named
    assert len(named) == len(set(named)), "each figure named once"


def test_the_numbered_kinds_a_program_names():
    # A program asks for a feed and an advisor document by name, and compares
    # a condition's kind against these.
    assert ibkr_dx.MarketDataTypeEnum.REALTIME == 1
    assert ibkr_dx.MarketDataTypeEnum.DELAYED == 3
    assert ibkr_dx.MarketDataTypeEnum.DELAYED_FROZEN == 4
    assert ibkr_dx.FaDataTypeEnum.GROUPS == 1
    assert ibkr_dx.FaDataTypeEnum.ALIASES == 3
    assert ibkr_dx.OrderCondition.Price == 1
    assert ibkr_dx.OrderCondition.PercentChange == 7
    assert ibkr_dx.getEnumTypeName(ibkr_dx.MarketDataTypeEnum, 3) == "DELAYED"


def test_the_base_a_program_writes_its_own_objects_on():
    # Evaluated as a base class, so it has to exist before that class body runs.
    class Activity(ibkr_dx.Object):
        pass

    assert str(Activity()) == "Activity"


def test_a_scan_row_and_a_mid_offset_that_means_the_midpoint():
    row = ibkr_dx.ScanData(rank=1, distance="", benchmark="", projection="", legsStr="")
    assert row.rank == 1 and row.contract is None
    # Not a distance: the offset that says "up to the midpoint" is unbounded.
    assert ibkr_dx.COMPETE_AGAINST_BEST_OFFSET_UP_TO_MID == float("inf")


def test_a_clock_reading_written_for_a_person():
    assert ibkr_dx.getTimeStrFromMillis(0) == ""
    assert ibkr_dx.getTimeStrFromMillis(1772202600000).endswith(".000")
