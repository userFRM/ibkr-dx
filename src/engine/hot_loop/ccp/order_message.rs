//! What a gateway tells the program that placed an order when the venue
//! states a message beside the order's status: error 399, *Order Message:*,
//! a line describing the order the way a gateway's display writes it, and a
//! line of the venue's message.
//!
//! The lines are joined by line breaks, and a gateway writes every line break
//! of an error's text as a space on the way to a program, so a program reads
//! one line.

use crate::control::contracts::{ContractDefinition, MarketRule, OptionRight, SecurityType};
use crate::client_core::cash_quantity::{self, listed, rounded_half_even};
use crate::types::Side;
use crate::types::orders::ord_type_api_name;

/// The code a gateway states a message about an order under.
pub(super) const ORDER_MESSAGE: i32 = 399;

/// The code a gateway states the venue's price-management question under.
const PRICE_MANAGEMENT: i32 = 10212;

/// What an order is, as a message about it describes it.
pub(super) struct Described<'a> {
    /// Which way it goes.
    pub side: Side,
    /// How much, as the decimal the order states.
    pub quantity: &'a str,
    /// The most a ladder holds, where the order is a ladder sized by its
    /// components: the figure the order went to the venue with.
    pub ladder: Option<i64>,
    /// The amount of money it is for, where it states one.
    pub cash: Option<f64>,
    /// The contract, as the venue defined it.
    pub contract: &'a ContractDefinition,
    /// The price and size rule the contract trades under.
    pub rule: Option<&'a MarketRule>,
    /// Whether the logon has a listing's market read whole rather than cut at
    /// its segment (SEPLSTDIV).
    pub whole_listing: bool,
    /// Whether the logon has a bond's ISIN named beside the CUSIP it holds
    /// (CUSIPD).
    pub isin_with_cusip: bool,
    /// The order's type as the venue names it (tag 40).
    pub order_type: &'a str,
    /// Where the order goes through one of the provider IBALGO's
    /// algorithms, whether its definition takes an amount: `Some(None)` where
    /// the definitions are not held, and `None` for an order through none.
    pub algo: Option<Option<bool>>,
    /// What the logon states about orders for an amount of money.
    pub money: Money<'a>,
    /// The part of a unit the logon has sizes shown to on a contract that
    /// states no least size of its own (tag 8079), empty where it states
    /// none.
    pub size_fraction: &'a str,
}

/// What the logon states that decides whether an order for an amount of money
/// is described by that amount.
#[derive(Clone, Copy, Default)]
pub(super) struct Money<'a> {
    /// The security types the logon takes such orders on (tag 8334).
    pub types: &'a str,
    /// The order types it takes them on (tag 8351).
    pub order_types: &'a str,
    /// Whether the account takes them (tag 8335).
    pub account: bool,
    /// Whether the logon asks for the venue's refusals to be told (tag 6130).
    pub refusals_told: bool,
    /// Whether the account may trade crypto currencies (tag 6652).
    pub crypto: bool,
    /// Whether amounts are written to their currency's precision, which the
    /// logon turns off with NOCASHQTYPRECISION.
    pub precise: bool,
    /// Each product's default size and precision (tag 6052).
    pub product_defaults: &'a str,
}

/// Where a message's references to the venue's answers to frequent questions
/// lead, as the logon names it.
#[derive(Clone, Copy, Default)]
pub(super) struct Faq<'a> {
    /// The address the logon states the answers under.
    pub base: Option<&'a str>,
    /// Whether the login is the venue's own rather than a partner's branded
    /// one.
    pub own_brand: bool,
}

/// What a gateway sends the program for a message the venue states beside an
/// order's status, as the code and the text a program reads, where it sends
/// anything.
///
/// `code` is the message's code (tag 6360) and `text` the message (tag 6361).
/// A price-cap message on an order a program placed is not sent: a gateway
/// shows it in its own window only.
pub(super) fn for_the_venues_message(
    code: &str,
    text: &str,
    order: &Described<'_>,
    faq: Faq<'_>,
) -> Option<(i32, String)> {
    if code == "PRICECAP" {
        return None;
    }
    if closed_fund(code, text) {
        return Some((ORDER_MESSAGE, as_sent(FUND_CLOSED)));
    }
    Some(built(code, text, &describe(order)?, faq))
}

/// What a gateway sends the program for the venue's refusal of an order
/// stated on a status report (tag 58), where the logon asks for such refusals
/// to be told.
pub(super) fn for_the_venues_refusal(
    code: &str,
    text: &str,
    order: &Described<'_>,
    faq: Faq<'_>,
) -> Option<(i32, String)> {
    if closed_fund(code, text) {
        return Some((ORDER_MESSAGE, as_sent(FUND_CLOSED)));
    }
    Some(built(code, text, &describe(order)?, faq))
}

/// The venue's statement that the fund an order is for is closed, which a
/// gateway tells in the venue's words for a closed fund in place of the order
/// message.
fn closed_fund(code: &str, text: &str) -> bool {
    code == "CM" && text == "Fund_Closed"
}

/// A quantity held in the engine's fixed point, as the decimal it is.
pub(super) fn quantity(qty: crate::types::Qty) -> String {
    let scale = crate::types::QTY_SCALE;
    let sign = if qty < 0 { "-" } else { "" };
    let (whole, part) = ((qty / scale).unsigned_abs(), (qty % scale).unsigned_abs());
    if part == 0 {
        return format!("{sign}{whole}");
    }
    let places = scale.ilog10() as usize;
    let fraction = format!("{part:0places$}");
    format!("{sign}{whole}.{}", fraction.trim_end_matches('0'))
}

fn built(code: &str, text: &str, description: &str, faq: Faq<'_>) -> (i32, String) {
    if code == "PRICECAP" {
        let lead = if description.is_empty() {
            "\n\n".to_string()
        } else {
            format!("\n\n{description}\n")
        };
        let question = format!(
            "{}<br>{PRICE_MANAGEMENT_QUESTION}",
            text.replace("<html>", "").replace("</html>", "")
        );
        let shown = linked(&marked_up(&question), faq);
        return (
            PRICE_MANAGEMENT,
            as_sent(&format!("{PRICE_MANAGEMENT_TITLE}{lead}{}", laid_out_marked_up(&shown))),
        );
    }
    let lead = if description.is_empty() { "\n".to_string() } else { format!("\n{description}\n") };
    let shown = linked(text, faq);
    (ORDER_MESSAGE, as_sent(&format!("Order Message:{lead}{}", laid_out(&shown))))
}

/// The venue's words for a closed fund.
const FUND_CLOSED: &str = "We are sorry but the mutual fund specified in your order has been closed and is not \
accepting purchase orders at this time.\n\nPlease note that mutual fund companies may close one or more mutual \
funds to new or existing customers.\nHowever, the terms of the fund closure may allow for additional purchases \
of the fund under certain circumstances.\nCustomers are encouraged to carefully review the fund prospectus for \
further details regarding the fund's closure policies.\nCustomers may also contact the fund company or visit the \
fund website for further information.";

const PRICE_MANAGEMENT_TITLE: &str = "Please review your Order parameters.";
const PRICE_MANAGEMENT_QUESTION: &str = "Use the Price Management Algo?";

/// An error's text as a gateway writes it to a program: each line break a
/// space, and each character beyond seven-bit ASCII as its `\uXXXX` escape.
fn as_sent(text: &str) -> String {
    let mut sent = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => sent.push(' '),
            c if c.is_ascii() => sent.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    sent.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    sent
}

/// Every reference to one of the venue's answers (`FAQ` and eight digits)
/// made a link to the first such answer, where the logon states where the
/// answers are, the whole then marked up; the text as it stands otherwise.
fn linked(text: &str, faq: Faq<'_>) -> String {
    let Some(base) = faq.base else { return text.to_string() };
    if !refers_to_an_answer(text) {
        return text.to_string();
    }
    let text = text.replace("FAQ <br>", "FAQ ");
    let references: Vec<(usize, &str)> = answer_references(&text).collect();
    let Some(&(first, number)) = references.first() else { return marked_up(&text) };
    let address = if faq.own_brand {
        format!("{base}{number}")
    } else {
        let joint = if base.contains('?') { "&" } else { "?" };
        format!("{base}{number}{joint}isEmbedded=1&clientLabel=wb")
    };
    let link = format!("<a href=\"{address}\">{}</a> ", &text[first..first + 12]);
    let mut out = String::with_capacity(text.len() + link.len());
    let mut from = 0;
    for (at, _) in references {
        out.push_str(&text[from..at]);
        out.push_str(&link);
        from = at + 12;
    }
    out.push_str(&text[from..]);
    marked_up(&out)
}

/// Whether the text refers to an answer, the reference possibly broken across
/// a line.
fn refers_to_an_answer(text: &str) -> bool {
    text.match_indices("FAQ ").any(|(at, _)| {
        let rest = &text[at + 4..];
        let rest = rest.strip_prefix("<br>").unwrap_or(rest);
        rest.len() >= 8 && rest.as_bytes()[..8].iter().all(u8::is_ascii_digit)
    })
}

/// Each `FAQ` followed by eight digits, where it starts and the digits.
fn answer_references(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(found) = text[from..].find("FAQ ") {
            let at = from + found;
            from = at + 4;
            if let Some(number) = text.get(at + 4..at + 12)
                && number.bytes().all(|b| b.is_ascii_digit())
            {
                from = at + 12;
                return Some((at, number));
            }
        }
        None
    })
}

/// Text as a gateway marks it up for display, inside `<html>`, its line breaks
/// as `<br>` and its tabs as five spaces; text already marked up as it stands.
fn marked_up(text: &str) -> String {
    if text.starts_with("<html>") || text.starts_with("<HTML>") {
        return text.to_string();
    }
    format!("<html>{}</html>", text.replace('\n', "<br>").replace('\t', "     "))
}

/// Marked-up text laid out as a gateway lays out a marked-up message: its
/// line breaks as `<br>`, and each line broken with a `<br>` ahead of the word
/// that would carry its text, tags not counted, to 110 characters.
fn laid_out_marked_up(text: &str) -> String {
    struct Lines {
        out: String,
        line: String,
        word: String,
        line_tags: usize,
        word_tags: usize,
    }
    impl Lines {
        fn end_word(&mut self, width: usize) {
            let units = |text: &str| text.encode_utf16().count();
            let mut broke = false;
            if units(&self.line) + units(&self.word) >= width + self.line_tags + self.word_tags {
                if !self.line.is_empty() {
                    self.out.push_str(&self.line);
                    if !self.word.is_empty() {
                        self.out.push_str("<br>");
                        broke = true;
                    }
                    self.line.clear();
                    self.line_tags = 0;
                }
                if units(&self.word) >= width + self.word_tags {
                    self.out.push_str(&self.word);
                    if !self.word.is_empty() {
                        self.out.push_str("<br>");
                        broke = true;
                    }
                    self.word.clear();
                    self.word_tags = 0;
                }
            }
            if self.word.ends_with("<br>") {
                if !self.line.is_empty() {
                    self.out.push_str(&self.line);
                    if units(&self.word) > 4 {
                        self.out.push_str("<br>");
                        broke = true;
                    }
                    self.line.clear();
                    self.line_tags = 0;
                }
                if !(broke && units(&self.word) <= 4) {
                    self.out.push_str(&self.word);
                }
                self.word.clear();
                self.word_tags = 0;
            }
            if self.word.is_empty() {
                return;
            }
            if !self.line.is_empty() && units(&self.word) != self.word_tags {
                self.line.push(' ');
            }
            self.line.push_str(self.word.trim());
            self.word.clear();
            self.line_tags += self.word_tags;
            self.word_tags = 0;
        }
    }
    const WIDTH: usize = 110;
    let text = text.replace('\n', "<br>");
    let mut lines = Lines {
        out: String::new(),
        line: String::new(),
        word: String::new(),
        line_tags: 0,
        word_tags: 0,
    };
    let mut in_tag = false;
    for c in text.chars() {
        if in_tag {
            lines.word.push(c);
            lines.word_tags += c.len_utf16();
            if c == '>' {
                in_tag = false;
                if lines.word.ends_with("<br>") {
                    lines.end_word(WIDTH);
                }
            }
            continue;
        }
        match c {
            ' ' | '\r' | '\n' | '\t' => lines.end_word(WIDTH),
            '<' => {
                in_tag = true;
                lines.end_word(WIDTH);
                lines.word.push(c);
                lines.word_tags += 1;
            }
            c => lines.word.push(c),
        }
    }
    lines.end_word(WIDTH);
    let Lines { mut out, line, .. } = lines;
    out.push_str(&line);
    out
}

/// Text laid out as a gateway lays out a message: a text already over several
/// lines is left as it is; otherwise each line, trimmed, is broken ahead of the
/// word or space that would carry it past 110 characters, never ahead of a
/// lone closing bracket or full stop, and after a web address that has more
/// after it.
///
/// The space ahead of a break stays where it was, so a program, reading each
/// break as a space, reads two there.
fn laid_out(text: &str) -> String {
    if text.contains('\n') {
        return text.to_string();
    }
    let tokens: Vec<&str> = split_keeping(text.trim(), ' ').collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut line = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        let length = token.encode_utf16().count();
        let closing = length == 1 && ")].}".contains(*token);
        if line > 0 && line + length > 110 && !closing {
            out.push('\n');
            line = 0;
        }
        out.push_str(token);
        line += length;
        let address = (token.contains("http://") || token.contains("https://"))
            && !(token.starts_with('<') && token.ends_with('>'));
        if address && index + 1 < tokens.len() {
            out.push('\n');
            line = 0;
        }
    }
    out
}

/// The text cut into words and the single spaces between them.
fn split_keeping(text: &str, separator: char) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let cut = if rest.starts_with(separator) {
            separator.len_utf8()
        } else {
            rest.find(separator).unwrap_or(rest.len())
        };
        let (token, after) = rest.split_at(cut);
        rest = after;
        Some(token)
    })
}

/// The order as a gateway's display describes it: side, size and contract;
/// nothing for a kind of contract whose description is not written here.
fn describe(order: &Described<'_>) -> Option<String> {
    if !describable(order.contract) {
        return None;
    }
    let mut line = String::from(side_word(order));
    line.push(' ');
    let size = size_text(order)?;
    if !size.is_empty() {
        line.push_str(&size);
        line.push(' ');
    }
    line.push_str(&contract_text(order));
    Some(line)
}

/// The kinds of contract whose description is written here.
fn describable(def: &ContractDefinition) -> bool {
    matches!(
        def.sec_type,
        SecurityType::Stock
            | SecurityType::Option
            | SecurityType::Future
            | SecurityType::FutureOption
            | SecurityType::Forex
            | SecurityType::Crypto
            | SecurityType::Cfd
            | SecurityType::Commodity
            | SecurityType::Bond
            | SecurityType::Bill
            | SecurityType::FixedIncome
            | SecurityType::Combo
    ) || matches!(&def.sec_type, SecurityType::Other(kind) if kind == "EC")
        || (matches!(def.sec_type, SecurityType::Warrant | SecurityType::IndexOption)
        // A structured product of a kind the venue names is described by
        // details that are not written here.
        && !stated(def, 6832).is_some_and(|kind| matches!(kind, "U" | "K" | "B" | "D" | "F" | "T")))
}

fn side_word(order: &Described<'_>) -> &'static str {
    // An order on a currency pair for an amount of money is the trade of the
    // other currency, and is described from its side.
    let turned = order.contract.sec_type == SecurityType::Forex
        && order.cash.is_some()
        && cash_quantity::takes(order.contract, "CASHQTY");
    match (order.side, turned) {
        (Side::Buy, false) | (Side::Sell | Side::ShortSell, true) => "BUY",
        (Side::Sell, false) | (Side::Buy, true) => "SELL",
        (Side::ShortSell, false) => "SSHORT",
    }
}

fn size_text(order: &Described<'_>) -> Option<String> {
    let def = order.contract;
    if let Some(cash) = order.cash {
        match takes_money(order) {
            // An order carrying an algorithm is described by an amount where
            // the algorithm's definition takes one, which is not held here.
            None => return None,
            Some(true) => return Some(money(cash, order)),
            Some(false) => {}
        }
    }
    // A ladder sized by its components is sized by the most it holds,
    // written as the contract writes any size.
    let most = order.ladder.map(|most| most.to_string());
    let quantity = most.as_deref().unwrap_or(order.quantity);
    if quantity.is_empty() {
        return Some(String::new());
    }
    if def.sec_type.is_fixed_income() {
        return Some(face_value(quantity, def, order.rule));
    }
    if by_the_thousand(def) {
        return Some(thousands(quantity));
    }
    Some(sized(quantity, def, order.rule, order.size_fraction))
}

/// Whether a gateway describes an order for an amount of money by that amount:
/// a crypto currency's always; a currency pair's where the venue takes an
/// amount for it; a share's where the logon takes amounts on shares for the
/// order's type and the share, its rule or the logon allows one. An order
/// through one of the provider IBALGO's algorithms is described by an amount
/// where the algorithm's definition takes one, and on a pair that takes
/// amounts; `None` where the definitions are not held.
fn takes_money(order: &Described<'_>) -> Option<bool> {
    let def = order.contract;
    let m = &order.money;
    let takes = |name: &str| cash_quantity::takes(def, name);
    let pair_by_amount = def.sec_type == SecurityType::Forex && takes("CASHQTY");
    if let Some(defined) = order.algo {
        return defined.map(|by_amount| pair_by_amount || by_amount);
    }
    let crypto = def.sec_type == SecurityType::Crypto;
    if crypto {
        return Some(true);
    }
    let share = def.sec_type == SecurityType::Stock;
    let in_parts = deals_in_fractions(order.rule);
    let plain = |list: &str| cash_quantity::BASIC_TYPES.iter().any(|name| listed(list, name));
    let opened = m.crypto || ((m.account || m.refusals_told) && plain(m.types));
    let by_rule = (opened
        && ((!share && in_parts) || (share && def.min_size_stated && listed(m.types, "CASHQTY"))))
        || (in_parts && takes("CASHQTY"));
    let by_logon = share && plain(m.order_types) && takes("CASHQTY");
    let otherwise = def.sec_type != SecurityType::Combo
        && def.ev_rule.is_empty()
        && def.stock_type != "ETMF"
        && def.exchange != "CFETAS"
        && (by_rule || by_logon);
    let typed = !order.order_type.is_empty()
        && order.order_type != "-1"
        && (pair_by_amount
            || (share && listed(m.order_types, cash_quantity::list_name(ord_type_api_name(order.order_type, "")))));
    Some((pair_by_amount || otherwise) && typed)
}

/// An order for an amount of money as a gateway sizes it: the amount and its
/// currency. A currency pair's to three places; any other's to its currency's
/// precision where the logon states one below a unit for it, and in whole
/// units otherwise.
fn money(cash: f64, order: &Described<'_>) -> String {
    let def = order.contract;
    let amount = if def.sec_type == SecurityType::Forex {
        grouped(&decimal(cash), 3)
    } else if order.money.precise && finer_than_a_unit(order.money.product_defaults, &def.currency)
    {
        grouped(&format!("{cash:.16}"), 16)
    } else {
        grouped(&(cash.trunc() as i64).to_string(), 0)
    };
    format!("{amount} {}", def.currency)
}

/// Whether the logon's product defaults state a currency to a precision finer
/// than a unit: a currency they list for `CASH`, at the precision they state,
/// or, where that is none or nought, the one a gateway holds for it (a
/// hundredth, a yen whole).
fn finer_than_a_unit(product_defaults: &str, currency: &str) -> bool {
    let mut listed = false;
    let mut precision = None;
    for entry in product_defaults.split(';').filter(|entry| !entry.is_empty()) {
        let mut tokens = entry.split(',').filter(|token| !token.is_empty());
        let (Some(kind), Some(product)) = (tokens.next(), tokens.next()) else { continue };
        // The default size and the most, which say nothing here.
        tokens.next();
        tokens.next();
        if kind != "CASH" || product != currency {
            continue;
        }
        if !listed {
            listed = true;
            precision = tokens.next().and_then(|stated| stated.trim().parse::<f64>().ok());
        }
    }
    if !listed {
        return false;
    }
    let precision = match precision {
        Some(stated) if stated != 0.0 => stated,
        _ if currency == "JPY" => 1.0,
        _ => 0.01,
    };
    precision > 0.0 && precision < 1.0
}

/// A size as the contract's own display writes one: whole units grouped by
/// thousands; parts of a unit written out in full where the contract deals in
/// them, and grouped to the places of its least size otherwise.
fn sized(
    quantity: &str,
    def: &ContractDefinition,
    rule: Option<&MarketRule>,
    size_fraction: &str,
) -> String {
    if !quantity.contains('.') {
        return grouped(quantity, 0);
    }
    if deals_in_fractions(rule) {
        return quantity.to_string();
    }
    grouped(quantity, fraction_places(def, size_fraction))
}

/// A bond's size as a gateway writes it: the face value it comes to, in
/// thousands of its currency (`$5K`), where the venue states the face value of
/// one; the number of bonds where it does not. A bond whose rule deals in
/// parts of one is sized by the whole quantity, and one that does not by its
/// whole bonds.
fn face_value(quantity: &str, def: &ContractDefinition, rule: Option<&MarketRule>) -> String {
    let Ok(stated) = quantity.parse::<f64>() else { return String::new() };
    let in_parts = deals_in_fractions(rule);
    let bonds = if in_parts { stated } else { stated.trunc() };
    if bonds == 0.0 {
        return String::new();
    }
    // Stated as a figure the venue can read; nought, or none, is no face
    // value.
    let face = stated_face_value(def);
    let Some(face) = face else {
        return format!("{}", bonds.trunc() as i64);
    };
    format!("{}{}K", currency_sign(&def.currency), grouped(&exactly(bonds * face / 1000.0), 3))
}

/// The face value of one bond, where the venue states one.
fn stated_face_value(def: &ContractDefinition) -> Option<f64> {
    stated(def, 6504)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|face| *face != f64::MAX && *face != 0.0)
}

/// A number's exact decimal expansion, as a gateway's number formats round
/// it.
fn exactly(value: f64) -> String {
    format!("{value:.60}")
}

/// How a gateway's display marks a currency ahead of an amount: its sign, a
/// space after one longer than a character.
fn currency_sign(currency: &str) -> String {
    let sign = match currency.trim() {
        "" => "",
        "EUR" => "\u{20ac}",
        "GBP" => "\u{a3}",
        "USD" => "$",
        _ => currency,
    };
    if sign.chars().count() > 1 { format!("{sign} ") } else { sign.to_string() }
}

/// A currency pair's sizes, and a contract for difference on one, are written
/// in thousands and millions.
fn by_the_thousand(def: &ContractDefinition) -> bool {
    def.sec_type == SecurityType::Forex
        || (def.sec_type == SecurityType::Cfd && def.under_sec_type == "CASH")
}

/// Whether the contract's rule deals in parts of a unit.
fn deals_in_fractions(rule: Option<&MarketRule>) -> bool {
    rule.and_then(|r| {
        r.size_increments.iter().map(|b| b.increment).filter(|v| *v > 0.0).min_by(f64::total_cmp)
    })
    .is_some_and(|finest| finest < 1.0)
}

/// How many places a size is written to for a contract dealt in whole units:
/// those of the least size the venue states behind its flag, or of a
/// ten-thousandth where it states the flag alone; on a contract without the
/// flag, those of the part of a unit the logon states where that is finer
/// than a ten-thousandth, and four otherwise.
fn fraction_places(def: &ContractDefinition, size_fraction: &str) -> usize {
    let places = |least: &str| {
        least.split_once('.').map_or(0, |(_, places)| places.trim_end_matches('0').len())
    };
    if def.min_size_stated {
        return places(if def.min_size_text.is_empty() { "0.0001" } else { &def.min_size_text });
    }
    if size_fraction.trim().is_empty() { 4 } else { places(size_fraction).max(4) }
}

/// A number as a decimal written out in full, without an exponent.
fn decimal(value: f64) -> String {
    let text = format!("{value}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// A decimal grouped by thousands, rounded half to even to at most `places`.
fn grouped(value: &str, places: usize) -> String {
    let (negative, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let (whole, fraction) = rounded_half_even(whole, fraction, places);
    let mut out = String::new();
    if negative && (!whole.trim_start_matches('0').is_empty() || !fraction.is_empty()) {
        out.push('-');
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(&fraction);
    }
    out
}


/// A currency pair's size as a gateway writes it: thousands as `K` and
/// millions as `M` where the rest divides out, a tenth of either as `.1`, and
/// the groups written out otherwise.
fn thousands(quantity: &str) -> String {
    let (negative, digits) = match quantity.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, quantity),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let Ok(n) = whole.parse::<u128>() else { return quantity.to_string() };
    let fraction = fraction.trim_end_matches('0');
    // The units' group carries any fraction.
    let units = n % 1000;
    let units_nought = units == 0 && fraction.is_empty();
    let units_text =
        if fraction.is_empty() { units.to_string() } else { format!("{units}.{fraction}") };
    let hundreds_nought = units % 100 == 0 && fraction.is_empty();
    // A group after the first, written to three digits.
    let group = |value: u128, text: String| match value {
        0 if text == "0" => "000".to_string(),
        v if v < 10 => format!("00{text}"),
        v if v < 100 => format!("0{text}"),
        _ => text,
    };
    let mut out = String::from(if negative { "-" } else { "" });
    if n >= 1_000_000_000 {
        let (billions, millions, thousands) =
            (n / 1_000_000_000, n / 1_000_000 % 1000, n / 1000 % 1000);
        out.push_str(&billions.to_string());
        if millions != 0 {
            if millions % 100 == 0 && units_nought {
                out.push_str(&format!(".{}", millions / 100));
            } else {
                out.push_str(&format!(",{}", group(millions, millions.to_string())));
            }
        } else if thousands != 0 || !units_nought {
            out.push_str(",000");
        }
        if thousands != 0 || !units_nought {
            out.push_str(&format!(
                ",{},{}",
                group(thousands, thousands.to_string()),
                group(units, units_text)
            ));
        } else {
            out.push_str(",000,000");
        }
    } else if n >= 1_000_000 {
        let (millions, thousands) = (n / 1_000_000, n / 1000 % 1000);
        out.push_str(&millions.to_string());
        let mut whole_millions = true;
        if thousands != 0 {
            if thousands % 100 == 0 && units_nought {
                out.push_str(&format!(".{}", thousands / 100));
            } else {
                out.push_str(&format!(",{}", group(thousands, thousands.to_string())));
                whole_millions = false;
            }
        } else if !units_nought {
            out.push_str(",000");
            whole_millions = false;
        }
        if !units_nought {
            out.push_str(&format!(",{}", group(units, units_text)));
        } else if thousands % 100 != 0 {
            out.push('K');
        } else if whole_millions {
            out.push('M');
        }
    } else if n >= 1000 {
        out.push_str(&(n / 1000).to_string());
        if units_nought {
            out.push('K');
        } else if hundreds_nought {
            out.push_str(&format!(".{}K", units / 100));
        } else {
            out.push_str(&format!(",{}", group(units, units_text)));
        }
    } else {
        out.push_str(&units_text);
    }
    out
}

/// The contract as a gateway's display names it: its symbol, what kind of
/// contract it is, and the venue's own name for it in brackets where that is
/// not the symbol.
fn contract_text(order: &Described<'_>) -> String {
    let def = order.contract;
    let symbol = def.symbol.as_str();
    // A combination whose legs are known is named by its symbol, which a
    // gateway holds as its own name for it too, and the word for one.
    if def.sec_type == SecurityType::Combo {
        return format!("{symbol} Combo");
    }
    let local = if def.local_symbol.is_empty() { symbol } else { def.local_symbol.as_str() };
    let kind = kind_text(order);
    let kind = kind.trim();
    let mut out = String::new();
    if !kind.starts_with(symbol) {
        out.push_str(symbol);
        out.push(' ');
    }
    out.push_str(kind);
    let bond =
        matches!(def.sec_type, SecurityType::Bond | SecurityType::Bill | SecurityType::FixedIncome);
    if symbol != local && !bond && def.sec_type != SecurityType::Forex {
        out.push_str(" (");
        out.push_str(local);
        out.push_str(") ");
    }
    out
}

/// The contract's own part of its name, by the kind of contract it is.
fn kind_text(order: &Described<'_>) -> String {
    let (def, rule) = (order.contract, order.rule);
    let symbol = def.symbol.as_str();
    let class = def.trading_class.as_str();
    let after_symbol =
        |rest: &str| if symbol.is_empty() { rest.to_string() } else { format!("{symbol} {rest}") };
    let bracketed = |text: &str| if text.is_empty() { String::new() } else { format!(" ({text})") };
    match &def.sec_type {
        SecurityType::Stock => after_symbol(&listing(def)),
        SecurityType::Option => {
            let chain = chain_name(def).map(|name| format!("{name} ")).unwrap_or_default();
            after_symbol(&format!(
                "{chain}{} {} {}",
                option_expiry(def),
                strike(def, rule),
                right(def)
            ))
        }
        SecurityType::Future => {
            let protected = if stated(def, 6808).is_some_and(is_true) { " (DP)" } else { "" };
            after_symbol(&format!("{}{protected}", month_year(contract_month(def), true)))
        }
        SecurityType::FutureOption => {
            let rest = format!(
                "{} {} {} Fut. Option",
                month_year(contract_month(def), true),
                strike(def, rule),
                right(def)
            );
            let classed = if !class.is_empty() && class != symbol {
                format!(" ({class})")
            } else {
                String::new()
            };
            format!("{}{classed}", after_symbol(&rest))
        }
        // An event contract reads as a future's option does, its right as a
        // yes or a no.
        SecurityType::Other(kind) if kind == "EC" => {
            let answer = match def.right {
                Some(OptionRight::Call) => "YES",
                Some(OptionRight::Put) => "NO",
                None => "???",
            };
            let rest = format!(
                "{} {} {answer} Event",
                month_year(contract_month(def), true),
                strike(def, rule)
            );
            let classed = if !class.is_empty() && class != symbol {
                format!(" ({class})")
            } else {
                String::new()
            };
            format!("{}{classed}", after_symbol(&rest))
        }
        SecurityType::Warrant | SecurityType::IndexOption => {
            let expiry = match month_year(contract_month(def), true) {
                month if month == "NOEXP" => "Perpetual".to_string(),
                month => month,
            };
            format!(
                "{} {} {}{}",
                after_symbol(&expiry),
                strike(def, rule),
                right(def),
                issued_by(def, order.whole_listing)
            )
        }
        SecurityType::Forex => format!("{symbol}.{} Forex", def.currency),
        SecurityType::Crypto => format!("Crypto{}", bracketed(class)),
        SecurityType::Commodity => format!("Commodity{}", bracketed(class)),
        SecurityType::Cfd if def.under_sec_type == "CASH" => {
            format!("{symbol}.{} CFD", def.currency)
        }
        SecurityType::Cfd => format!("{}{}", after_symbol("CFD"), bracketed(class)),
        SecurityType::Bond | SecurityType::Bill | SecurityType::FixedIncome => {
            bond_text(def, order.isin_with_cusip)
        }
        _ => String::new(),
    }
}

/// A bond as a gateway's display names it: its class, its kind, its coupon,
/// its maturity or that it has none, its CUSIP or else its ISIN, and its
/// ratings, each where stated.
fn bond_text(def: &ContractDefinition, isin_with_cusip: bool) -> String {
    let mut out = String::new();
    let class = stated(def, 6503).unwrap_or("");
    for part in [class, &bond_kind(def, class), &def.coupon_text] {
        if !part.trim().is_empty() {
            out.push_str(part);
            out.push(' ');
        }
    }
    if stated(def, 6757).is_some_and(is_true) {
        out.push_str("Perpetual");
    } else {
        let maturity =
            if def.last_trade_date == "00000000" { "NOEXP" } else { &def.last_trade_date };
        out.push_str(&short_date(maturity));
    }
    let cusip = def.cusip.as_str();
    let a_cusip = !cusip.is_empty()
        && (cusip.encode_utf16().count() == 9
            || cusip
                .strip_prefix("IBCID")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())));
    if a_cusip {
        out.push(' ');
        out.push_str(cusip);
    } else if !def.isin.is_empty()
        && (cusip.is_empty() || !def.isin.contains(cusip) || isin_with_cusip)
    {
        out.push(' ');
        out.push_str(&def.isin);
    }
    if !def.ratings.trim().is_empty() {
        out.push(' ');
        out.push_str(&def.ratings);
    }
    out
}

/// What kind of bond it is, as the display shows it: the venue's word for it,
/// less the kind of contract it opens with, where what is left says more than
/// three letters; a municipal bond's as the venue states it.
fn bond_kind(def: &ContractDefinition, class: &str) -> String {
    let stated = def.bond_type.as_str();
    let name = def.sec_type.to_api_str();
    if class == "MUNI" || !stated.to_lowercase().starts_with(&name.to_lowercase()) {
        return stated.to_string();
    }
    let rest = stated.get(name.len()..).unwrap_or("").trim();
    match rest {
        "TIPSs" => "TIPS".to_string(),
        rest if rest.encode_utf16().count() > 3 => rest.to_string(),
        _ => String::new(),
    }
}

/// A date as a bond's maturity is shown: `May05'30`, a month without its day
/// as `May'30`; any other text as it stands.
fn short_date(date: &str) -> String {
    let Some((year, month)) = year_and_month(date) else { return date.to_string() };
    let month = &MONTHS[MONTHS.iter().position(|m| *m == month).unwrap_or(0)];
    let camel = format!("{}{}", &month[..1], month[1..].to_lowercase());
    format!("{camel}{}'{year}", date.get(6..8).unwrap_or(""))
}

/// A flag the venue states as a number: set where the number is not nought.
fn is_true(value: &str) -> bool {
    value.trim().parse::<i64>().is_ok_and(|number| number != 0)
}

/// Who issued a warrant or a structured product and on what terms, in
/// brackets: where it is listed, its issuer, its multiplier and the venue's
/// further note, each where stated.
fn issued_by(def: &ContractDefinition, whole_listing: bool) -> String {
    // The market read whole where the logon has it so; otherwise with its
    // segment where the contract is routed to it by that name, and without
    // what follows a point in its name if not.
    let listed = match stated(def, 8224).filter(|segment| !segment.is_empty()) {
        Some(segment) => format!("{}.{segment}", def.primary_exchange),
        None => def.primary_exchange.clone(),
    };
    let market = if whole_listing {
        def.primary_exchange.clone()
    } else if def.valid_exchanges.contains(&listed) {
        listed
    } else {
        def.primary_exchange.split('.').next().unwrap_or_default().to_string()
    };
    let parts: Vec<&str> = [
        market.as_str(),
        stated(def, 106).unwrap_or(""),
        def.multiplier_text.as_str(),
        stated(def, 6648).unwrap_or(""),
    ]
    .into_iter()
    .filter(|part| !part.trim().is_empty())
    .collect();
    if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(",")) }
}

/// A field of the definition kept under its number.
fn stated(def: &ContractDefinition, tag: u32) -> Option<&str> {
    def.unnamed_fields.iter().find(|(t, _)| *t == tag).map(|(_, v)| v.as_str())
}

/// Where a share is listed and the segment of that market, as `NASDAQ.NMS`:
/// the market as the venue names it, not the older spelling a program may be
/// handed.
fn listing(def: &ContractDefinition) -> String {
    let market = crate::control::contracts::exchange_to_fix(&def.primary_exchange);
    match stated(def, 8224).filter(|segment| !segment.is_empty()) {
        Some(segment) => format!("{market}.{segment}"),
        None => market.to_string(),
    }
}

/// The name an option's chain goes by where it is not the underlying's: the
/// classifier where the venue states one, else the trading class in brackets.
fn chain_name(def: &ContractDefinition) -> Option<String> {
    if let Some(classifier) = stated(def, 6957) {
        return (!classifier.trim().is_empty()).then(|| classifier.to_string());
    }
    let class = def.trading_class.as_str();
    (!class.trim().is_empty() && class != def.under_symbol).then(|| format!("({class})"))
}

/// The month a contract is for, or the month of its last day where the venue
/// states none.
fn contract_month(def: &ContractDefinition) -> &str {
    if !def.contract_month.is_empty() {
        return &def.contract_month;
    }
    def.last_trade_date.get(..6).unwrap_or(&def.last_trade_date)
}

/// An option's expiry: its last day as `OCT 16 '26`, or, where the month it
/// is for is not that day's month, the month as `DEC26`.
fn option_expiry(def: &ContractDefinition) -> String {
    let month = contract_month(def);
    let day = def.last_trade_date.as_str();
    if !month.is_empty() && !day.is_empty() && !day.starts_with(month) {
        month_year(month, false)
    } else {
        day_month_year(day)
    }
}

const MONTHS: [&str; 12] =
    ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

/// The month and year of a `yyyymm[dd]` date, as `DEC'26` or `DEC26`; any
/// other text as it stands.
fn month_year(date: &str, apostrophe: bool) -> String {
    let Some((year, month)) = year_and_month(date) else { return date.to_string() };
    format!("{month}{}{year}", if apostrophe { "'" } else { "" })
}

/// A `yyyymmdd` date as `OCT 16 '26`, a month without its day as `OCT  '26`;
/// any other text as it stands.
fn day_month_year(date: &str) -> String {
    let Some((year, month)) = year_and_month(date) else { return date.to_string() };
    format!("{month} {} '{year}", date.get(6..8).unwrap_or(""))
}

fn year_and_month(date: &str) -> Option<(&str, &str)> {
    if date == "NOEXP" || !date.get(..4)?.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let month: usize = date.get(4..6)?.parse().ok()?;
    Some((date.get(2..4)?, MONTHS.get(month.checked_sub(1)?)?))
}

/// An option's strike as a gateway writes it: raised by the rule's
/// magnifier, rounded to the places the rule displays prices to (two where it
/// states none), and written to at most eight places. A contract the venue
/// files as an event states its strike as a text where it has one, and a
/// strike of nought as nothing.
fn strike(def: &ContractDefinition, rule: Option<&MarketRule>) -> String {
    let event = stated(def, 6688).is_some_and(|category| category.eq_ignore_ascii_case("Event"));
    if event && let Some(text) = stated(def, 8568).filter(|text| !text.is_empty()) {
        return text.to_string();
    }
    let strike = match rule {
        None => def.strike,
        Some(rule) => {
            let raised = def.strike * 10f64.powi(rule.price_magnifier);
            match rule.price_places {
                Some(-3) => raised,
                Some(places) if places > 0 => rounded(raised, places),
                _ => rounded(raised, 2),
            }
        }
    };
    if event && strike == 0.0 {
        return String::new();
    }
    let text = format!("{strike:.8}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".to_string() } else { text.to_string() }
}

/// A price rounded half up to `places`, as a gateway rounds a strike.
fn rounded(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    let scaled = value * scale;
    let floor = scaled.floor();
    (if scaled - floor >= 0.5 { floor + 1.0 } else { floor }) / scale
}

fn right(def: &ContractDefinition) -> &'static str {
    match def.right {
        Some(OptionRight::Call) => "Call",
        Some(OptionRight::Put) => "Put",
        None => "???",
    }
}
