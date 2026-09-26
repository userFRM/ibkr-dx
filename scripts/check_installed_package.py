"""What an installed ibkr-dx carries, checked from outside the repository.

Run with the interpreter a wheel or a source distribution was installed into;
it imports only what is installed. The suites cannot answer this: they run
against a build that carries the test helpers, and what is published must not.

    python scripts/check_installed_package.py

Exits non-zero and names each problem.
"""

import os
import sys
import sysconfig
from importlib.metadata import version

import ibkr_dx
from ibkr_dx.client import EClient
from ibkr_dx.contract import Contract
from ibkr_dx.order import Order
from ibkr_dx.wrapper import EWrapper


def problems() -> list[str]:
    found = []
    # The layout a program written against `ibapi` imports from, holding the
    # same objects the package itself exports.
    for module_name, name, held in (
        ("client", "EClient", EClient),
        ("wrapper", "EWrapper", EWrapper),
        ("contract", "Contract", Contract),
        ("order", "Order", Order),
    ):
        if getattr(ibkr_dx, name, None) is not held:
            found.append(f"ibkr_dx.{module_name}.{name} is not ibkr_dx.{name}")
    for name in ("configure", "settings", "describe", "UNAVAILABLE"):
        if not hasattr(ibkr_dx, name):
            found.append(f"ibkr_dx.{name} is missing")
    if getattr(ibkr_dx, "__version__", None) != version("ibkr-dx"):
        found.append("ibkr_dx.__version__ is not the installed release")
    if not callable(getattr(EClient, "req_mkt_data", None)):
        found.append("EClient has no req_mkt_data")
    # The methods that fabricate a session belong to the test build only.
    injected = sorted(n for n in dir(EClient) if n.startswith("_test"))
    if injected:
        found.append(f"the test helpers are compiled in: {', '.join(injected[:5])}")
    # A free-threaded interpreter turns the GIL back on for an extension that
    # does not declare it can run without one, and says so only in a warning.
    if sysconfig.get_config_var("Py_GIL_DISABLED") and sys._is_gil_enabled():
        found.append("importing the extension re-enabled the GIL")
    # Stated by a release build, which names the interpreter each wheel is for.
    # Handed the other kind, a build makes the wrong wheel and nothing fails.
    expected = os.environ.get("EXPECT_FREE_THREADED")
    free = bool(sysconfig.get_config_var("Py_GIL_DISABLED"))
    if expected is not None and (expected.lower() == "true") != free:
        found.append(f"a {'free-threaded' if expected.lower() == 'true' else 'GIL'} "
                     f"interpreter was expected, and this one is {'free-threaded' if free else 'not'}")
    return found


def main() -> int:
    found = problems()
    for line in found:
        print(line)
    if found:
        return 1
    threading = "free-threaded" if sysconfig.get_config_var("Py_GIL_DISABLED") else "with the GIL"
    print(f"ibkr-dx {version('ibkr-dx')} on Python "
          f"{sys.version_info.major}.{sys.version_info.minor} {threading}: "
          "EClient, EWrapper and the reference layout present, no test helpers")
    return 0


if __name__ == "__main__":
    sys.exit(main())
