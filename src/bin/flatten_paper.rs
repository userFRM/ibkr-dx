//! Close every position a paper account is carrying.
//!
//! The compatibility suite places orders that fill and never closes them, so an
//! account it has been run against for weeks accumulates holdings until the
//! venue refuses new orders on margin grounds — a refusal that names the rule
//! rather than the order, and that reads in the suite's output as though the
//! order this client built were wrong. Emptying the account is what makes the
//! next run mean something.
//!
//! Paper only, checked twice: the session must have been opened as paper and
//! the account must be one of the venue's paper names. Neither check is a
//! formality — this places market orders.

use std::time::{Duration, Instant};

use ibkr_dx::api::client::{EClient, EClientConfig};
use ibkr_dx::api::types::Order;

fn main() {
    let _ = ibkr_dx::logging::try_init_from_env("error");
    let username = std::env::var("IB_USERNAME").unwrap_or_default();
    let password = std::env::var("IB_PASSWORD").unwrap_or_default();
    if username.trim().is_empty() || password.trim().is_empty() {
        eprintln!("IB_USERNAME/IB_PASSWORD unset. This trades against a real session.");
        std::process::exit(2);
    }

    let config = EClientConfig {
        username,
        password,
        host: std::env::var("IB_HOST").unwrap_or_default(),
        paper: true,
        ..Default::default()
    };
    let session = match EClient::connect(&config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not open a session: {e}");
            std::process::exit(1);
        }
    };

    // The account the venue named, not the one asked for. A live account
    // reached through a paper configuration would still trade.
    let account = session.accounts.first().cloned().unwrap_or_default();
    if !account.starts_with("DU") && !account.starts_with("DF") {
        eprintln!("account {account} is not a paper account; refusing to trade it");
        std::process::exit(1);
    }
    println!("account {account}");

    // The venue sends the holdings once the session settles; asking before it
    // has says the account is empty, which is the one answer that reads as
    // success and does nothing.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut held = session.positions().unwrap_or_default();
    while held.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        held = session.positions().unwrap_or_default();
    }

    let held: Vec<_> = held.into_iter().filter(|p| p.position != 0.0).collect();
    if held.is_empty() {
        println!("nothing held");
        return;
    }
    println!("{} position(s) to close", held.len());

    let mut closed = 0usize;
    for position in &held {
        let side = if position.position > 0.0 { "SELL" } else { "BUY" };
        let order = Order {
            action: side.to_string(),
            total_quantity: position.position.abs(),
            order_type: "MKT".to_string(),
            tif: "DAY".to_string(),
            ..Default::default()
        };
        let symbol = position.contract.symbol.clone();
        let order_id = session.next_order_id();
        // A refusal is heard where the order's reports are, and ends the wait.
        session.place_order(order_id, &position.contract, &order);
        match session.await_order(order_id, Duration::from_secs(30)) {
            Ok(report) => {
                println!("  {side} {} {symbol}: {}", position.position.abs(),
                    if report.is_done() { "closed" } else { "sent, still working" });
                closed += 1;
            }
            Err(why) => println!("  {side} {} {symbol}: {why}", position.position.abs()),
        }
    }
    println!("{closed} of {} placed", held.len());
}
