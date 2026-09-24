//! PyO3 bindings for ibkr_dx. Feature-gated behind `python`.
//!
//! Provides an ibapi-compatible API (callback-based):
//! ```python
//! from ibkr_dx import EClient, EWrapper, Contract, Order
//! class App(EWrapper):
//!     def next_valid_id(self, order_id):
//!         ..
//! app = App()
//! client = EClient(app)
//! client.connect(username="user", password="pass", paper=True)
//! client.run()
//! ```

/// What the Python side scales prices by, which is what the model scales them
/// by. A file of its own said nothing more than this line.
mod types {
    pub use crate::types::model::PRICE_SCALE_F;
}
pub mod compat;

/// What a session runs under, from the names the Python client states them by.
///
/// The same names `ibkr_dx.configure` uses, so a caller states a setting the same
/// way whether it is for one session or for the process.
pub(crate) fn settings_from(
    stated: std::collections::HashMap<String, String>,
) -> Result<crate::settings::SessionSettings, String> {
    use crate::settings::{ExecutionReportScope, GatewaySettings};
    let mut settings = GatewaySettings::default();
    let mut level = None;
    for (name, value) in stated {
        match name.as_str() {
            "timezone" => settings.timezone = Some(value),
            "locale" => settings.locale = Some(value),
            "build" => settings.build = Some(value),
            "version" => settings.version = Some(value),
            "encoded" => settings.encoded = Some(value),
            "hardware_id" => settings.hardware_id = Some(value),
            "mac_address" => settings.mac_address = Some(value),
            "lan_ip" => settings.lan_ip = Some(value),
            "market_data_host" => settings.market_data_host = Some(value),
            "port" => settings.port = Some(value.parse().map_err(|_| format!("port: {value}"))?),
            // One logger per process, and importing this client installs it.
            // Its level can be moved while it runs, so a level stated here
            // moves it, as `configure` does; where it is not this client's
            // logger, or not a level, the connect says so. Moved once every
            // other setting has been read: a connect refused for one of those
            // leaves the level where it was.
            "log_level" => level = Some(value),
            // Where it writes and how much it buffers are fixed once it is
            // installed. Taken here they were parsed, held and then dropped on
            // the way to the session, which reads as a setting that was set
            // and did nothing. Said plainly instead, naming where they do work
            // — which for this client is before it is imported.
            "log_dir" | "log_queue" => {
                return Err(format!(
                    "{name} belongs to the process, not one session: importing ibkr_dx \
                     installs the logger, so set IBKR_DX_{} in the environment before that",
                    name.to_uppercase(),
                ));
            }
            // However it is spelled, matching what the same settings are read
            // as when they come from the environment. Matched against the
            // lowercase spelling alone, `Today` was refused here where the
            // environment accepts it, and `False` turned the Island setting on.
            "execution_reports" => {
                settings.execution_reports = Some(if value.eq_ignore_ascii_case("today") {
                    ExecutionReportScope::Today
                } else if value.eq_ignore_ascii_case("all") {
                    ExecutionReportScope::All
                } else {
                    return Err(format!("execution_reports: {value}"));
                });
            }
            "island_for_nasdaq" => {
                settings.island_for_nasdaq = Some(
                    !["0", "false", "no"].iter().any(|off| value.eq_ignore_ascii_case(off)),
                );
            }
            // The switch a gateway carries, under its name there and the spelling
            // the rest of this map uses. Off means the loss is reported and
            // nothing is done about it.
            "reconnect_on_socket_err" | "reconnectOnSocketErr" => {
                settings.reconnect_on_socket_err = Some(
                    !["0", "false", "no"].iter().any(|off| value.eq_ignore_ascii_case(off)),
                );
            }
            other => return Err(format!("no such setting: {other}")),
        }
    }
    if let Some(level) = level {
        set_log_level(Some(&level))?;
    }
    Ok(settings.resolve())
}

use pyo3::prelude::*;

/// The level the logger ran at once importing this client had installed it.
static LEVEL_AT_IMPORT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Move the logger this client installed to `level`, or say why it was not.
///
/// `level` is a filter as `RUST_LOG` states one; `None` is the level the
/// logger ran at once importing this client installed it. A program that
/// installed its own logger keeps it, and is told so rather than told the
/// level moved.
pub(crate) fn set_log_level(level: Option<&str>) -> Result<(), String> {
    let Some(level) = level.or(LEVEL_AT_IMPORT.get().map(String::as_str)) else {
        return Err(not_this_clients("the level it was installed at"));
    };
    match crate::logging::set_level(level) {
        Ok(()) => {
            log::info!("logging at {level}");
            Ok(())
        }
        Err(crate::logging::LevelNotMoved::NotALevel) => {
            Err(format!("log_level: {level} is not a level this logger reads"))
        }
        Err(crate::logging::LevelNotMoved::NotThisClients) => Err(not_this_clients(level)),
    }
}

fn not_this_clients(level: &str) -> String {
    format!(
        "log_level: {level} was not applied because ibkr_dx did not install the \
         logger in this process; whoever did holds the level"
    )
}

/// `configure(log_level=)`'s way to the logger.
#[pyfunction]
#[pyo3(signature = (level=None))]
fn _set_log_level(level: Option<&str>) -> PyResult<()> {
    set_log_level(level).map_err(pyo3::exceptions::PyValueError::new_err)
}

/// Python module definition.
#[pymodule]
fn ibkr_dx(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Forward Rust `log::*` records wherever the environment asks for them.
    // `IBKR_DX_LOG_DIR` is published to callers as a setting, so a wheel that
    // answered it with stderr was answering something else. Both paths are
    // no-ops when a logger is already installed, which is what a module
    // initialiser wants: it runs once per interpreter, not once per process.
    let settings = crate::logging::LogConfig::from_env();
    if settings.log_dir.is_some() {
        if let Some(guard) = crate::logging::try_init(&settings) {
            guard.keep_for_the_process();
        }
    } else if crate::logging::try_init_from_env("warn")
        && let Some(level) = &settings.level
        // `IBKR_DX_LOG_LEVEL` is a setting this client publishes, and it wins
        // over `RUST_LOG` here as it does beside a log directory. Read only
        // there, it did nothing on the ordinary path to stderr; and one that
        // is not a level is said rather than passed over.
        && crate::logging::set_level(level).is_err()
    {
        log::warn!("IBKR_DX_LOG_LEVEL {level} is not a level this logger reads, so the level stays");
    }
    if let Some(level) = crate::logging::current_level() {
        let _ = LEVEL_AT_IMPORT.set(level);
    }
    m.add_function(wrap_pyfunction!(_set_log_level, m)?)?;
    compat::register(m)?;
    m.add("FIRST_RESERVED_REQUEST_ID", crate::FIRST_RESERVED_REQUEST_ID)?;
    Ok(())
}

#[cfg(test)]
mod settings_from_tests {
    /// The switch a gateway carries is honoured under the name it has there.
    ///
    /// A program migrating from a gateway carries the gateway's spelling, and
    /// a name this map does not know is refused rather than dropped — so
    /// without both spellings the caller's connect line simply fails.
    #[test]
    fn the_recovery_switch_is_taken_under_either_spelling() {
        for name in ["reconnectOnSocketErr", "reconnect_on_socket_err"] {
            let stated = std::collections::HashMap::from([
                (name.to_string(), "false".to_string()),
            ]);
            let settled = super::settings_from(stated).expect("the name is known");
            assert!(!settled.reconnect_on_socket_err, "stated off under {name}");
        }
        let on = super::settings_from(std::collections::HashMap::from([
            ("reconnectOnSocketErr".to_string(), "true".to_string()),
        ])).expect("the name is known");
        assert!(on.reconnect_on_socket_err, "and on when it says so");
    }
}

/// A collection can release the GIL while an engine is joined. Python skips
/// another collection until that pass finishes, so its completion is what
/// these checks wait for.
#[cfg(test)]
pub(crate) fn collect_until(py: Python<'_>, collected: impl Fn() -> PyResult<bool>) -> PyResult<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        py.import("gc")?.call_method0("collect")?;
        if collected()? { return Ok(()); }
        if std::time::Instant::now() >= deadline {
            return Err(pyo3::exceptions::PyAssertionError::new_err("Python cycle was not collected"));
        }
        py.detach(|| std::thread::sleep(std::time::Duration::from_millis(1)));
    }
}

#[cfg(test)]
#[pyfunction]
pub(crate) fn collect_weakref(py: Python<'_>, reference: &Bound<'_, PyAny>) -> PyResult<()> {
    collect_until(py, || Ok(reference.call0()?.is_none()))
}
