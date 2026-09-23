"""The settings a gateway keeps in its own configuration file."""

import pytest

import ibkr_dx


def test_a_setting_can_be_set_and_read_back():
    ibkr_dx.configure(timezone="America/New_York")
    assert ibkr_dx.settings()["timezone"] == "America/New_York"
    ibkr_dx.configure(timezone=None)
    assert ibkr_dx.settings()["timezone"] is None


def test_a_misspelled_setting_is_refused_not_dropped():
    """Silently ignoring it leaves a caller believing a session is configured
    a way it is not."""
    with pytest.raises(ValueError, match="no such setting"):
        ibkr_dx.configure(timezoen="UTC")


def test_every_setting_names_the_gateway_setting_it_stands_in_for():
    text = ibkr_dx.describe()
    for name in ibkr_dx.settings():
        assert name in text


def test_a_gateway_setting_with_no_counterpart_says_so_rather_than_vanishing():
    """Someone migrating will look for these."""
    assert "TrustedIPs" in ibkr_dx.UNAVAILABLE
    assert "LocalServerPort" in ibkr_dx.UNAVAILABLE
    assert "readonly" in ibkr_dx.UNAVAILABLE["ApiOnly"]


#: Every setting a gateway carries, less the ones that only move a window
#: about, and the name this client answers each by. `None` means the answer is
#: recorded under the gateway's spelling.
#:
#: A caller migrating looks a name up and expects an answer, so the names have
#: to be written down somewhere or the claim is only a claim. Several settings
#: spell the same thing differently here, and the seven debug switches are one
#: log level.
_A_GATEWAY_CARRIES = {
    "ApiOnly": None,
    "LocalApiPort": None,
    "LocalServerPort": None,
    "Local_Port": "LocalApiPort",
    "TrustedIPs": None,
    "Local_FIX_Server_Settings": None,
    "useSsl": None,
    "UseSSL": None,
    "reconnectOnSocketErr": "reconnect_on_socket_err",
    "RemoteHostOrderRouting": None,
    "RemotePortOrderRouting": None,
    "Select_account_type": None,
    "Verbose_logging": "log_level",
    "Market_Data_Connection": "market_data_host",
    "Debug": "log_level",
    "Error": "log_level",
    "Internal": "log_level",
    "WriteDebug": "log_level",
    "OutOfBandDebug": "log_level",
    "ServiceDebug": "log_level",
    "ssoDebug": "log_level",
}


def test_every_setting_a_gateway_carries_is_answered_here():
    """A name a gateway carries is either a setting here or a stated reason.

    Silence is the one answer that leaves a caller migrating with nowhere to
    go: they cannot tell a setting this client does not have from one it
    spells differently.
    """
    answered = set(ibkr_dx.settings()) | set(ibkr_dx.UNAVAILABLE)
    unanswered = sorted(
        name for name, here in _A_GATEWAY_CARRIES.items() if (here or name) not in answered
    )
    assert not unanswered, f"a gateway carries these and this client answers neither way: {unanswered}"


def test_a_setting_reaches_the_variable_the_client_reads_it_from():
    """A setting that reads back but changes nothing is decoration.

    This is the half of that a process can check on its own: the value lands
    where a session opening will look for it. What each one then does on the
    wire — the host a farm connection opens on, the fields a logon announces,
    the machine identity presented — is checked against the messages
    themselves, beside the code that composes them.
    """
    import os

    ibkr_dx.configure(market_data_host="example.invalid")
    assert os.environ["IBKR_DX_FARM_HOST"] == "example.invalid"
    ibkr_dx.configure(market_data_host=None)


def test_a_logging_setting_says_it_cannot_be_set_rather_than_storing_one():
    """A process has one logger and importing this client installs it.

    Stored, the value reads back as a log level that was set and did nothing.
    Both ways of stating a setting refuse it, and both name the one place it
    is read from.
    """
    before = ibkr_dx.settings()["log_level"]
    with pytest.raises(ValueError, match="IBKR_DX_LOG_LEVEL"):
        ibkr_dx.configure(log_level="debug")
    assert ibkr_dx.settings()["log_level"] == before, "and nothing was stored"

    # The same refusal on the other way in, so a caller does not find one door
    # open and the other shut.
    client = ibkr_dx.EClient(ibkr_dx.EWrapper())
    with pytest.raises(RuntimeError, match="IBKR_DX_LOG_DIR"):
        client.connect(username="u", password="p", settings={"log_dir": "/tmp/ibkr_dx"})


def test_a_setting_stated_beside_a_logging_one_is_not_half_applied():
    """The refusal comes before anything is stored, so a call that names both
    a logging setting and an ordinary one leaves neither set."""
    before = ibkr_dx.settings()["timezone"]
    with pytest.raises(ValueError):
        ibkr_dx.configure(timezone="America/New_York", log_queue=4096)
    assert ibkr_dx.settings()["timezone"] == before
