//! A session that states a log level moves the logger this client installed.
//!
//! A file of its own because a process has one logger: every other test binary
//! shares its process with whoever installed one first, and the level moved
//! here would be moved for all of them.

use ibkr_dx::settings::GatewaySettings;

/// Stated by a session opened after the logger was installed, a level moves
/// it; one that is not a level leaves it where it was, and says so on its own
/// rather than as a setting stated too late.
#[test]
fn a_session_that_states_a_level_moves_the_installed_logger() {
    assert!(ibkr_dx::logging::try_init_from_env("warn"), "this process has no logger yet");
    let stating =
        |level: &str| GatewaySettings { log_level: Some(level.into()), ..Default::default() };

    ibkr_dx::logging::apply(&stating("debug"));
    assert_eq!(ibkr_dx::logging::current_level().as_deref(), Some("debug"));

    ibkr_dx::logging::apply(&stating("info=loud"));
    assert_eq!(
        ibkr_dx::logging::current_level().as_deref(),
        Some("debug"),
        "not a level, so the level stays",
    );
}
