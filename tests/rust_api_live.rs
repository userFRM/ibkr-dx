//! The Rust surface against a live session.
//!
//! Each test connects with `EClient::connect`, asks the venue, records what
//! the wrapper is handed and asserts on it. A step skips only where the venue
//! stated a refusal under the request, which it quotes; silence fails.
//!
//! Requires IB_USERNAME and IB_PASSWORD environment variables.
//! Run with: cargo test --test rust_api_live -- --test-threads=1 --nocapture

use ibkr_dx::api::client::{Contract, EClient, EClientConfig, Order};
use ibkr_dx::api::types::*;
use ibkr_dx::api::wrapper::Wrapper;
use std::env;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ── Recording Wrapper ──

/// Every field is the callback's own payload, kept whole so a failing
/// comparison prints what actually arrived rather than the part the assertion
/// happened to name. They are read through `Debug`, which dead-code analysis
/// does not see.
#[allow(dead_code)]
#[derive(Clone, Debug)]
enum Cb {
    NextValidId {
        order_id: i64,
    },
    Error {
        req_id: i64,
        code: i64,
        msg: String,
    },
    ContractDetails {
        req_id: i64,
        contract: ContractSnapshot,
        market_name: String,
        min_tick: f64,
    },
    ContractDetailsEnd {
        req_id: i64,
    },
    SymbolSamples {
        req_id: i64,
        descriptions: Vec<ContractDescSnapshot>,
    },
    TickPrice {
        req_id: i64,
        tick_type: i32,
        price: f64,
    },
    TickSize {
        req_id: i64,
        tick_type: i32,
        size: f64,
    },
    OrderStatus {
        order_id: i64,
        status: String,
        filled: f64,
        remaining: f64,
        why_held: String,
    },
    OpenOrder {
        order_id: i64,
        contract: ContractSnapshot,
        order: OrderSnapshot,
        state: OrderStateSnapshot,
    },
    OpenOrderEnd,
    CompletedOrder {
        contract: ContractSnapshot,
        order: OrderSnapshot,
        state: OrderStateSnapshot,
    },
    CompletedOrdersEnd,
    ExecDetails {
        req_id: i64,
        contract: ContractSnapshot,
        execution: ExecSnapshot,
    },
    ExecDetailsEnd {
        req_id: i64,
    },
    Position {
        account: String,
        contract: ContractSnapshot,
        pos: f64,
        avg_cost: f64,
    },
    PositionEnd,
    AccountSummary {
        req_id: i64,
        account: String,
        tag: String,
        value: String,
        currency: String,
    },
    AccountSummaryEnd {
        req_id: i64,
    },
    Pnl {
        req_id: i64,
        daily: f64,
        unrealized: f64,
        realized: f64,
    },
    HistoricalData {
        req_id: i64,
        date: String,
    },
    HistoricalDataEnd {
        req_id: i64,
    },
    HeadTimestamp {
        req_id: i64,
        ts: String,
    },
    HistogramData {
        req_id: i64,
        count: usize,
    },
    HistoricalTicks {
        req_id: i64,
        done: bool,
    },
    ScannerParameters,
    HistoricalSchedule {
        req_id: i64,
        tz: String,
    },
    MarketRule {
        id: i64,
        count: usize,
    },
    ScannerData {
        req_id: i64,
        rank: i32,
        contract: ContractSnapshot,
    },
    ScannerDataEnd {
        req_id: i64,
    },
    FundamentalData {
        req_id: i64,
        has_data: bool,
    },
    HistoricalNews {
        req_id: i64,
        provider_code: String,
        article_id: String,
        headline: String,
    },
    HistoricalNewsEnd {
        req_id: i64,
        has_more: bool,
    },
    NewsArticle {
        req_id: i64,
        article_type: i32,
    },
    AccountValue {
        key: String,
        value: String,
        currency: String,
        account: String,
    },
    AccountDownloadEnd {
        account: String,
    },
    NewsBulletin {
        msg_id: i64,
        msg_type: i32,
        message: String,
    },
    PnlSingle {
        req_id: i64,
        pos: f64,
    },
    SmartComponents {
        req_id: i64,
        count: usize,
    },
    TickReqParams {
        bbo_exchange: String,
    },
    NewsProviders {
        count: usize,
    },
    CurrentTime {
        time: i64,
    },
    SoftDollarTiers {
        req_id: i64,
        count: usize,
    },
    FamilyCodes {
        count: usize,
    },
    UserInfo {
        req_id: i64,
        white_branding_id: String,
    },
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // the captured payload, read through Debug
struct ContractSnapshot {
    con_id: i64,
    symbol: String,
    sec_type: String,
    exchange: String,
    currency: String,
    local_symbol: String,
    trading_class: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // the captured payload, read through Debug
struct OrderSnapshot {
    order_id: i64,
    action: String,
    total_quantity: f64,
    order_type: String,
    lmt_price: f64,
    tif: String,
    account: String,
    perm_id: i64,
    outside_rth: bool,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // the captured payload, read through Debug
struct OrderStateSnapshot {
    status: String,
    completed_time: String,
    completed_status: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // the captured payload, read through Debug
struct ExecSnapshot {
    exec_id: String,
    time: String,
    acct_number: String,
    exchange: String,
    side: String,
    shares: f64,
    price: f64,
    avg_price: f64,
    cum_qty: f64,
    last_liquidity: i32,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // the captured payload, read through Debug
struct ContractDescSnapshot {
    con_id: i64,
    symbol: String,
    sec_type: String,
    currency: String,
}

fn snap_contract(c: &ibkr_dx::api::types::Contract) -> ContractSnapshot {
    ContractSnapshot {
        con_id: c.con_id,
        symbol: c.symbol.clone(),
        sec_type: c.sec_type.clone(),
        exchange: c.exchange.clone(),
        currency: c.currency.clone(),
        local_symbol: c.local_symbol.clone(),
        trading_class: c.trading_class.clone(),
    }
}

struct RecWrapper {
    events: Mutex<Vec<Cb>>,
}

impl RecWrapper {
    fn new() -> Self {
        Self { events: Mutex::new(Vec::new()) }
    }
    fn push(&self, cb: Cb) {
        self.events.lock().unwrap().push(cb);
    }
    fn drain(&self) -> Vec<Cb> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

impl Wrapper for RecWrapper {
    fn next_valid_id(&mut self, order_id: i64) {
        self.push(Cb::NextValidId { order_id });
    }
    fn error(&mut self, req_id: i64, error_code: i64, error_string: &str, _: &str) {
        self.push(Cb::Error { req_id, code: error_code, msg: error_string.into() });
    }
    fn contract_details(&mut self, req_id: i64, details: &ContractDetails) {
        self.push(Cb::ContractDetails {
            req_id,
            contract: snap_contract(&details.contract),
            market_name: details.market_name.clone(),
            min_tick: details.min_tick,
        });
    }
    fn contract_details_end(&mut self, req_id: i64) {
        self.push(Cb::ContractDetailsEnd { req_id });
    }
    fn symbol_samples(&mut self, req_id: i64, descriptions: &[ContractDescription]) {
        self.push(Cb::SymbolSamples {
            req_id,
            descriptions: descriptions
                .iter()
                .map(|d| ContractDescSnapshot {
                    con_id: d.con_id,
                    symbol: d.symbol.clone(),
                    sec_type: d.sec_type.clone(),
                    currency: d.currency.clone(),
                })
                .collect(),
        });
    }
    fn tick_price(&mut self, req_id: i64, tick_type: i32, price: f64, _: &TickAttrib) {
        self.push(Cb::TickPrice { req_id, tick_type, price });
    }
    fn tick_size(&mut self, req_id: i64, tick_type: i32, size: f64) {
        self.push(Cb::TickSize { req_id, tick_type, size });
    }
    fn order_status(
        &mut self,
        order_id: i64,
        status: &str,
        filled: f64,
        remaining: f64,
        _: f64,
        _: i64,
        _: i64,
        _: f64,
        _: i64,
        why_held: &str,
        _: f64,
    ) {
        self.push(Cb::OrderStatus {
            order_id,
            status: status.into(),
            filled,
            remaining,
            why_held: why_held.into(),
        });
    }
    fn open_order(
        &mut self,
        order_id: i64,
        contract: &ibkr_dx::api::types::Contract,
        order: &ibkr_dx::api::types::Order,
        state: &OrderState,
    ) {
        self.push(Cb::OpenOrder {
            order_id,
            contract: snap_contract(contract),
            order: OrderSnapshot {
                order_id: order.order_id,
                action: order.action.clone(),
                total_quantity: order.total_quantity,
                order_type: order.order_type.clone(),
                lmt_price: order.lmt_price,
                tif: order.tif.clone(),
                account: order.account.clone(),
                perm_id: order.perm_id,
                outside_rth: order.outside_rth,
            },
            state: OrderStateSnapshot {
                status: state.status.clone(),
                completed_time: state.completed_time.clone(),
                completed_status: state.completed_status.clone(),
            },
        });
    }
    fn open_order_end(&mut self) {
        self.push(Cb::OpenOrderEnd);
    }
    fn completed_order(
        &mut self,
        contract: &ibkr_dx::api::types::Contract,
        order: &ibkr_dx::api::types::Order,
        state: &OrderState,
    ) {
        self.push(Cb::CompletedOrder {
            contract: snap_contract(contract),
            order: OrderSnapshot {
                order_id: order.order_id,
                action: order.action.clone(),
                total_quantity: order.total_quantity,
                order_type: order.order_type.clone(),
                lmt_price: order.lmt_price,
                tif: order.tif.clone(),
                account: order.account.clone(),
                perm_id: order.perm_id,
                outside_rth: order.outside_rth,
            },
            state: OrderStateSnapshot {
                status: state.status.clone(),
                completed_time: state.completed_time.clone(),
                completed_status: state.completed_status.clone(),
            },
        });
    }
    fn completed_orders_end(&mut self) {
        self.push(Cb::CompletedOrdersEnd);
    }
    fn exec_details(
        &mut self,
        req_id: i64,
        contract: &ibkr_dx::api::types::Contract,
        execution: &Execution,
    ) {
        self.push(Cb::ExecDetails {
            req_id,
            contract: snap_contract(contract),
            execution: ExecSnapshot {
                exec_id: execution.exec_id.clone(),
                time: execution.time.clone(),
                acct_number: execution.acct_number.clone(),
                exchange: execution.exchange.clone(),
                side: execution.side.clone(),
                shares: execution.shares,
                price: execution.price,
                avg_price: execution.avg_price,
                cum_qty: execution.cum_qty,
                last_liquidity: execution.last_liquidity,
            },
        });
    }
    fn exec_details_end(&mut self, req_id: i64) {
        self.push(Cb::ExecDetailsEnd { req_id });
    }
    fn position(
        &mut self,
        account: &str,
        contract: &ibkr_dx::api::types::Contract,
        pos: f64,
        avg_cost: f64,
    ) {
        self.push(Cb::Position {
            account: account.into(),
            contract: snap_contract(contract),
            pos,
            avg_cost,
        });
    }
    fn position_end(&mut self) {
        self.push(Cb::PositionEnd);
    }
    fn account_summary(
        &mut self,
        req_id: i64,
        account: &str,
        tag: &str,
        value: &str,
        currency: &str,
    ) {
        self.push(Cb::AccountSummary {
            req_id,
            account: account.into(),
            tag: tag.into(),
            value: value.into(),
            currency: currency.into(),
        });
    }
    fn account_summary_end(&mut self, req_id: i64) {
        self.push(Cb::AccountSummaryEnd { req_id });
    }
    fn pnl(&mut self, req_id: i64, daily: f64, unrealized: f64, realized: f64) {
        self.push(Cb::Pnl { req_id, daily, unrealized, realized });
    }
    fn historical_data(&mut self, req_id: i64, bar: &BarData) {
        self.push(Cb::HistoricalData { req_id, date: bar.date.clone() });
    }
    fn historical_data_end(&mut self, req_id: i64, _: &str, _: &str) {
        self.push(Cb::HistoricalDataEnd { req_id });
    }
    fn head_timestamp(&mut self, req_id: i64, ts: &str) {
        self.push(Cb::HeadTimestamp { req_id, ts: ts.into() });
    }
    fn histogram_data(&mut self, req_id: i64, items: &[(f64, i64)]) {
        self.push(Cb::HistogramData { req_id, count: items.len() });
    }
    fn historical_ticks(
        &mut self,
        req_id: i64,
        _: &ibkr_dx::types::HistoricalTickData,
        done: bool,
    ) {
        self.push(Cb::HistoricalTicks { req_id, done });
    }
    fn scanner_parameters(&mut self, _: &str) {
        self.push(Cb::ScannerParameters);
    }
    fn historical_schedule(
        &mut self,
        req_id: i64,
        _: &str,
        _: &str,
        tz: &str,
        _: &[(String, String, String)],
    ) {
        self.push(Cb::HistoricalSchedule { req_id, tz: tz.into() });
    }
    fn scanner_data(
        &mut self,
        req_id: i64,
        rank: i32,
        details: &ContractDetails,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
    ) {
        self.push(Cb::ScannerData { req_id, rank, contract: snap_contract(&details.contract) });
    }
    fn scanner_data_end(&mut self, req_id: i64) {
        self.push(Cb::ScannerDataEnd { req_id });
    }
    fn fundamental_data(&mut self, req_id: i64, data: &str) {
        self.push(Cb::FundamentalData { req_id, has_data: !data.is_empty() });
    }
    fn historical_news(
        &mut self,
        req_id: i64,
        _: &str,
        provider_code: &str,
        article_id: &str,
        headline: &str,
    ) {
        self.push(Cb::HistoricalNews {
            req_id,
            provider_code: provider_code.into(),
            article_id: article_id.into(),
            headline: headline.into(),
        });
    }
    fn historical_news_end(&mut self, req_id: i64, has_more: bool) {
        self.push(Cb::HistoricalNewsEnd { req_id, has_more });
    }
    fn news_article(&mut self, req_id: i64, article_type: i32, _: &str) {
        self.push(Cb::NewsArticle { req_id, article_type });
    }
    fn update_account_value(&mut self, key: &str, value: &str, currency: &str, account: &str) {
        self.push(Cb::AccountValue {
            key: key.into(),
            value: value.into(),
            currency: currency.into(),
            account: account.into(),
        });
    }
    fn account_download_end(&mut self, account: &str) {
        self.push(Cb::AccountDownloadEnd { account: account.into() });
    }
    fn update_news_bulletin(&mut self, msg_id: i64, msg_type: i32, message: &str, _: &str) {
        self.push(Cb::NewsBulletin { msg_id, msg_type, message: message.into() });
    }
    fn pnl_single(&mut self, req_id: i64, pos: f64, _: f64, _: f64, _: f64, _: f64) {
        self.push(Cb::PnlSingle { req_id, pos });
    }
    fn market_rule(&mut self, id: i64, increments: &[PriceIncrement]) {
        self.push(Cb::MarketRule { id, count: increments.len() });
    }
    fn smart_components(&mut self, req_id: i64, components: &[ibkr_dx::types::SmartComponent]) {
        self.push(Cb::SmartComponents { req_id, count: components.len() });
    }
    fn tick_req_params(&mut self, _: i64, _: f64, bbo_exchange: &str, _: i64) {
        self.push(Cb::TickReqParams { bbo_exchange: bbo_exchange.into() });
    }
    fn news_providers(&mut self, providers: &[ibkr_dx::types::NewsProvider]) {
        self.push(Cb::NewsProviders { count: providers.len() });
    }
    fn current_time(&mut self, time: i64) {
        self.push(Cb::CurrentTime { time });
    }
    fn soft_dollar_tiers(&mut self, req_id: i64, tiers: &[ibkr_dx::types::SoftDollarTier]) {
        self.push(Cb::SoftDollarTiers { req_id, count: tiers.len() });
    }
    fn family_codes(&mut self, codes: &[ibkr_dx::types::FamilyCode]) {
        self.push(Cb::FamilyCodes { count: codes.len() });
    }
    fn user_info(&mut self, req_id: i64, white_branding_id: &str) {
        self.push(Cb::UserInfo { req_id, white_branding_id: white_branding_id.into() });
    }
}

// ── Helpers ──

fn get_config() -> Option<EClientConfig> {
    let username = env::var("IB_USERNAME").ok()?;
    let password = env::var("IB_PASSWORD").ok()?;
    let host = env::var("IB_HOST").unwrap_or_else(|_| "cdc1.ibllc.com".to_string());
    Some(EClientConfig {
        username,
        password,
        host,
        paper: true,
        core_id: None,
        code_provider: None,
        ..Default::default()
    })
}

fn spy() -> Contract {
    Contract {
        con_id: 756733,
        symbol: "SPY".into(),
        sec_type: "STK".into(),
        exchange: "SMART".into(),
        currency: "USD".into(),
        ..Default::default()
    }
}

fn aapl() -> Contract {
    Contract {
        con_id: 265598,
        symbol: "AAPL".into(),
        sec_type: "STK".into(),
        exchange: "SMART".into(),
        currency: "USD".into(),
        ..Default::default()
    }
}

fn poll(client: &EClient, wrapper: &mut RecWrapper, duration: Duration) {
    let start = Instant::now();
    while start.elapsed() < duration {
        client.process_msgs(wrapper);
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn poll_until(
    client: &EClient,
    wrapper: &mut RecWrapper,
    pred: impl Fn(&[Cb]) -> bool,
    timeout: Duration,
) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        client.process_msgs(wrapper);
        if pred(&wrapper.events.lock().unwrap()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether anything was said under a request, which ends the wait for it.
fn said_under(cbs: &[Cb], req_id: i64) -> bool {
    cbs.iter().any(|c| matches!(c, Cb::Error { req_id: r, .. } if *r == req_id))
}

/// Why the venue would not answer a request, where it declined it: data the
/// account is not subscribed to (354), the historical service's refusal (162),
/// a subscription it refused, said in those words, or the notice that a data
/// connection dropped. A refusal this client makes is none of these, and fails
/// the step it stops. Nothing else here is a reason not to have an answer.
fn refused_under(cbs: &[Cb], req_id: i64) -> Option<String> {
    cbs.iter().find_map(|c| match c {
        Cb::Error { req_id: r, code, msg }
            if *r == req_id
                && (matches!(code, 354 | 162) || msg.starts_with("the venue refused")) =>
        {
            Some(format!("{code} {msg}"))
        }
        Cb::Error { req_id: -1, code: code @ (2103 | 2105), msg } => Some(format!("{code} {msg}")),
        _ => None,
    })
}

/// The calls no other live test names, each asked of the venue and each
/// asserted: what arrived is checked, a refusal the venue stated under the
/// request is said and quoted, and silence fails.
#[test]
fn the_calls_no_other_live_test_names() {
    let _ = ibkr_dx::logging::try_init_from_env("error");
    let Some(config) = get_config() else {
        println!("Skipping: IB credentials not set");
        return;
    };
    let client = EClient::connect(&config).expect("EClient::connect failed");
    let mut wrapper = RecWrapper::new();
    // The session's opening burst, read before anything is asked.
    poll(&client, &mut wrapper, Duration::from_secs(5));
    wrapper.drain();

    // Answered from what the logon stated, at once.
    client.req_soft_dollar_tiers(901, &mut wrapper);
    client.req_family_codes(&mut wrapper);
    client.req_user_info(902, &mut wrapper);
    let cbs = wrapper.drain();
    assert!(
        cbs.iter().any(|c| matches!(c, Cb::SoftDollarTiers { req_id: 901, count } if *count > 0)),
        "the soft dollar tiers the logon states: {cbs:?}",
    );
    assert!(cbs.iter().any(|c| matches!(c, Cb::FamilyCodes { .. })), "the family codes: {cbs:?}");
    assert!(
        cbs.iter().any(|c| matches!(c, Cb::UserInfo { req_id: 902, .. })),
        "the user info: {cbs:?}"
    );

    // The venues behind a quote's exchange mask, named by the BBO exchange the
    // subscription's acknowledgement states, so a quote is asked for first.
    client.req_mkt_data(903, &spy(), "", false, false).unwrap();
    poll_until(
        &client,
        &mut wrapper,
        |cbs| {
            cbs.iter().any(
                |c| matches!(c, Cb::TickReqParams { bbo_exchange } if !bbo_exchange.is_empty()),
            )
        },
        Duration::from_secs(15),
    );
    client.cancel_mkt_data(903).unwrap();
    let cbs = wrapper.drain();
    let bbo = cbs.iter().find_map(|c| match c {
        Cb::TickReqParams { bbo_exchange } if !bbo_exchange.is_empty() => {
            Some(bbo_exchange.clone())
        }
        _ => None,
    });
    match (bbo, refused_under(&cbs, 903)) {
        (Some(bbo), _) => {
            client.req_smart_components(900, &bbo, &mut wrapper);
            // A map that has not arrived is answered from the dispatch loop
            // once it does, within the two seconds a gateway waits.
            poll_until(
                &client,
                &mut wrapper,
                |cbs| cbs.iter().any(|c| matches!(c, Cb::SmartComponents { req_id: 900, .. })),
                Duration::from_secs(3),
            );
            let cbs = wrapper.drain();
            assert!(
                cbs.iter()
                    .any(|c| matches!(c, Cb::SmartComponents { req_id: 900, count } if *count > 0)),
                "the venues behind {bbo}: {cbs:?}",
            );
        }
        (None, Some(why)) => {
            println!("smart components: the quote was refused, as the venue said: {why}")
        }
        (None, None) => {
            panic!("a quote subscription was acknowledged with no BBO exchange: {cbs:?}")
        }
    }

    // When a contract trades: a historical-service question, answered at any
    // hour.
    client.req_historical_schedule(450, &spy(), "", "1 M", true).unwrap();
    poll_until(
        &client,
        &mut wrapper,
        |cbs| {
            cbs.iter().any(|c| matches!(c, Cb::HistoricalSchedule { req_id: 450, .. }))
                || said_under(cbs, 450)
        },
        Duration::from_secs(15),
    );
    let cbs = wrapper.drain();
    let zone = cbs.iter().find_map(|c| match c {
        Cb::HistoricalSchedule { req_id: 450, tz } => Some(tz.clone()),
        _ => None,
    });
    match (zone, refused_under(&cbs, 450)) {
        (Some(tz), _) => assert!(!tz.is_empty(), "a schedule states its time zone: {cbs:?}"),
        (None, Some(why)) => println!("historical schedule refused, as the venue said: {why}"),
        (None, None) => panic!("the venue said nothing to a trading schedule within 15s: {cbs:?}"),
    }

    // Past trades from a moment: history, answered at any hour.
    client
        .req_historical_ticks(440, &spy(), "20260320 09:30:00", "", 1000, "TRADES", true)
        .unwrap();
    poll_until(
        &client,
        &mut wrapper,
        |cbs| {
            cbs.iter().any(|c| matches!(c, Cb::HistoricalTicks { req_id: 440, done: true }))
                || said_under(cbs, 440)
        },
        Duration::from_secs(15),
    );
    let cbs = wrapper.drain();
    let ended = cbs.iter().any(|c| matches!(c, Cb::HistoricalTicks { req_id: 440, done: true }));
    match (ended, refused_under(&cbs, 440)) {
        (true, _) => {}
        (false, Some(why)) => println!("historical ticks refused, as the venue said: {why}"),
        (false, None) => {
            panic!("the venue said nothing to a historical ticks request within 15s: {cbs:?}")
        }
    }

    // A fundamental report is asked for by the venue's id for the contract, so
    // the contract is named first: asked for one this session has not named,
    // the request is refused here before the venue hears of it.
    client.req_contract_details(469, &aapl()).unwrap();
    poll_until(
        &client,
        &mut wrapper,
        |cbs| {
            cbs.iter().any(|c| {
                matches!(c, Cb::ContractDetailsEnd { req_id: 469 } | Cb::Error { req_id: 469, .. })
            })
        },
        Duration::from_secs(20),
    );
    let cbs = wrapper.drain();
    assert!(
        cbs.iter().any(|c| matches!(c, Cb::ContractDetails { req_id: 469, .. })),
        "the venue names a contract at any hour: {cbs:?}",
    );
    client.req_fundamental_data(470, &aapl(), "ReportSnapshot").unwrap();
    poll_until(
        &client,
        &mut wrapper,
        |cbs| {
            cbs.iter().any(|c| matches!(c, Cb::FundamentalData { req_id: 470, .. }))
                || said_under(cbs, 470)
        },
        Duration::from_secs(20),
    );
    let cbs = wrapper.drain();
    let report = cbs.iter().find_map(|c| match c {
        Cb::FundamentalData { req_id: 470, has_data } => Some(*has_data),
        _ => None,
    });
    match (report, refused_under(&cbs, 470)) {
        (Some(has_data), _) => assert!(has_data, "a report states something: {cbs:?}"),
        (None, Some(why)) => println!("fundamental data refused, as the venue said: {why}"),
        (None, None) => {
            panic!("the venue said nothing to a fundamental report within 20s: {cbs:?}")
        }
    }
    // And one withdrawn as soon as it is asked for.
    client.req_fundamental_data(471, &aapl(), "ReportSnapshot").unwrap();
    client.cancel_fundamental_data(471).unwrap();
    poll(&client, &mut wrapper, Duration::from_secs(2));

    client.disconnect();
    assert!(!client.is_connected(), "a disconnected session says so");
}

/// Calls the client serves that no suite exercised.
///
/// Two of them answered on the callbacks belonging to the requests without
/// "multi" in the name, and a third returned in silence when it had nothing
/// cached. Nothing caught it because nothing called them. This does.
#[test]
fn reference_and_account_calls_live() {
    let _ = ibkr_dx::logging::try_init_from_env("error");
    let Some(config) = get_config() else {
        println!("Skipping: IB credentials not set");
        return;
    };
    let client = EClient::connect(&config).expect("EClient::connect failed");

    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Heard {
        accounts: Vec<String>,
        depth_venues: usize,
        rule_refusals: Vec<String>,
        positions_multi: Vec<(i64, String)>,
        positions_multi_end: Vec<i64>,
        values_multi: Vec<(i64, String)>,
        values_multi_end: Vec<i64>,
    }
    struct W(Arc<Mutex<Heard>>);
    impl ibkr_dx::api::wrapper::Wrapper for W {
        fn managed_accounts(&mut self, accounts: &str) {
            self.0.lock().unwrap().accounts.push(accounts.to_string());
        }
        fn mkt_depth_exchanges(&mut self, d: &[ibkr_dx::types::DepthMktDataDescription]) {
            self.0.lock().unwrap().depth_venues += d.len();
        }
        fn position_multi(
            &mut self,
            req_id: i64,
            account: &str,
            _model: &str,
            _c: &Contract,
            _pos: f64,
            _avg: f64,
        ) {
            self.0.lock().unwrap().positions_multi.push((req_id, account.to_string()));
        }
        fn position_multi_end(&mut self, req_id: i64) {
            self.0.lock().unwrap().positions_multi_end.push(req_id);
        }
        fn account_update_multi(
            &mut self,
            req_id: i64,
            _account: &str,
            _model: &str,
            key: &str,
            _value: &str,
            _currency: &str,
        ) {
            self.0.lock().unwrap().values_multi.push((req_id, key.to_string()));
        }
        fn account_update_multi_end(&mut self, req_id: i64) {
            self.0.lock().unwrap().values_multi_end.push(req_id);
        }
        fn error(&mut self, req_id: i64, _code: i64, message: &str, _: &str) {
            if req_id == 999_777 {
                self.0.lock().unwrap().rule_refusals.push(message.to_string());
            }
        }
    }

    let heard = Arc::new(Mutex::new(Heard::default()));
    let mut w = W(Arc::clone(&heard));

    // None of these answers anything; they must simply not fall over.
    client.set_server_log_level(2);
    client.req_market_data_type(1);
    client.req_auto_open_orders(true);
    client.cancel_positions();
    client.cancel_account_updates_multi(1);
    client.cancel_positions_multi(1);

    client.req_managed_accts(&mut w);
    // A rule this session cannot have seen, so the refusal is the answer.
    client.req_market_rule(999_777, &mut w);
    client.req_positions_multi(9101, "", "", &mut w);
    client.req_account_updates_multi(9102, "", "", true, &mut w);
    let _ = client.req_mkt_depth_exchanges();

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        client.process_msgs(&mut w);
        if heard.lock().unwrap().depth_venues > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let h = heard.lock().unwrap();
    assert!(!h.accounts.is_empty(), "the session names the account it manages");
    // Every account the login holds, comma separated. This login holds one, so
    // it is answered with one name and no comma; a login holding several is
    // answered with each of them.
    for answer in &h.accounts {
        assert!(!answer.starts_with(','), "an account list does not lead with a comma: {answer:?}");
        for account in answer.split(',') {
            assert!(!account.trim().is_empty(), "every name in the list is an account: {answer:?}");
        }
    }
    assert_eq!(h.rule_refusals.len(), 1, "a rule never seen says so: {:?}", h.rule_refusals);
    assert_eq!(h.positions_multi_end, vec![9101], "holdings answer on their own callback");
    assert_eq!(h.values_multi_end, vec![9102], "account values answer on their own callback");
    assert!(!h.values_multi.is_empty(), "and state something");
    assert!(h.depth_venues > 0, "the venues carrying depth are named");
    assert!(h.positions_multi.iter().all(|(r, _)| *r == 9101), "under the request that asked");
    assert!(h.values_multi.iter().all(|(r, _)| *r == 9102), "under the request that asked");
    drop(h);
    client.disconnect();
}

/// Everything the venue sends this session is read.
///
/// This client claims to replace the vendor's gateway. That is a claim about
/// messages, and it is checkable: exercise every path a caller has, then ask
/// what arrived that nothing read. Anything listed is a wire this client is
/// discarding — which is how the option chain, the venue's error channel,
/// the algorithms it offers and the holdings held away were all found, each
/// having arrived unread for as long as this client has existed.
#[test]
fn the_venue_sends_nothing_this_client_does_not_read() {
    let _ = ibkr_dx::logging::try_init_from_env("error");
    let Some(config) = get_config() else {
        println!("Skipping: IB credentials not set");
        return;
    };
    let client = EClient::connect(&config).expect("EClient::connect failed");
    let mut w = RecWrapper::new();

    // Everything a caller can ask for, so the venue has reason to send
    // everything it would ever send.
    let spy = spy();
    client.req_mkt_data(1, &spy, "", false, false).unwrap();
    let _ = client.req_mkt_depth(2, &spy, 5, false);
    client.req_contract_details(3, &spy).unwrap();
    client.req_positions(&mut w);
    client.req_account_summary(4, "All", "NetLiquidation,BuyingPower");
    client.req_open_orders(&mut w);
    client.req_executions(5, &Default::default(), &mut w);
    let _ = client.req_historical_data(6, &spy, "", "1 D", "1 hour", "TRADES", true, 1, false);
    let _ = client.req_sec_def_opt_params(7, "SPY", "", "STK", 756733);
    client.req_news_bulletins(true);

    // An order through its whole life, which is when the venue says most.
    let order_id = 70_000 + (std::process::id() as i64 % 9_000);
    let resting = Order {
        action: "BUY".into(),
        total_quantity: 1.0,
        order_type: "LMT".into(),
        lmt_price: 1.0,
        tif: "GTC".into(),
        outside_rth: true,
        ..Default::default()
    };
    let _ = client.place_order(order_id, &spy, &resting);
    poll(&client, &mut w, Duration::from_secs(20));
    let _ = client.place_order(order_id, &spy, &Order { lmt_price: 2.0, ..resting });
    poll(&client, &mut w, Duration::from_secs(10));
    let _ = client.cancel_order(order_id, "");
    poll(&client, &mut w, Duration::from_secs(20));

    let unread = client.unread_wire();
    client.disconnect();

    assert!(
        unread.is_empty(),
        "the venue sent {} kind(s) of message this client does not read: {unread:?}. \
         Each is a wire being discarded — read it, or record in `known_unread` why it \
         carries nothing a caller could use",
        unread.len(),
    );
}
