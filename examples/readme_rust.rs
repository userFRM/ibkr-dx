//! The Rust example from the README, run against the venue.
//!
//! Kept as an example so it is compiled with everything else: a snippet in a
//! README that no longer builds is a snippet that turns readers away.
//!
//!     IB_USERNAME=… IB_PASSWORD=… cargo run --example readme_rust

use ibkr_dx::api::client::{EClient, EClientConfig};
use ibkr_dx::api::types::{Contract, Order};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = EClient::connect(&EClientConfig {
        username: std::env::var("IB_USERNAME").unwrap_or_default(),
        password: std::env::var("IB_PASSWORD").unwrap_or_default(),
        paper: true,
        ..Default::default()
    })
    ?;

    let spy = client.qualify_contract(&Contract {
        symbol: "SPY".into(),
        sec_type: "STK".into(),
        exchange: "SMART".into(),
        currency: "USD".into(),
        ..Default::default()
    })?;

    let bars = client.historical_data(&spy, "", "2 D", "1 hour", "TRADES", true)?;
    let preview = client.what_if_order(&spy, &Order {
        action: "BUY".into(),
        order_type: "LMT".into(),
        total_quantity: 1.0,
        lmt_price: 1.0,
        ..Default::default()
    })?;
    println!("{} bars, preview {}", bars.len(), preview.status);

    client.req_mkt_data(1, &spy, "", false, false)?;
    std::thread::sleep(std::time::Duration::from_secs(2));
    if let Some(quote) = client.quote(1) {
        // Prices are held as integers scaled by `PRICE_SCALE`.
        let scale = ibkr_dx::types::PRICE_SCALE as f64;
        println!("bid {:.2} ask {:.2}", quote.bid as f64 / scale, quote.ask as f64 / scale);
    }

    client.disconnect();
    Ok(())
}
