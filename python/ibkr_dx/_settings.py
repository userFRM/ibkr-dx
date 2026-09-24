"""The settings a gateway keeps in its own configuration file.

A gateway is a process, and a process is configured by a file next to it and a
window in front of it. This client is a library, so the same settings belong on
the client where a caller can set them in code and read them back.

The same settings are stated on the Rust client as ``EClientConfig.gateway``,
where they belong to the session that states them. This is the same list,
reached the other way: what is set here is what a session that states none of
its own falls back to.

Each setting below names the IB Gateway setting it corresponds to. A few of
those have no meaning without a local process to configure — a port to listen
on, the addresses allowed to reach it, how much heap the runtime may take — and
are named at the bottom rather than dropped, so nobody goes looking for them.

Settings are read when a session opens, so set them before ``connect()``.
Setting one afterwards affects the next session, not the running one.

The three logging settings are read earlier still. A process has one logger and
importing ``ibkr_dx`` installs it, so ``IBKR_DX_LOG_LEVEL``, ``IBKR_DX_LOG_DIR`` and
``IBKR_DX_LOG_QUEUE`` are read at that moment. The level can be moved afterwards:
:func:`configure` moves the logger ``ibkr_dx`` installed. Where it writes and how
much it buffers are fixed once it runs, so those two belong in the environment
before the import, and :func:`configure` refuses them rather than storing a
value nothing will read.
"""

from __future__ import annotations

import os

#: Each setting, the variable it is held in, and the equivalent IB Gateway
#: setting. Held in the environment because that is where this client already
#: reads them from; the names here are the interface, the variables are not.
#
# ponytail: environment-backed because every read site already does env::var
# lazily. If settings ever need to differ between two sessions in one process,
# this becomes a per-session struct passed through connect().
_SETTINGS: dict[str, tuple[str, str]] = {
    "timezone": ("IBKR_DX_TZ", "the time zone announced at logon"),
    "log_level": ("IBKR_DX_LOG_LEVEL", "verbose logging"),
    "log_dir": ("IBKR_DX_LOG_DIR", "log directory"),
    "log_queue": ("IBKR_DX_LOG_QUEUE", "how many records logging buffers before dropping them"),
    "market_data_host": ("IBKR_DX_FARM_HOST", "the host every farm connection is opened on"),
    "port": ("IBKR_DX_MISC_PORT", "the port a farm connection opens on, where the routing names none"),
    "registration_timeout_ms": (
        "IBKR_DX_REGISTRATION_TIMEOUT_MS",
        "how long to wait to be admitted",
    ),
    "locale": ("IBKR_DX_LOCALE", "session locale"),
    "build": ("IBKR_DX_BUILD", "the build announced at logon"),
    "version": ("IBKR_DX_VERSION", "the version announced at logon"),
    "encoded": ("IBKR_DX_ENCODED", "the longer string announced with them"),
    "hardware_id": ("IBKR_DX_HWID", "the machine identity presented at logon"),
    "mac_address": ("IBKR_DX_MAC", "the network card named as this machine's, where the machine's own is not the one to name"),
    "lan_ip": ("IBKR_DX_IP", "the address on the local network named as this machine's, for the same reason"),
    "execution_reports": (
        "IBKR_DX_EXECUTION_REPORTS",
        "which executions arrive when a session opens: 'today' or 'all'",
    ),
    "island_for_nasdaq": (
        "IBKR_DX_ISLAND_FOR_NASDAQ",
        "whether a US stock on Nasdaq is handed back under the older spelling",
    ),
    "reconnect_on_socket_err": (
        "IBKR_DX_RECONNECT_ON_SOCKET_ERR",
        "whether a session recovers on its own when a connection goes away",
    ),
}

#: Gateway settings that are not settings here, and what to do instead. Named
#: rather than dropped: a caller migrating from a gateway will look for them,
#: and "there is no such thing here" is an answer where silence is not. Some do
#: have a stand-in — it is simply not one of the settings above — and the reason
#: names it.
UNAVAILABLE: dict[str, str] = {
    "rejectMessagesAboveMaxRate": "nothing paces what a caller sends: a gateway paces requests at the rate its logon states (fifty a second where it states none) unless this is set; set, a request above that rate is answered with error 100 and still carried out, and the third ends the connection, unless the venue or the client asks for pacing. This client does neither; the pacing here is the subscription burst a reconnect replays, stated on ReconnectConfig",
    "sendInstrumentTimezone": "no setting chooses the zone a timestamp is stated in: a bar is stated on the zone the venue names beside it, or as seconds since the epoch where the request asked for that; an execution is stamped as the venue stamps it",
    "LocalServerPort": "no local socket to listen on; this client is the client",
    "LocalApiPort": "no local socket to listen on; this client is the client",
    "TrustedIPs": "nothing connects to this client, so nothing needs trusting",
    "Local_FIX_Server_Settings": "no local socket to listen on; this client is the client, and the only sockets it holds are the ones it opened to the venue",
    "ApiOnly": "`readonly` on the client config, or connect(readonly=True): stated per session rather than once for a process",
    "RemoteHostOrderRouting": "`host` on the client config, or connect(host=...): orders are routed on the connection the login opened, and left unset the venue names the server this account is on",
    "RemotePortOrderRouting": "one port, fixed by the protocol: a redirect naming another is accepted at the socket and then reset, and the session only completes on the fixed one",
    "useSsl": "no switch: the login and the order connection are TLS with no plaintext path, and a farm connection is opened the one way the venue answers — a key exchange, an enciphered logon, then messages signed rather than enciphered",
    "UseSSL": "no switch: the login and the order connection are TLS with no plaintext path, and a farm connection is opened the one way the venue answers — a key exchange, an enciphered logon, then messages signed rather than enciphered",
    "Select_account_type": "`paper` on the client config, or connect(paper=True): nothing here selects an account type, the login decides it and the venue names the accounts it holds at logon",
    "MainWindow.Width": "no window",
    "MainWindow.Height": "no window",
    "vmoptions": "no runtime to size",
}


#: The two that are fixed once the logger runs. A process has one logger, and
#: importing ``ibkr_dx`` installs it, so a value set from here arrives after the
#: only moment it could have been read. Refused rather than stored: stored, it
#: reads back as a setting that was set and did nothing. The level is not one of
#: them — it moves the running logger.
_INSTALLED_AT_IMPORT = ("log_dir", "log_queue")


def configure(**settings) -> None:
    """Set one or more settings. Returns nothing; raises on a name it does not have.

    Raising rather than ignoring: a misspelled setting that is silently dropped
    leaves a caller believing a session is configured a way it is not.

        ibkr_dx.configure(timezone="America/New_York", execution_reports="today")

    ``log_level`` moves the logger ``ibkr_dx`` installed, and raises where the
    program installed its own; ``None`` or ``""`` moves it back to the level it
    was installed at, as either leaves the setting unset. ``log_dir`` and
    ``log_queue`` are refused here:
    set them in the environment before ``import ibkr_dx``, which is when the
    logger is installed.
    """
    unknown = set(settings) - set(_SETTINGS)
    if unknown:
        raise ValueError(
            f"no such setting: {', '.join(sorted(unknown))}. "
            f"Known: {', '.join(sorted(_SETTINGS))}"
        )
    too_late = sorted(set(settings) & set(_INSTALLED_AT_IMPORT))
    if too_late:
        raise ValueError(
            f"{', '.join(too_late)} belongs to the process, not one session: "
            "importing ibkr_dx installs the logger, so set "
            f"{', '.join(_SETTINGS[name][0] for name in too_late)} in the "
            "environment before that"
        )
    if "log_level" in settings:
        from .ibkr_dx import _set_log_level

        # Empty is unset, as the session reads it; unset is the level the
        # logger was installed at, rather than wherever it was last moved.
        if settings["log_level"] == "":
            settings["log_level"] = None
        level = settings["log_level"]
        _set_log_level(None if level is None else str(level))
    for name, value in settings.items():
        var, _ = _SETTINGS[name]
        if value is None:
            os.environ.pop(var, None)
        else:
            os.environ[var] = str(value)


def settings() -> dict[str, str | None]:
    """Every setting and what it is currently set to, unset ones as ``None``."""
    return {name: os.environ.get(var) for name, (var, _) in _SETTINGS.items()}


def describe() -> str:
    """Every setting, its value, and the IB Gateway setting it corresponds to."""
    lines = ["settings:"]
    for name, (var, stands_for) in sorted(_SETTINGS.items()):
        value = os.environ.get(var)
        shown = "unset" if value is None else repr(value)
        lines.append(f"  {name:24s} {shown:28s} {stands_for}")
    lines.append("")
    lines.append("no counterpart here:")
    for name, why in sorted(UNAVAILABLE.items()):
        lines.append(f"  {name:24s} {why}")
    return "\n".join(lines)


def _names_match_the_rust_client() -> list[str]:
    """The settings this module carries, for the test that checks both lists.

    Two lists of the same settings drift the moment one is added to. The test
    beside this compares them, so a setting added on one side and not the other
    fails rather than being quietly available in one language.
    """
    return sorted(_SETTINGS)
