"""`dir()` lists the names the reference client gives what an object carries.

Those names are answered by the object's attribute hook rather than carried as
attributes, so they read and write but were missing from the listing: an editor
or a notebook completing from `dir()` offered `exec_id` and never `execId`, and
a program written against that client was offered none of the names it reads.

Run: pytest tests/python/test_a_listing_names_what_the_reference_client_reads.py -v
"""

from collections import Counter

import pytest
from ibkr_dx import (
    BarData,
    ContractDescription,
    ContractDetails,
    DepthMktDataDescription,
    EClient,
    EWrapper,
    Execution,
    OrderAllocation,
    OrderState,
    SmartComponent,
)


class Handed(EWrapper):
    def __init__(self):
        super().__init__()
        self.family = self.sessions = None

    def familyCodes(self, familyCodes):
        self.family = familyCodes

    def historicalSchedule(self, reqId, startDateTime, endDateTime, timeZone, sessions):
        self.sessions = sessions


def _handed():
    """The records a callback hands over, which are not the plain classes of
    the same names a program imports."""
    w = Handed()
    c = EClient(w)
    c._test_connect("T")
    c._test_set_family_codes("DU123", "Fam")
    c.reqFamilyCodes()
    c._test_push_historical_schedule(11, "20260907", "09:30:00", "16:00:00")
    c.poll()
    return w


class App(EWrapper, EClient):
    """The reference client's own sample is built on both bases."""

    def __init__(self):
        EClient.__init__(self, self)


# The reference client's names for what each carries: its fields, and for the
# client the two calls it opens with an `e`.
LISTED = [
    ("BarData", BarData, "barCount close date high low open volume wap"),
    ("ContractDescription", ContractDescription, "contract derivativeSecTypes"),
    ("ContractDetails", ContractDetails, """
        aggGroup bondType callable category contract contractMonth
        convertible coupon couponType cusip descAppend evMultiplier
        evRule eventContract1 eventContractDescription1
        eventContractDescription2 fundAssetType fundBackLoad
        fundBackLoadTimeInterval fundBlueSkyStates
        fundBlueSkyTerritories fundClosed fundClosedForNewInvestors
        fundClosedForNewMoney fundDistributionPolicyIndicator fundFamily
        fundFrontLoad fundManagementFee fundMinimumInitialPurchase
        fundName fundNotifyAmount fundSubsequentMinimumPurchase fundType
        industry ineligibilityReasonList issueDate lastPricePrecision
        lastSizePrecision lastTradeTime liquidHours longName marketName
        marketRuleIds maturity minAlgoSize minSize minTick
        nextOptionDate nextOptionPartial nextOptionType notes orderTypes
        priceMagnifier putable ratings realExpirationDate secIdList
        settlementMethod sizeIncrement stockType subcategory
        suggestedSizeIncrement timeZoneId tradingHours underConId
        underSecType underSymbol validExchanges
    """),
    ("DepthMktDataDescription", DepthMktDataDescription,
     "aggGroup exchange listingExch secType serviceDataType"),
    ("Execution", Execution, """
        acctNumber avgPrice clientId cumQty evMultiplier evRule exchange
        execId lastLiquidity liquidation modelCode
        optExerciseOrLapseType orderId orderRef pendingPriceRevision
        permId price shares side submitter time
    """),
    ("FamilyCode", lambda: _handed().family[0], "accountID familyCodeStr"),
    ("HistoricalSession", lambda: _handed().sessions[0], "endDateTime refDate startDateTime"),
    ("OrderAllocation", OrderAllocation, """
        account allowedAllocQty desiredAllocQty isMonetary position
        positionAfter positionDesired
    """),
    ("OrderState", OrderState, """
        commissionAndFees commissionAndFeesCurrency completedStatus
        completedTime equityWithLoanAfter equityWithLoanAfterOutsideRTH
        equityWithLoanBefore equityWithLoanBeforeOutsideRTH
        equityWithLoanChange equityWithLoanChangeOutsideRTH
        initMarginAfter initMarginAfterOutsideRTH initMarginBefore
        initMarginBeforeOutsideRTH initMarginChange
        initMarginChangeOutsideRTH maintMarginAfter
        maintMarginAfterOutsideRTH maintMarginBefore
        maintMarginBeforeOutsideRTH maintMarginChange
        maintMarginChangeOutsideRTH marginCurrency maxCommissionAndFees
        minCommissionAndFees orderAllocations rejectReason status
        suggestedSize warningText
    """),
    ("SmartComponent", SmartComponent, "bitNumber exchange exchangeLetter"),
    ("EClient", lambda: EClient(EWrapper()), "eConnect eDisconnect"),
    ("App(EWrapper, EClient)", App, "eConnect eDisconnect"),
]


@pytest.mark.parametrize("name, make, theirs", LISTED, ids=[name for name, _, _ in LISTED])
def test_a_listing_names_what_the_reference_client_reads(name, make, theirs):
    made = make()
    listed = dir(made)
    missing = [n for n in theirs.split() if n not in listed]
    assert not missing, f"{name} answers to these and does not list them: {missing}"
    # And lists nothing it does not answer to.
    unanswered = [n for n in listed if not n.startswith("_") and not hasattr(made, n)]
    assert not unanswered, f"{name} lists these and does not answer to them: {unanswered}"
    # And lists each name under one spelling of its words: the reference
    # client's `accountID`, not `accountId` beside it.
    capitalised = [n for n in listed if not n.startswith("_") and n != n.lower()]
    spellings = Counter(n.replace("_", "").lower() for n in capitalised)
    twice = sorted(n for n in capitalised if spellings[n.replace("_", "").lower()] > 1)
    assert not twice, f"{name} lists these under more than one spelling: {twice}"
