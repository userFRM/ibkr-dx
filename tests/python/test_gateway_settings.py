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


def test_only_settings_a_gateway_has_are_named():
    """A name a gateway does not carry, recorded as one of its settings, sends
    a caller migrating to look for something that was never there. A gateway's
    pacing has one switch, and its timestamps one setting; both are named by
    what a gateway calls them."""
    for invented in ("ApiMsgsPerSlice", "ApiTimeSliceMillis", "TimestampZone"):
        assert invented not in ibkr_dx.UNAVAILABLE
    assert "rejectMessagesAboveMaxRate" in ibkr_dx.UNAVAILABLE
    assert "sendInstrumentTimezone" in ibkr_dx.UNAVAILABLE


def test_a_setting_with_a_counterpart_leads_with_it():
    for name, counterpart in (("ApiOnly", "`readonly`"), ("RemoteHostOrderRouting", "`host`"),
                              ("Select_account_type", "`paper`")):
        assert ibkr_dx.UNAVAILABLE[name].startswith(counterpart), ibkr_dx.UNAVAILABLE[name]


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

    Where it writes is fixed once it runs, so a directory stated afterwards
    reads back as one that was set and did nothing. Both ways of stating it
    refuse it, and both name the one place it is read from.
    """
    before = ibkr_dx.settings()["log_dir"]
    with pytest.raises(ValueError, match="IBKR_DX_LOG_DIR"):
        ibkr_dx.configure(log_dir="/tmp/ibkr_dx")
    assert ibkr_dx.settings()["log_dir"] == before, "and nothing was stored"

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


def _run(code, **env):
    """Run `code` in a process of its own: the logger is installed once, at
    import, and a test that moved it here would move it for the whole suite."""
    import os
    import subprocess
    import sys

    clean = {k: v for k, v in os.environ.items()
             if not k.startswith("IBKR_DX_LOG") and k != "RUST_LOG"}
    return subprocess.run([sys.executable, "-c", code], env={**clean, **env},
                          capture_output=True, text=True, timeout=60)


def test_a_log_level_moves_the_logger_this_client_installed():
    """The logger runs at warn until told otherwise; `configure` moves it, and
    the line it logs at info on the way is the proof it moved."""
    done = _run("import ibkr_dx; ibkr_dx.configure(log_level='info'); "
                "print(ibkr_dx.settings()['log_level'])")
    assert done.returncode == 0, done.stderr
    assert done.stdout.strip() == "info"
    assert "logging at info" in done.stderr, done.stderr


def test_a_log_level_that_is_not_one_is_refused():
    done = _run("import ibkr_dx\n"
                "try:\n    ibkr_dx.configure(log_level='info=loud')\n"
                "except ValueError as e:\n    print('refused', e)\n"
                "print(ibkr_dx.settings()['log_level'])")
    assert done.returncode == 0, done.stderr
    assert done.stdout.splitlines() == [
        "refused log_level: info=loud is not a level this logger reads", "None"], done.stdout


def test_the_log_level_in_the_environment_is_read_at_import():
    """`IBKR_DX_LOG_LEVEL` is read as the logger is installed, with or without
    a log directory beside it. Read only beside one, the ordinary logger to
    stderr stayed at warn whatever it said."""
    code = ("import ibkr_dx\n"
            "c = ibkr_dx.EClient(ibkr_dx.EWrapper())\n"
            "c._test_connect()\n"
            "f = ibkr_dx.ExecutionFilter()\n"
            "f.specificDates = [20000101]\n"
            "c.reqExecutions(1, f)\n"
            "c.poll()\n")
    said = "The dates: [2000-01-01] are outside"
    at_info = _run(code, IBKR_DX_LOG_LEVEL="info")
    assert at_info.returncode == 0, at_info.stderr
    assert said in at_info.stderr, at_info.stderr
    at_default = _run(code)
    assert said not in at_default.stderr, "the line is an info line: " + at_default.stderr


def test_a_connect_refused_for_a_setting_leaves_the_level_where_it_was():
    """The level is moved once every other setting has been read. Moved as the
    map was read, a connect refused for a later setting had already moved the
    process's logger, in whatever order the map was read that time."""
    done = _run("import ibkr_dx\n"
                "for _ in range(8):\n"
                "    c = ibkr_dx.EClient(ibkr_dx.EWrapper())\n"
                "    try:\n"
                "        c.connect(settings={'log_level': 'info', 'port': 'abc'})\n"
                "    except RuntimeError as e:\n"
                "        print('refused', e)\n")
    assert done.returncode == 0, done.stderr
    assert done.stdout.splitlines() == ["refused port: abc"] * 8, done.stdout
    assert "logging at info" not in done.stderr, done.stderr


def test_an_unset_log_level_is_the_level_the_logger_was_installed_at():
    """Empty is unset, as a session reads it, and unset puts the logger back
    where importing this client put it rather than leaving it where it was
    last moved. An info line is the probe: said at info, not at warn."""
    done = _run("import ibkr_dx\n"
                "c = ibkr_dx.EClient(ibkr_dx.EWrapper())\n"
                "c._test_connect()\n"
                "f = ibkr_dx.ExecutionFilter()\n"
                "f.specificDates = [20000101]\n"
                "ibkr_dx.configure(log_level='info')\n"
                "c.reqExecutions(1, f)\n"
                "c.poll()\n"
                "ibkr_dx.configure(log_level='')\n"
                "c.reqExecutions(2, f)\n"
                "c.poll()\n"
                "print(ibkr_dx.settings()['log_level'])\n"
                "ibkr_dx.configure(log_level='info')\n"
                "ibkr_dx.configure(log_level=None)\n"
                "c.reqExecutions(3, f)\n"
                "c.poll()\n"
                "print(ibkr_dx.settings()['log_level'])\n")
    assert done.returncode == 0, done.stderr
    assert done.stdout.splitlines() == ["None", "None"], done.stdout
    assert done.stderr.count("The dates: [2000-01-01] are outside") == 1, done.stderr


def test_a_log_level_in_the_environment_that_is_not_one_is_said():
    done = _run("import ibkr_dx", IBKR_DX_LOG_LEVEL="info=loud")
    assert done.returncode == 0, done.stderr
    assert "IBKR_DX_LOG_LEVEL info=loud is not a level this logger reads" in done.stderr, done.stderr
