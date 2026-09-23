//! Every public path this crate has published, named the way a caller names it.
//!
//! A module that moves takes its paths with it unless something says otherwise,
//! and the compiler will not say so: a `pub use` deleted during a refactor
//! breaks a downstream program and nothing here. Twenty paths were lost that
//! way in one afternoon, and every one of them still had a working replacement
//! — the loss was the old name, not the code behind it.
//!
//! So the names live here. Adding one is a decision; removing one is a
//! breaking change, and this file is where that gets noticed.

#![allow(unused_imports, dead_code)]

// ── The surface a program is written against ────────────────────────────────
use ibkr_dx::api::error_codes::Refusal;
use ibkr_dx::api::reliability::{ReconnectConfig, RecoveryBudget};
use ibkr_dx::api::settings::{GatewaySettings, SessionSettings};
use ibkr_dx::api::types::{
    BarData, CommissionAndFeesReport, Contract, ContractDescription, ContractDetails,
    Execution, Order, OrderState, TagValue,
};
use ibkr_dx::api::{EClient, EClientConfig, Wrapper};
use ibkr_dx::{EClient as RootEClient, Refusal as RootRefusal};

// ── Reachable because a program already reaches it ──────────────────────────
//
// These moved during the reorganisation. The path each was published under is
// kept, so what follows is the whole of what "nothing a caller names has moved"
// means.
use ibkr_dx::client_core::{is_open_or_reactivatable, is_open_status, order_status_str};
use ibkr_dx::config::{
    IbExpiry, TimestampBuf, chrono_free_timestamp, days_to_ymd, ib_datetime_to_unix,
    midnight_days_ago, parse_ib_expiry, unix_to_ib_datetime, unix_to_ib_utc_dash,
};
use ibkr_dx::control::calendar::CalendarQuery;
use ibkr_dx::gateway::{build_mktdata_subscribe, build_mktdata_unsubscribe};
use ibkr_dx::protocol::fix::{fix_build, fix_parse, fix_read_deadline};

/// Items a `use` cannot name on its own: an associated function, and a method.
#[test]
fn every_published_name_still_resolves() {
    let _ = ibkr_dx::gateway::chrono_free_timestamp();
    let _ = ibkr_dx::gateway::days_to_ymd(0);
    let _ = ibkr_dx::client_core::ClientCore::contract_identity("", 0.0, "", "", "");
    let _ = ibkr_dx::client_core::parse_algo_params("", &[]);
    let _ = ibkr_dx::types::model::contract_identity("", 0.0, "", "", "");

    // Handing the open connections to the loop is named on the engine, where
    // what it builds lives. Named on the session module instead, that module
    // named the engine while the engine was already naming the session.
    let _built_by_the_engine = ibkr_dx::engine::hot_loop::HotLoop::for_session;
}

/// The three a caller configures, reachable from the crate root as well as
/// through `api`, because that is where a caller looks first.
#[test]
fn what_a_caller_configures_is_reachable_from_the_root() {
    let _: ibkr_dx::settings::GatewaySettings = Default::default();
    let _: ibkr_dx::reliability::ReconnectConfig = Default::default();
    let _ = ibkr_dx::error_codes::Refusal::NOT_CONNECTED;
}
