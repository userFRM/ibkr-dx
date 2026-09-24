"""One table: every call the TWS API names, and which client carries it.

This client exists to be put where a gateway was, so the question a reader
actually has is not "what can it do" but "is anything I use missing". A page
that describes only this client cannot answer that. A row per call, a column
per client, and a mark in each, can.

Every column is read from the client it names — not recalled, not asserted:

  TWS API     the canonical list this repository already generates
  ibapi       IBKR's own Python client, imported and enumerated
  ib_async    the widely used asynchronous client, imported and enumerated
  ibkr-dx     this client, from the coverage matrix the build already checks

A reference client's mark says a method exists and nothing more: enumerating a
package cannot say what a method does. This client's marks say what it does,
and the evidence column says how that was established.

A column can only be written for a client that is installed. One that is not
is left out of the table entirely rather than filled with guesses, and the page
says which were read and at what version.

    python scripts/gen_parity_matrix.py
"""

import importlib
import inspect
import os
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
COVERAGE = ROOT / "docs" / "book" / "src" / "reference" / "coverage-data.md"
OUT = ROOT / "docs" / "capabilities.md"
README = ROOT / "README.md"

#: Where the README carries the same answer. Written between these, so the
#: page a reader lands on cannot drift from the one this script generates —
#: a comparison somebody keeps by hand is a comparison that is wrong by the
#: second release.
README_OPEN = "<!-- capabilities:begin — written by scripts/gen_parity_matrix.py -->"
README_SHUT = "<!-- capabilities:end -->"

#: Where a reference client lives when it is not on the import path.
#:
#: The official client is not a dependency of this repository — it is what this
#: client replaces — so it is installed beside the tests rather than depended
#: on, and read from the import path like any other package.
#:
#: These are the fallbacks for a copy that was unpacked rather than installed.
#: A directory under somebody's home used to be named here, which made this
#: page reproducible on exactly one machine: the check that compares a
#: generated file against the committed one then failed everywhere else, and
#: the columns for both reference clients silently vanished from the answer.
#: `IBKR_DX_REFERENCE_CLIENTS` names such a directory where there is one.
EXTRA_PATHS = [
    pathlib.Path(p) for p in os.environ.get("IBKR_DX_REFERENCE_CLIENTS", "").split(os.pathsep) if p
] + [ROOT / "vendor"]

#: What a mark means. Three states, because two would lie: a call that exists
#: and reports why it cannot be served is not the same as one that is absent,
#: and a reader porting a program needs to tell them apart.
SERVED, TAKEN, ABSENT = "●", "◐", "·"

#: Columns that describe a row rather than a client, and are not counted.
NOT_A_CLIENT = {"Evidence", "Fires on a gateway", "Answered from"}

#: Calls beyond the canonical list that answer from this client itself — its
#: own state, a measurement it takes, or a helper — rather than from anything
#: the venue states. Every other row there asks the venue or reads what it
#: stated. Named by their spelling-free form, so either surface's name matches.
#:
#: Hand-kept, and checked: a name here that no longer appears in that table
#: fails the run rather than standing as a claim about nothing.
LOCAL = {
    "checkconnected", "connectwithevents", "eventslost", "instrumentof",
    "keeprecord", "lastrtt", "lastrttms", "nextorderid", "nextsharedid",
    "parsealgoparams", "poll", "reset", "run", "serverversion", "session",
    "sessionover", "sessiontokenbytes", "setconnectoptions", "sharedstate",
    "startapi", "unreadwire", "waitfordata",
}


def as_camel(snake: str) -> str:
    """`req_mkt_data` as a Python client spells it: `reqMktData`."""
    head, *rest = snake.split("_")
    return head + "".join(part[:1].upper() + part[1:] for part in rest)


def rows_from_coverage():
    """The canonical calls and callbacks, with this client's own two columns.

    The matrix's third column is the C++ name — `eConnect` — and a Python
    client does not name its methods that way, so what is asked of one is the
    camel spelling of the call itself. The C++ name is kept as a second guess
    for the few that differ.

    Read from the generated matrix rather than restated here: that file is
    produced from the source and checked against it on every commit, so there
    is one list and it cannot drift from a second copy.
    """
    text = COVERAGE.read_text()
    calls, backs, section = [], [], None
    for line in text.splitlines():
        if line.startswith("## EClient"):
            section = "calls"
            continue
        if line.startswith("## EWrapper"):
            section = "backs"
            continue
        if line.startswith("## "):
            section = None
            continue
        if not section or not line.startswith("| ") or "`" not in line:
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if section == "calls" and len(cells) >= 5 and cells[1].startswith("`"):
            calls.append({
                "category": cells[0],
                "snake": cells[1].strip("`"),
                "camel": as_camel(cells[1].strip("`")),
                "cpp": cells[2].strip("`"),
                "rust": cells[3],
                "python": cells[4],
                "evidence": cells[5] if len(cells) > 5 else "",
            })
        elif section == "backs" and len(cells) >= 4 and cells[1].startswith("`"):
            backs.append({
                "category": cells[0],
                "snake": cells[1].strip("`"),
                "camel": as_camel(cells[1].strip("`")),
                "cpp": cells[1].strip("`"),
                "rust": cells[2],
                "python": cells[3],
                "gateway_sends": len(cells) < 5 or cells[4] != "no",
            })
    # The category cell is left blank on continuation rows in that file.
    for group in (calls, backs):
        held = ""
        for row in group:
            held = row["category"] or held
            row["category"] = held
    return calls, backs


def stub_methods():
    """Calls that exist and state why they cannot be served.

    Read from the generator that already keeps the list, so the two pages
    cannot disagree about which a call is.
    """
    sys.path.insert(0, str(ROOT / "scripts"))
    try:
        import gen_api_docs

        return set(gen_api_docs.STUB_METHODS)
    except Exception:
        return set()
    finally:
        sys.path.pop(0)


def surface_from_reference(page: str):
    """Every method a generated reference page names, or None where absent.

    The Rust surface cannot be imported and enumerated the way a Python one
    can, and guessing it from the binding is how this page came to mark calls
    as carried in Rust that exist only in Python. The reference pages are
    produced from the source on every commit, so they are the surface.
    """
    path = ROOT / "docs" / "book" / "src" / "api" / page
    if not path.is_file():
        return None
    names = set(re.findall(r"^#### `([A-Za-z_][A-Za-z_0-9]*)", path.read_text(), re.M))
    return names or None


def surface_of(module_name, *attr_paths, kind=inspect.isfunction):
    """Every public method a client's classes define, or None where absent.

    More than one class may be named, and the answer is their union: a client
    that splits the transport from the facade carries some calls on one and
    some on the other, and asking only the transport marks the facade's own as
    missing. One of them does exactly that.

    None and an empty set are different answers: a client that is not
    installed cannot be reported on, and saying it carries nothing would be a
    claim about it rather than about this machine.
    """
    # Appended, never prepended. One of these directories carries a build of
    # a shared dependency for another interpreter, and putting it first made
    # an installed client fail to import — which this function would then have
    # reported as a client that is not installed, and the page as a column
    # nobody could see.
    for extra in EXTRA_PATHS:
        if extra.is_dir() and str(extra) not in sys.path:
            sys.path.append(str(extra))
    try:
        module = importlib.import_module(module_name)
    except Exception:
        return None, None
    names = set()
    for attr_path in attr_paths:
        obj = module
        for part in attr_path.split("."):
            obj = getattr(obj, part, None)
            if obj is None:
                break
        if obj is None or obj is module:
            continue
        # `kind` is what counts as a method. A compiled class's methods are
        # descriptors rather than functions, and asked for functions alone it
        # answered with only the ones given a second spelling in Python — so
        # every call whose two spellings are one word read as absent.
        names |= {
            name for name, _ in inspect.getmembers(obj, kind)
            if not name.startswith("_")
        }
    if not names:
        return None, None
    version = getattr(module, "__version__", None)
    return names, version


def package_version(name: str) -> str | None:
    """What a client calls itself, asked of the package and not a module in it.

    A submodule carries no version of its own, and asking one answered
    "unstated" for a client that states it plainly one level up.
    """
    try:
        module = importlib.import_module(name)
    except Exception:
        return None
    stated = getattr(module, "__version__", None)
    if stated:
        return str(stated)
    numbers = getattr(module, "VERSION", None)
    if isinstance(numbers, dict):
        parts = [numbers.get(k) for k in ("major", "minor", "patch")]
        if all(p is not None for p in parts):
            return ".".join(str(p) for p in parts)
    return None


#: What a call used to be called, where the TWS API renamed one. A client that
#: predates the rename carries the thing under its old name, and marking it
#: absent would say it cannot do something it has always done. Each entry is a
#: former name of the same call, and nothing else belongs here: a name that is
#: merely similar is a different call.
FORMER_NAMES = {
    # Renamed when the fees were added alongside the commission.
    "commission_and_fees_report": ["commissionReport"],
}


def plain(name: str) -> str:
    """A name with its spelling taken off, so two clients can be compared.

    One writes `realtimeBar`, another `realTimeBar`, a third `real_time_bar`.
    Compared as written, a client was marked as missing a callback it has —
    which is the one thing a table like this must never do. What is left after
    the letters are lowered and everything else dropped is the same in all
    three, and differs only where the clients genuinely named different things.
    """
    return "".join(ch for ch in name.lower() if ch.isalnum())


def known(row, names) -> bool:
    """Whether a client names this call, however it spells it."""
    spellings = {plain(row["camel"]), plain(row["cpp"]), plain(row["snake"])}
    spellings |= {plain(n) for n in FORMER_NAMES.get(row["snake"], [])}
    return bool(spellings & {plain(n) for n in names})


def mark(present):
    return SERVED if present else ABSENT


def ours(state):
    """This client's own mark, from the coverage matrix's own vocabulary."""
    return {"Y": SERVED, "STUB": TAKEN}.get(state, ABSENT)


def table(rows, columns, title, intro):
    # The category column is only there where the rows have categories; a
    # table of blanks beside every name reads as a column somebody forgot.
    grouped = any(row["category"] for row in rows)
    lead = "| Category | Call | " if grouped else "| Call | "
    rule = "| --- | --- | " if grouped else "| --- | "
    head = [lead + " | ".join(c[0] for c in columns) + " |",
            rule + " | ".join(":---:" for _ in columns) + " |"]
    body = []
    held = None
    for row in rows:
        cells = " | ".join(c[1](row) for c in columns)
        if grouped:
            shown = "" if row["category"] == held else row["category"]
            held = row["category"]
            body.append(f"| {shown} | `{row['snake']}` | {cells} |")
        else:
            body.append(f"| `{row['snake']}` | {cells} |")
    return [f"## {title}", "", intro, ""] + head + body + [""]


def counted(rows, columns):
    """Carried, taken and absent per client, over one table's rows."""
    for name, fn in columns:
        served = sum(1 for r in rows if fn(r) == SERVED)
        taken = sum(1 for r in rows if fn(r) == TAKEN)
        yield name, served, taken, len(rows) - served - taken


def write_readme(page, calls, backs, columns, back_columns, beyond):
    """Put the same answer on the page a reader lands on.

    Somebody deciding whether to put this client where their gateway is has
    one question — is anything I already use missing — and sending them to
    another file to find out is sending most of them away. The whole page goes
    in, behind a fold so it does not bury the rest of the readme, under a
    summary that answers the question on its own.

    Written rather than kept: a comparison table maintained by hand is a
    comparison table that is wrong by the second release, and this one is read
    from the clients themselves on every commit.
    """
    if not README.is_file():
        return
    text = README.read_text()
    if README_OPEN not in text or README_SHUT not in text:
        return

    calls_by = {n: (s, t, a) for n, s, t, a in counted(calls, columns) if n not in NOT_A_CLIENT}
    backs_by = {n: (s, t, a) for n, s, t, a in counted(backs, back_columns) if n not in NOT_A_CLIENT}
    # One line per client, and this client's two surfaces last, because they
    # are the answer and the rest is what the answer is measured against.
    ours = [n for n in calls_by if n.startswith("ibkr-dx")]
    theirs = [n for n in calls_by if not n.startswith("ibkr-dx")]

    def row(name):
        served, taken, absent = calls_by.get(name, (0, 0, 0))
        b_served, b_taken, b_absent = backs_by.get(name, (0, 0, 0))
        of_calls = f"{served} / {len(calls)}"
        of_backs = f"{b_served} / {len(backs)}" if name in backs_by else "—"
        missing = []
        if absent:
            missing.append(f"{absent} absent")
        if taken:
            missing.append(f"{taken} present, not served")
        if b_taken:
            missing.append(f"{b_taken} callback declared, not fired"
                           if b_taken == 1 else
                           f"{b_taken} callbacks declared, not fired")
        if b_absent:
            missing.append(f"{b_absent} callbacks absent")
        note = ", ".join(missing) or "nothing missing"
        mine = name.startswith("ibkr-dx")
        cells = [f"**{name}**" if mine else name,
                 f"**{of_calls}**" if mine else of_calls,
                 f"**{of_backs}**" if mine else of_backs,
                 note]
        return "| " + " | ".join(cells) + " |"

    summary = [
        "| Client | Calls | Callbacks | |",
        "| --- | ---: | ---: | --- |",
    ] + [row(n) for n in theirs + ours]

    # Said from the figures, not asserted: if a gap ever opens this line
    # reports it instead of claiming there is none.
    gone = sum(calls_by.get(n, (0, 0, 0))[2] + backs_by.get(n, (0, 0, 0))[2] for n in ours)
    held = sum(calls_by.get(n, (0, 0, 0))[1] + backs_by.get(n, (0, 0, 0))[1] for n in ours)
    unsent = sum(1 for r in backs if not r.get("gateway_sends", True))
    if gone:
        verdict = (
            f"**{gone} of them are absent here**, counted rather than left out of "
            "the denominator. The table below says which."
        )
    else:
        verdict = (
            "**Every call and callback on that list is present on both surfaces.** "
            + (f"{held // max(len(ours), 1)} callbacks are declared and not fired: "
               "a gateway reroutes a request to another contract for a contract for "
               "difference whose definition asks for it, and this client does not read "
               "a definition's flags for that before subscribing: the request goes to "
               "the venue as asked. Each says so where it is declared, so a program "
               "that implements one still compiles and runs. "
               if held else "")
            + (f"{unsent} more are declared by the TWS API and never fire on a gateway — "
               "the four steps of the verification handshake, the exchange-for-physical "
               "quote, the delta-neutral validation, which a gateway never sends, and "
               "`win_error`, which no message on the wire carries — so they "
               "fire here exactly as often as there: never."
               if unsent else "")
        )

    block = [
        README_OPEN,
        "",
        "## Capabilities",
        "",
        f"One row per capability, one column per client — every one of the "
        f"{len(calls)} calls and {len(backs)} callbacks on the canonical list of the "
        "TWS API's requests and callbacks, read from each client rather than recalled.",
        "",
    ] + summary + [
        "",
        verdict,
    ] + ([
        "",
        f"**And {len(beyond)} more beyond that list.** The venue states more on a "
        "session than the documented calls ask for — what it permits this account, "
        "which algorithms it offers, the order defaults it fills an order's blanks "
        "from, what it says about an issuer, which session holds the account — and "
        "this client answers for those too, beside helpers and instrumentation of its "
        "own. The table under *Beyond the canonical list* says which each is, and "
        "which a reference client also names.",
    ] if beyond else []) + [
        "",
        "Every figure here is read from the client it names, on the machine that "
        "generated it. A client that is not installed is left out rather than "
        "filled in from memory.",
        "",
        "<details>",
        "<summary><b>The whole table — every call, every callback, and the calls "
        "beyond them</b></summary>",
        "",
    ]
    # The page itself, minus its own title and the note about where it comes
    # from, which the readme has said already.
    body = [ln for ln in page if ln.strip() != "# Capabilities"]
    while body and not body[0].strip():
        body.pop(0)
    if body and body[0].startswith("*Generated by"):
        body.pop(0)
    block += body + [
        "</details>",
        "",
        # Absolute, because the readme is also the package page on the
        # registries, where a path relative to the repository leads nowhere.
        "The same table stands on its own in "
        "[docs/capabilities.md](https://github.com/userFRM/ibkr-dx/blob/main/docs/capabilities.md), "
        "and what each claim rests on is in "
        "[docs/evidence.md](https://github.com/userFRM/ibkr-dx/blob/main/docs/evidence.md).",
        "",
        README_SHUT,
    ]

    head, _, rest = text.partition(README_OPEN)
    _, _, tail = rest.partition(README_SHUT)
    README.write_text(head + "\n".join(block) + tail)


def main() -> int:
    if not COVERAGE.is_file():
        print(f"{COVERAGE} is not there; run scripts/gen_api_docs.py first")
        return 1
    calls, backs = rows_from_coverage()

    ibapi_calls, _ = surface_of("ibapi.client", "EClient")
    ibapi_v = package_version("ibapi")
    ibapi_backs, _ = surface_of("ibapi.wrapper", "EWrapper")
    async_calls, async_v = surface_of("ib_async", "client.Client", "IB")
    async_backs, _ = surface_of("ib_async", "wrapper.Wrapper", "IB")
    # The transport alone: the requests ib_async sends as TWS API messages, as
    # opposed to the helpers its facade builds on them.
    async_wire, _ = surface_of("ib_async", "client.Client")

    columns = []
    read = [
        "**TWS API** — the canonical list this repository keeps of the TWS API's "
        "requests and callbacks, in `scripts/gen_api_docs.py`. Beyond that list, a "
        "call is marked here where the TWS API's own client, or ib_async's "
        "transport, has a method by that name; a helper of ib_async's facade "
        "is not.",
    ]
    if ibapi_calls is not None:
        columns.append(("ibapi", lambda r: mark(known(r, ibapi_calls))))
        read.append(
            f"**ibapi** — IBKR's own Python client, version `{ibapi_v or 'unstated'}`, "
            "imported and enumerated. This is the copy published to PyPI; the "
            "version IBKR distributes directly is numbered 10.x and names calls "
            "this one predates, so a gap in this column is a gap in the copy "
            "that was read and not necessarily in the client you have."
        )
    if async_calls is not None:
        columns.append(("ib_async", lambda r: mark(known(r, async_calls))))
        read.append(
            f"**ib_async** — version `{async_v or 'unstated'}`, imported and "
            "enumerated across both the transport and the facade, because it "
            "carries some calls on one and some on the other."
        )
    columns.insert(0, ("TWS API", lambda r: SERVED if r.get("documented", True) else ABSENT))
    columns += [
        ("ibkr-dx Rust", lambda r: ours(r["rust"])),
        ("ibkr-dx Python", lambda r: ours(r["python"])),
    ]
    read += [
        "**ibkr-dx Rust** and **ibkr-dx Python** — this client's two surfaces, "
        "from the coverage matrix `scripts/gen_api_docs.py` generates from the "
        "source. A mark here says what the call does, not only that it exists.",
        "**Evidence** — how this client's status for a call was established: named "
        "by a suite that opens a session, named only by the offline suites, or "
        "not named by a test.",
        "**Fires on a gateway** — whether the callback fires at all for a program "
        "on a gateway. The TWS API declares six that never do.",
        "**Answered from** — beyond the canonical list, whether a call asks the "
        "venue or reads what it stated, or answers from this client itself: its "
        "own state, a measurement it takes, or a helper.",
    ]
    call_columns = columns + [("Evidence", lambda r: r.get("evidence", ""))]

    back_columns = [
        ("TWS API", lambda r: SERVED),
        ("Fires on a gateway", lambda r: "yes" if r.get("gateway_sends", True) else "no"),
    ]
    if ibapi_backs is not None:
        back_columns.append(("ibapi", lambda r: mark(known(r, ibapi_backs))))
    if async_backs is not None:
        back_columns.append(("ib_async", lambda r: mark(known(r, async_backs))))
    back_columns += [
        ("ibkr-dx Rust", lambda r: ours(r["rust"])),
        ("ibkr-dx Python", lambda r: ours(r["python"])),
    ]

    out = [
        "# Capabilities",
        "",
        "*Generated by `scripts/gen_parity_matrix.py`. Do not edit.*",
        "",
        "One row per capability, one column per client. This client exists to be",
        "put where a gateway was, so the question is not what it can do but",
        "whether anything you already use is missing — which is a question about",
        "the columns, not about the rows.",
        "",
        "The first two tables are the canonical list of the TWS API's calls and",
        "callbacks, which are the rows a port has to match. The last is what this",
        "client answers beyond them.",
        "",
        "Presence and behaviour are marked apart. A reference client's mark says a",
        "method exists, read by importing the package and listing its methods;",
        "it says nothing about what the method does. This client's mark says what",
        "the call does, and the evidence column says how that was established.",
        "",
        f"| Mark | Meaning |",
        "| :---: | --- |",
        f"| {SERVED} | Present. For ibkr-dx, also served: a call does what it names; a callback is fired whenever what it reports arrives |",
        f"| {TAKEN} | Present and not served: a call reports why on the error callback; a callback is declared and not fired here, although a gateway sends it |",
        f"| {ABSENT} | Absent |",
        "",
        "Each column is read from the client it names, on the machine that",
        "generated this page:",
        "",
    ] + [f"- {line}" for line in read] + [
        "",
        "A client that is not installed is left out of the table rather than",
        "filled in from memory. A mark here is a thing that was read.",
        "",
    ]

    out += table(
        calls, call_columns, "Calls",
        "What a program asks the venue for.",
    )
    out += table(
        backs, back_columns, "Callbacks",
        "What the venue says back. `ib_async` delivers these as events as well "
        "as methods, so a mark here says the method exists on its wrapper, not "
        "that the information is unavailable by another route.",
    )

    # What each surface has that the canonical list does not name. Each column
    # is read from its own surface: taking the Python binding's methods and
    # marking both surfaces carried invented a Rust column, and calls that
    # exist only in the binding were published as Rust's too.
    beyond: list[str] = []
    ours_extra, _ = surface_of("ibkr_dx", "EClient", kind=inspect.isroutine)
    rust_extra = surface_from_reference("rust-reference.md")
    if ours_extra is not None:
        # Both tables above, not only the calls. The Rust surface is read off
        # its reference page, which names the callbacks beside the calls — so
        # measured against the calls alone, every documented callback this
        # client implements was published here as something beyond the
        # canonical list, in a table whose first column says "Call".
        canon = {plain(r[spelling]) for r in calls + backs for spelling in ("snake", "camel")}
        rust_plain = {plain(n) for n in (rust_extra or set())}
        both = sorted(
            {n for n in ours_extra if plain(n) not in canon}
            | {n for n in (rust_extra or set()) if plain(n) not in canon},
            # The spelling breaks the tie, so the same one is picked every
            # time. Sorted on the comparison key alone, which of the two
            # surfaces' spellings a row was written under came out of a set's
            # iteration order and changed from one run of this script to the
            # next, for a page that is checked against what is committed.
            key=lambda name: (plain(name), name),
        )
        # One row per capability, not one per spelling: the two surfaces name
        # the same thing in their own cases.
        seen, extra = set(), []
        for name in both:
            if plain(name) in seen:
                continue
            seen.add(plain(name))
            extra.append(name)
        beyond = extra
        if extra:
            # A call that exists and states why it cannot be served is not
            # carried, and the canonical table already tells the two apart.
            # Told apart only there, a call in this table read as carried while
            # the page's own key said otherwise.
            stubs = {plain(n) for n in stub_methods()}
            rows = [{
                "category": "", "snake": n, "camel": n, "cpp": n,
                "rust": ("STUB" if plain(n) in stubs else "Y")
                        if plain(n) in rust_plain else "-",
                "python": ("STUB" if plain(n) in stubs else "Y")
                          if plain(n) in {plain(m) for m in ours_extra} else "-",
                # The documented surface names it after all where the TWS
                # API's own client, or ib_async's transport, has a method by
                # that name: the canonical list this page is built from is not
                # the whole of that surface. A helper ib_async's facade builds
                # on top of the messages is not a TWS API call.
                "documented": bool(
                    (ibapi_calls and known({"snake": n, "camel": n, "cpp": n}, ibapi_calls))
                    or (async_wire and known({"snake": n, "camel": n, "cpp": n}, async_wire))
                ),
                "answered_from": "this client" if plain(n) in LOCAL else "venue",
            } for n in extra]
            stale = LOCAL - {plain(n) for n in extra}
            if stale:
                print(f"LOCAL names calls this table no longer has: {', '.join(sorted(stale))}")
                return 1
            beyond_columns = (
                [("Answered from", lambda r: r["answered_from"])] + columns
            )
            out += table(
                rows, beyond_columns, "Beyond the canonical list",
                "What this client answers that the canonical list does not name.\n"
                "Three kinds, told apart by the *Answered from* column and by the\n"
                "reference columns: what the venue states that no documented call\n"
                "asks for (what it permits this account, which algorithms it offers,\n"
                "the order defaults it holds, what it says about an issuer, which\n"
                "session holds the account); a question answered in one call rather\n"
                "than delivered on a callback; and this client's own state, helpers\n"
                "and instrumentation, which are about the client and not the venue.\n\n"
                "A mark against a reference client here means it names the same\n"
                "thing; a mark under TWS API means the TWS API's own client, or\n"
                "ib_async's transport, has a method by that name.",
            )

    totals = []
    for name, fn in columns:
        if name in NOT_A_CLIENT:
            continue
        served = sum(1 for r in calls if fn(r) == SERVED)
        taken = sum(1 for r in calls if fn(r) == TAKEN)
        totals.append(f"| {name} | {served} | {taken} | {len(calls) - served - taken} |")
    out += [
        "## Calls, counted",
        "",
        f"| Client | Present {SERVED} | Present, not served {TAKEN} | Absent {ABSENT} |",
        "| --- | ---: | ---: | ---: |",
    ] + totals + [""]

    OUT.write_text("\n".join(out) + "\n")
    write_readme(out, calls, backs, columns, back_columns, beyond)
    print(f"{OUT.relative_to(ROOT)} — {len(calls)} calls, {len(backs)} callbacks, "
          f"{len(columns)} columns")
    for line in totals:
        print("   ", line.strip("| ").replace(" | ", "  "))
    return 0


if __name__ == "__main__":
    sys.exit(main())
