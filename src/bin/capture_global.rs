//! Exercise this client against several continents at once.
//!
//! A capability proved on one American share is proved for one American share.
//! The venue routes differently per region, states different increments, keeps
//! different hours and names its exchanges differently — so a client meant to
//! replace a gateway has to be shown working somewhere other than New York.
//!
//! Reads only, apart from order previews, which the venue prices and does not
//! place.
//!
//!     IB_USERNAME=… IB_PASSWORD=… cargo run --features dev-tools --bin capture_global

use std::time::{Duration, Instant};

use ibkr_dx::api::client::{EClient, EClientConfig};
use ibkr_dx::api::types::{Contract, Order};
use ibkr_dx::api::wrapper::Wrapper;

/// The quantity a preview asks about: the step the venue deals the contract
/// in, or its least size where it states no step, and never below one.
///
/// A Japanese or a Hong Kong share is dealt in board lots, and an order for
/// one share of either is refused by the exchange for its size — which says
/// nothing about whether the order itself is one the venue takes.
fn lot(details: &ibkr_dx::types::model::ContractDetails) -> f64 {
    let stated = |v: f64| v.is_finite() && v > 0.0 && v != f64::MAX;
    [details.size_increment, details.min_size]
        .into_iter()
        .find(|v| stated(*v))
        .unwrap_or(1.0)
        .max(1.0)
}

/// One contract per market this account can reach.
fn subjects() -> Vec<(&'static str, Contract)> {
    let equity = |symbol: &str, exchange: &'static str, currency: &'static str| Contract {
        symbol: symbol.to_string(),
        sec_type: "STK".to_string(),
        exchange: exchange.to_string(),
        currency: currency.to_string(),
        ..Default::default()
    };
    vec![
        ("a German share", equity("SAP", "IBIS", "EUR")),
        ("a Dutch share", equity("ASML", "AEB", "EUR")),
        ("a British share", equity("VOD", "LSE", "GBP")),
        ("a Swiss share", equity("NESN", "EBS", "CHF")),
        ("a Japanese share", equity("7203", "TSEJ", "JPY")),
        ("a Hong Kong share", equity("700", "SEHK", "HKD")),
        ("an Australian share", equity("BHP", "ASX", "AUD")),
        ("a Canadian share", equity("RY", "TSE", "CAD")),
        ("an American share", equity("AAPL", "SMART", "USD")),
        ("a currency pair", Contract {
            symbol: "EUR".to_string(), sec_type: "CASH".to_string(),
            exchange: "IDEALPRO".to_string(), currency: "USD".to_string(),
            ..Default::default()
        }),
        ("a European future", Contract {
            symbol: "ESTX50".to_string(), sec_type: "FUT".to_string(),
            exchange: "EUREX".to_string(), currency: "EUR".to_string(),
            last_trade_date_or_contract_month: "202609".to_string(),
            ..Default::default()
        }),
        ("an American index", Contract {
            symbol: "SPX".to_string(), sec_type: "IND".to_string(),
            exchange: "CBOE".to_string(), currency: "USD".to_string(),
            ..Default::default()
        }),
    ]
}

#[derive(Default)]
struct Heard {
    bars: usize,
    previews: usize,
    said: Vec<String>,
}

impl Wrapper for Heard {
    fn historical_data(&mut self, _req: i64, _bar: &ibkr_dx::api::types::BarData) {
        self.bars += 1;
    }
    fn open_order(
        &mut self, _id: i64, _c: &Contract, _o: &Order,
        _state: &ibkr_dx::api::types::OrderState,
    ) {
        self.previews += 1;
    }
    fn error(&mut self, _req: i64, code: i64, message: &str, _adv: &str) {
        if code != 2104 && code != 2106 && code != 2158 {
            self.said.push(format!("{code}: {message}"));
        }
    }
}

fn main() {
    let _ = ibkr_dx::logging::try_init_from_env("error");
    let username = std::env::var("IB_USERNAME").unwrap_or_default();
    if username.trim().is_empty() {
        eprintln!("IB_USERNAME/IB_PASSWORD unset. This reads from real servers.");
        std::process::exit(2);
    }
    let client = match EClient::connect(&EClientConfig {
        username,
        password: std::env::var("IB_PASSWORD").unwrap_or_default(),
        paper: true,
        ..Default::default()
    }) {
        Ok(c) => c,
        Err(e) => { eprintln!("could not open a session: {e}"); std::process::exit(1); }
    };
    println!("session open\n");
    println!(
        "{:<22} {:>10}  {:>9}  {:>7}  {:>5}  what the venue said",
        "market", "contract", "quote", "bars", "order",
    );

    let mut heard = Heard::default();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() % 90_000) as i64 * 10)
        .unwrap_or(7000);

    for (n, (what, contract)) in subjects().into_iter().enumerate() {
        let req = n as i64 + 1;
        let details = match client.contract_details(&contract) {
            Ok(found) if !found.is_empty() => found.into_iter().next().unwrap(),
            Ok(_) => {
                println!("{what:<22} no: the venue described no contract");
                continue;
            }
            Err(e) => {
                println!("{what:<22} no: {} ({})", first_line(&e.message), e.code);
                continue;
            }
        };
        let resolved = details.contract.clone();
        let quantity = lot(&details);

        // Top of book.
        client.req_mkt_data(req, &resolved, "", false, false);
        // Bars, which every market keeps whether or not it is open now.
        client.req_historical_data(
            1000 + req, &resolved, "", "2 D", "1 hour", "TRADES", false, 1, false,
        );
        // And an order the venue prices without placing.
        let priced = resolved.sec_type != "IND";
        if priced {
            let order = Order {
                action: "BUY".into(),
                order_type: "LMT".into(),
                total_quantity: quantity,
                lmt_price: 1.0,
                what_if: true,
                ..Default::default()
            };
            client.place_order(stamp + req, &resolved, &order);
        }

        let before_bars = heard.bars;
        let before_previews = heard.previews;
        heard.said.clear();
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline {
            client.process_msgs(&mut heard);
            std::thread::sleep(Duration::from_millis(150));
        }

        let quote = client
            .instrument_of(resolved.con_id)
            .map(|i| client.shared_state().market.quote(i))
            .filter(|q| q.bid > 0 || q.ask > 0)
            .map(|q| format!("{:.4}", q.bid as f64 / 1e8))
            .unwrap_or_else(|| "—".to_string());
        let bars = heard.bars - before_bars;
        let priced_ok = if !priced {
            "n/a".to_string()
        } else if heard.previews > before_previews {
            "yes".to_string()
        } else {
            "no".to_string()
        };
        let said = heard.said.first().map(|s| first_line(s)).unwrap_or_default();
        println!(
            "{what:<22} {:>10}  {quote:>9}  {bars:>7}  {priced_ok:>5}  {said}",
            resolved.con_id,
        );
        // Dealt in lots, the same order for one share, so what the exchange
        // says of an odd lot is recorded beside the answer at its lot.
        if priced && quantity > 1.0 {
            heard.said.clear();
            let odd = Order {
                action: "BUY".into(), order_type: "LMT".into(), total_quantity: 1.0,
                lmt_price: 1.0, what_if: true, ..Default::default()
            };
            client.place_order(stamp + 100 + req, &resolved, &odd);
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                client.process_msgs(&mut heard);
                std::thread::sleep(Duration::from_millis(150));
            }
            let said = heard.said.first().map(|s| first_line(s)).unwrap_or_default();
            println!("{:<22} at {quantity} a lot; at one: {said}", "");
        }
        client.cancel_mkt_data(req);
    }

    client.disconnect();
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(64).collect()
}
