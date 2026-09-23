"""The settings on both clients are the same settings.

Two lists of the same thing drift the moment one is added to. A setting added
on the Rust client and not here would be one a Python caller cannot set, and
the other way round would be one that does nothing.
"""

import re
import pathlib

import ibkr_dx


def _rust_settings() -> set[str]:
    source = pathlib.Path(__file__).resolve().parents[2] / "src/settings.rs"
    text = source.read_text()
    at = text.index("pub struct GatewaySettings {")
    end = text.index("\n}", at)
    return set(re.findall(r"^\s*pub (\w+):", text[at:end], re.M))


def _rust_unavailable() -> dict[str, str]:
    source = pathlib.Path(__file__).resolve().parents[2] / "src/settings.rs"
    text = source.read_text()
    at = text.index("pub const UNAVAILABLE:")
    end = text.index("];", at)
    return dict(re.findall(r'\(\s*"([\w.]+)",\s*"([^"]*)"\s*\)', text[at:end]))


def test_both_clients_carry_the_same_settings():
    assert _rust_settings() == set(ibkr_dx.settings()), (
        "a setting exists on one client and not the other"
    )


def test_both_clients_name_the_same_settings_as_unavailable():
    """A setting with no counterpart is a statement, and both clients make it.

    Comparing only what is carried leaves the other list free to drift: three
    names sat on the Rust list and not on this one, so a Python caller asking
    about them was told nothing rather than why. What a caller cannot have is
    as much a part of the surface as what they can.
    """
    assert set(_rust_unavailable()) == set(ibkr_dx.UNAVAILABLE), (
        "a setting is recorded as having no counterpart on one client and not the other"
    )


def test_both_clients_give_the_same_reason_for_a_setting_with_no_counterpart():
    """The reason is the whole of the answer, and two copies of it drift.

    Comparing only the names leaves each client free to say something different
    about the same setting, and a reason that is wrong is worse than none:
    someone migrating believes it. Two of these said nothing here paces
    outgoing messages while a reconnect's replay was paced, and both clients
    said it.
    """
    assert _rust_unavailable() == dict(ibkr_dx.UNAVAILABLE), (
        "the two clients give different reasons for the same setting"
    )


def test_every_setting_is_readable_after_being_set():
    ibkr_dx.configure(timezone="America/New_York")
    assert ibkr_dx.settings()["timezone"] == "America/New_York"
    ibkr_dx.configure(timezone=None)
    assert ibkr_dx.settings()["timezone"] is None


def test_a_misspelled_setting_is_refused():
    try:
        ibkr_dx.configure(timezon="UTC")
    except ValueError as refused:
        assert "timezon" in str(refused)
    else:
        raise AssertionError("a misspelled setting was accepted")


def test_a_session_states_its_own_settings():
    """Settings belong to the session that stated them, not to the process.

    Written into the process environment as a session opens, a second session
    in one process silently retargets the first: whichever connects last
    decides the time zone, the build, and where the market-data connection
    goes for both.
    """
    client = ibkr_dx.EClient(ibkr_dx.EWrapper())
    # Refused before anything is sent, so a misspelling cannot open a session
    # configured differently from the way it was written.
    try:
        client.connect(username="u", password="p", settings={"tiemzone": "UTC"})
    except RuntimeError as refusal:
        assert "no such setting: tiemzone" in str(refusal)
    else:
        raise AssertionError("a setting that is not a setting was accepted")

    # And the process is not touched by a session stating one.
    assert ibkr_dx.settings()["timezone"] is None


#: A count in a sentence may be written as a word, and the published pages
#: write the small ones that way.
_NUMBERS = {
    word: n for n, word in enumerate(
        "zero one two three four five six seven eight nine ten eleven twelve "
        "thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty".split()
    )
}


def _stated(pattern: str, text: str) -> list[int]:
    return [
        int(found) if found.isdigit() else _NUMBERS[found.lower()]
        for found in re.findall(pattern, text)
    ]


def test_the_published_counts_are_what_the_lists_hold():
    """Both lists grew and the pages that count them did not: the readme and
    the evidence page said fourteen settings and ten that are not settings
    here while the lists held seventeen and sixteen."""
    root = pathlib.Path(__file__).resolve().parents[2]
    for page in ("README.md", "docs/evidence.md"):
        # Joined, so a figure broken across a line is read with its words.
        text = " ".join((root / page).read_text().split())
        carried = _stated(r"\b(\w+) (?:in total, readable at runtime|settings carried)", text)
        missing = _stated(r"\b(\w+) (?:gateway settings are|recorded as) not settings here", text)
        assert carried and missing, f"{page} no longer states either count"
        assert set(carried) == {len(ibkr_dx.settings())}, f"{page}: {carried}"
        assert set(missing) == {len(ibkr_dx.UNAVAILABLE)}, f"{page}: {missing}"
