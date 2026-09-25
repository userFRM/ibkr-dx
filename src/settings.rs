//! The settings a gateway keeps in its own configuration file.
//!
//! A gateway is a process, and a process is configured by a file beside it and
//! a window in front of it. This client is a library and has neither, so the
//! same settings belong on the client, where a caller sets them in code and
//! reads them back.
//!
//! They are stated on [`EClientConfig`](crate::api::client::EClientConfig)
//! alongside the login, because a caller has one session and should not have
//! to configure it in two places.
//!
//! **Each session runs under its own.** They are settled once, as the session
//! opens, into a [`SessionSettings`] the session carries: two sessions in one
//! process have their own, and neither can change the other's. What a caller
//! states wins over the environment, which wins over the default, so a program
//! configured the old way keeps working.
//!
//! Logging is the exception, and is named as one: a process has one logger, and
//! whoever installs it holds what flushes it, so `log_level`, `log_dir` and
//! `log_queue` belong to the process rather than to a session. The first
//! session to open installs the logger from them, the same way a gateway reads
//! its logging configuration as it starts; a session that opens after that
//! cannot move a logger that is already running, and one that states them then
//! is told so rather than left believing it was heard. See
//! [`logging::apply`](crate::logging::apply).

/// A setting stated on the client, or left to the environment.
///
/// Every one of these stands in for something the gateway held. Where a caller
/// states nothing, what was already in the environment stands — so a program
/// configured the old way keeps working, and one configured in code does not
/// have to know the environment exists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GatewaySettings {
    /// The time zone the gateway ran in, which it announced at logon. This
    /// announces the same one and no more: a time handed to this client
    /// without a zone of its own is read as UTC whatever this says. Defaults
    /// to UTC.
    pub timezone: Option<String>,
    /// The locale it announced itself with. Reaches the wire through
    /// [`encoded`](Self::encoded), whose locale segment it replaces, so
    /// stating both leaves this one with nothing to do.
    pub locale: Option<String>,
    /// The build it announced itself as, which the venue keeps a list of and
    /// stops accepting when it is old enough.
    pub build: Option<String>,
    /// The version beside that build.
    pub version: Option<String>,
    /// The longer string it announced with them. A session resumed from an
    /// earlier one announces that one's instead, because it is the identity
    /// the server holds the session under.
    pub encoded: Option<String>,
    /// The machine identity it presented. Resumed the same way
    /// [`encoded`](Self::encoded) is.
    pub hardware_id: Option<String>,
    /// The network card it presented, where the machine's own is not the one
    /// to present.
    pub mac_address: Option<String>,
    /// The address on the local network it presented, for the same reason.
    pub lan_ip: Option<String>,
    /// The host every farm connection is opened on, where it is not the one
    /// the venue names.
    pub market_data_host: Option<String>,
    /// The port a farm connection opens on, where the venue's routing names
    /// none. Logging in is always on the port the protocol fixes for it.
    pub port: Option<u16>,
    /// How much it wrote down. Logging reads this from `IBKR_DX_LOG_LEVEL`.
    pub log_level: Option<String>,
    /// Where it wrote it. Logging reads this from `IBKR_DX_LOG_DIR`.
    pub log_dir: Option<String>,
    /// How many records it buffered before dropping them. Logging reads this
    /// from `IBKR_DX_LOG_QUEUE`, and reads it as a count: a boolean here could
    /// state nothing the reader understood, so every value fell to the
    /// default.
    pub log_queue: Option<usize>,
    /// File retaining the next order id per account and API client. Unset uses
    /// `ibkr-dx/order-ids.json` in the user's data directory. An empty path
    /// disables persistence. Also read from `IBKR_DX_ORDER_ID_FILE`.
    pub order_id_file: Option<String>,

    // ── What the gateway did with what it received ──
    /// Which executions arrive when a session opens: today's, or every one
    /// the venue still holds. The gateway asks for every one.
    pub execution_reports: Option<ExecutionReportScope>,
    /// Whether a US stock trading on Nasdaq is handed back under the older
    /// spelling. The gateway does, so a program written against it compares
    /// against that spelling.
    pub island_for_nasdaq: Option<bool>,
    /// Whether a session recovers on its own when a connection goes away.
    ///
    /// On by default, which is what a process that must keep running wants.
    /// Turned off, the loss is reported and nothing is done about it, and the
    /// caller decides whether and when to open a session again. A Rust caller
    /// states the rest of recovery — how many attempts, for how long — on
    /// [`ReconnectConfig`](crate::reliability::ReconnectConfig); this is the
    /// switch, and it is the one both surfaces have.
    pub reconnect_on_socket_err: Option<bool>,
}

/// Which executions a session asks for when it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionReportScope {
    /// Today's only.
    Today,
    /// Every one the venue still holds, which is what the gateway asks for.
    All,
}

/// Gateway settings that are not settings here, and what to do instead.
///
/// Named rather than dropped: someone moving off a gateway will look for them,
/// and "there is no such thing here" is an answer where silence is not. Some
/// of these do have a stand-in — it is simply not one of the settings above —
/// and the reason names it.
pub const UNAVAILABLE: &[(&str, &str)] = &[
    ("rejectMessagesAboveMaxRate", "nothing paces what a caller sends: a gateway paces requests at the rate its logon states (fifty a second where it states none) unless this is set; set, a request above that rate is answered with error 100 and still carried out, and the third ends the connection, unless the venue or the client asks for pacing. This client does neither; the pacing here is the subscription burst a reconnect replays, stated on ReconnectConfig"),
    ("sendInstrumentTimezone", "no setting chooses the zone a timestamp is stated in: a bar is stated on the zone the venue names beside it, or as seconds since the epoch where the request asked for that; an execution is stamped as the venue stamps it"),
    ("LocalServerPort", "no local socket to listen on; this client is the client"),
    ("LocalApiPort", "no local socket to listen on; this client is the client"),
    ("TrustedIPs", "nothing connects to this client, so nothing needs trusting"),
    ("Local_FIX_Server_Settings", "no local socket to listen on; this client is the client, and the only sockets it holds are the ones it opened to the venue"),
    ("ApiOnly", "`readonly` on the client config, or connect(readonly=True): stated per session rather than once for a process"),
    ("RemoteHostOrderRouting", "`host` on the client config, or connect(host=...): orders are routed on the connection the login opened, and left unset the venue names the server this account is on"),
    ("RemotePortOrderRouting", "one port, fixed by the protocol: a redirect naming another is accepted at the socket and then reset, and the session only completes on the fixed one"),
    ("useSsl", "no switch: the login and the order connection are TLS with no plaintext path, and a farm connection is opened the one way the venue answers — a key exchange, an enciphered logon, then messages signed rather than enciphered"),
    ("UseSSL", "no switch: the login and the order connection are TLS with no plaintext path, and a farm connection is opened the one way the venue answers — a key exchange, an enciphered logon, then messages signed rather than enciphered"),
    ("Select_account_type", "`paper` on the client config, or connect(paper=True): nothing here selects an account type, the login decides it and the venue names the accounts it holds at logon"),
    ("MainWindow.Width", "no window"),
    ("MainWindow.Height", "no window"),
    ("vmoptions", "no runtime to size"),
];

/// Every setting a session runs under, resolved to a value.
///
/// Settled once, as the session opens, and immutable afterwards. Two sessions
/// in one process each hold their own, so one session's settings cannot reach
/// another session's reconnects through the process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSettings {
    /// The durable order-id file, or `None` when persistence is disabled.
    pub order_id_file: Option<std::path::PathBuf>,
    /// The zone the session announces at logon.
    pub timezone: String,
    /// The locale it announces itself for.
    pub locale: String,
    /// The build this session announces itself as.
    pub build: String,
    /// The version it announces.
    pub version: String,
    /// What the session encodes its client string as.
    pub encoded: String,
    /// What it identifies this machine as. Derived when unset.
    pub hardware_id: Option<String>,
    /// Which network card it names as this machine's. Probed when unset.
    ///
    /// The machine identity the venue holds a session under is built from
    /// three things, and only one of them was the caller's to state. The other
    /// two are read off whatever the operating system answers first, which on
    /// some of them is a virtual card, and inside a container is the
    /// container's rather than the machine's.
    pub mac_address: Option<String>,
    /// Which address on the local network it names as this machine's. Probed
    /// when unset, the same way and for the same reason.
    pub lan_ip: Option<String>,
    /// Which host every farm connection opens on, where the caller names one.
    pub market_data_host: Option<String>,
    /// Which port a farm connection opens on, where the venue's routing names
    /// none.
    pub port: u16,
    /// Which executions a session asks for when it opens.
    pub execution_reports: ExecutionReportScope,
    /// Whether a US stock on Nasdaq is named by the older spelling. Takes the
    /// venue's grant as well as this setting.
    pub island_for_nasdaq: bool,
    /// Whether this session recovers on its own when a connection goes away.
    pub reconnect_on_socket_err: bool,
}

fn order_id_file() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    #[cfg(target_os = "windows")]
    let data = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let data = std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    // A relative directory would be a different one in each process.
    let data = data.filter(|path| path.is_absolute());
    if data.is_none() {
        log::warn!("cannot locate the user data directory; order ids will use session memory and venue replay only");
    }
    data.map(|path| path.join("ibkr-dx/order-ids.json"))
}

impl Default for SessionSettings {
    fn default() -> Self {
        GatewaySettings::default().resolve()
    }
}

impl GatewaySettings {
    /// Settle every setting: what the caller stated, else what the environment
    /// holds, else the default.
    ///
    /// The one place a session's settings read the environment. Called on the
    /// caller's thread as the session opens, before any thread of the engine's
    /// exists, so nothing downstream reads a setting that can still change.
    pub fn resolve(&self) -> SessionSettings {
        fn stated(caller: Option<&String>, variable: &str) -> Option<String> {
            caller
                .filter(|v| !v.is_empty())
                .cloned()
                .or_else(|| std::env::var(variable).ok().filter(|v| !v.is_empty()))
                .and_then(|value| one_field(variable, value))
        }
        /// A setting is one field, and has to stay one.
        ///
        /// Two characters end a field on the wires this client writes: the
        /// separator a FIX message is written with, and the semicolon the
        /// connection request is written with. A setting carrying either
        /// reaches the venue as the fields it was cut into — a zone of
        /// `UTC<SOH>6900=0` states a zone and then a tag nobody set, and a
        /// client string with a semicolon in it adds a whole position to the
        /// request. Refused here rather than at the socket: this is the one
        /// place every setting passes through, and what the venue is told has
        /// to be what the caller stated.
        fn one_field(variable: &str, value: String) -> Option<String> {
            if value.contains('\u{1}') || value.contains(';') {
                log::warn!(
                    "{variable} carries a character that ends a field on the wire, so it \
                     states more than the one thing it was given as; the default stands \
                     instead",
                );
                return None;
            }
            Some(value)
        }
        SessionSettings {
            order_id_file: match self.order_id_file.clone()
                .or_else(|| std::env::var("IBKR_DX_ORDER_ID_FILE").ok()) {
                Some(path) => (!path.is_empty()).then(|| path.into()),
                None => order_id_file(),
            },
            timezone: stated(self.timezone.as_ref(), "IBKR_DX_TZ")
                .unwrap_or_else(|| "UTC".to_string()),
            locale: stated(self.locale.as_ref(), "IBKR_DX_LOCALE")
                .unwrap_or_else(|| crate::config::IB_LOCALE.to_string()),
            build: stated(self.build.as_ref(), "IBKR_DX_BUILD")
                .unwrap_or_else(|| crate::config::IB_BUILD.to_string()),
            version: stated(self.version.as_ref(), "IBKR_DX_VERSION")
                .unwrap_or_else(|| crate::config::IB_VERSION.to_string()),
            // The whole string, or the locale set into it, or neither. Tag
            // 6266 carries `{jdkVer}/{platform}/{locale}/{dist}` and the venue
            // refuses a locale that is not a canonical one.
            encoded: stated(self.encoded.as_ref(), "IBKR_DX_ENCODED").unwrap_or_else(|| {
                match stated(self.locale.as_ref(), "IBKR_DX_LOCALE") {
                    // The identity this client announces, with the locale
                    // segment replaced. Composing it a second time here makes a
                    // session that states a locale announce a stale runtime and
                    // platform while one that states none announces the current
                    // pair.
                    Some(locale) => match crate::config::IB_ENCODED.split('/')
                        .collect::<Vec<_>>()
                        .as_slice()
                    {
                        [runtime, platform, _, distribution] => {
                            format!("{runtime}/{platform}/{locale}/{distribution}")
                        }
                        _ => crate::config::IB_ENCODED.to_string(),
                    },
                    None => crate::config::IB_ENCODED.to_string(),
                }
            }),
            hardware_id: stated(self.hardware_id.as_ref(), "IBKR_DX_HWID"),
            mac_address: stated(self.mac_address.as_ref(), "IBKR_DX_MAC"),
            lan_ip: stated(self.lan_ip.as_ref(), "IBKR_DX_IP"),
            market_data_host: stated(self.market_data_host.as_ref(), "IBKR_DX_FARM_HOST"),
            port: self
                .port
                .or_else(|| std::env::var("IBKR_DX_MISC_PORT").ok().and_then(|v| v.parse().ok()))
                .unwrap_or(crate::config::MISC_PORT),
            execution_reports: self.execution_reports.unwrap_or_else(|| {
                // However it is spelled, and said out loud when it is spelled
                // as neither. Matched against lowercase alone, `Today` fell to
                // the default and the session asked the venue for every
                // execution it still holds, which is the opposite of what was
                // stated and a heavier request on every session that opens.
                match std::env::var("IBKR_DX_EXECUTION_REPORTS") {
                    Ok(stated) if stated.eq_ignore_ascii_case("today") => {
                        ExecutionReportScope::Today
                    }
                    Ok(stated)
                        if !stated.is_empty() && !stated.eq_ignore_ascii_case("all") =>
                    {
                        log::warn!(
                            "IBKR_DX_EXECUTION_REPORTS names neither today nor all: {stated}. \
                             This session asks for every execution the venue holds",
                        );
                        ExecutionReportScope::All
                    }
                    _ => ExecutionReportScope::All,
                }
            }),
            island_for_nasdaq: self.island_for_nasdaq.unwrap_or_else(|| {
                // As above: `False` turned the setting on, because only the
                // lowercase spelling counted as off.
                !std::env::var("IBKR_DX_ISLAND_FOR_NASDAQ").is_ok_and(|stated| {
                    ["0", "false", "no"].iter().any(|off| stated.eq_ignore_ascii_case(off))
                })
            }),
            reconnect_on_socket_err: self.reconnect_on_socket_err.unwrap_or_else(|| {
                !std::env::var("IBKR_DX_RECONNECT_ON_SOCKET_ERR").is_ok_and(|stated| {
                    ["0", "false", "no"].iter().any(|off| stated.eq_ignore_ascii_case(off))
                })
            }),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recovery is on unless the switch a gateway carries says otherwise.
    ///
    /// The switch is what a program migrating from a gateway looks for, and
    /// until it was a setting here the Python surface had no way to say it at
    /// all: a session recovered on its own whatever the caller wanted.
    #[test]
    fn the_recovery_switch_is_on_unless_it_is_turned_off() {
        assert!(
            GatewaySettings::default().resolve().reconnect_on_socket_err,
            "the documented default",
        );
        assert!(
            !GatewaySettings { reconnect_on_socket_err: Some(false), ..Default::default() }
                .resolve()
                .reconnect_on_socket_err,
            "a session that states it off is off",
        );
        assert!(
            GatewaySettings { reconnect_on_socket_err: Some(true), ..Default::default() }
                .resolve()
                .reconnect_on_socket_err,
            "and a session that states it on is on",
        );
    }

    /// A setting stated on the client is what the session runs under.
    #[test]
    fn a_stated_setting_is_what_the_session_runs_under() {
        let resolved = GatewaySettings {
            timezone: Some("America/New_York".to_string()),
            port: Some(4002),
            ..Default::default()
        }
        .resolve();
        assert_eq!(resolved.timezone, "America/New_York");
        assert_eq!(resolved.port, 4002);
    }

    /// Two sessions in one process each run under their own. Stating one used
    /// to write it into the process, where the other session's reconnects
    /// found it.
    #[test]
    fn one_session_does_not_state_anothers() {
        let first = GatewaySettings {
            timezone: Some("America/New_York".to_string()),
            ..Default::default()
        }
        .resolve();
        let second = GatewaySettings {
            timezone: Some("Europe/Zurich".to_string()),
            ..Default::default()
        }
        .resolve();
        assert_eq!(first.timezone, "America/New_York");
        assert_eq!(second.timezone, "Europe/Zurich");
    }

    /// What the caller stated, else what the environment holds, else the
    /// default. A program configured the old way keeps working.
    #[test]
    fn the_environment_is_what_a_caller_states_nothing_over() {
        // Environment changes belong to this test's process alone.
        if std::env::var("RUST_TEST_THREADS").as_deref() != Ok("1") {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "settings::tests::the_environment_is_what_a_caller_states_nothing_over"])
                .env("RUST_TEST_THREADS", "1")
                .output().unwrap();
            assert!(output.status.success(), "{}{}",
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        unsafe { std::env::set_var("IBKR_DX_LOCALE", "fr_FR") };
        let from_environment = GatewaySettings::default().resolve();
        assert_eq!(from_environment.locale, "fr_FR");
        // The identity this client announces with its locale set into it, read
        // from that identity rather than written out again: the two spellings
        // drifted apart the moment either changed.
        assert_eq!(
            from_environment.encoded,
            crate::config::IB_ENCODED.replace(crate::config::IB_LOCALE, "fr_FR"),
        );
        assert_ne!(from_environment.encoded, crate::config::IB_ENCODED);

        let stated = GatewaySettings {
            locale: Some("ja_JP".to_string()),
            ..Default::default()
        }
        .resolve();
        assert_eq!(stated.locale, "ja_JP", "the caller's own wins");
        unsafe { std::env::remove_var("IBKR_DX_LOCALE") };

        let neither = GatewaySettings::default().resolve();
        assert_eq!(neither.timezone, "UTC");
        assert_eq!(neither.build, crate::config::IB_BUILD);
        assert_eq!(neither.port, crate::config::MISC_PORT);
        assert!(neither.island_for_nasdaq, "the documented default");

        // However it is spelled. Compared against the lowercase spelling
        // alone, `Today` resolves to every execution the venue holds and
        // `False` leaves the older exchange spelling on, each the opposite of
        // what is written. Checked here rather than in a test of its own,
        // because these are the process's own variables and a second test
        // setting them races this one reading them.
        for spelling in ["today", "Today", "TODAY"] {
            unsafe { std::env::set_var("IBKR_DX_EXECUTION_REPORTS", spelling) };
            assert_eq!(
                GatewaySettings::default().resolve().execution_reports,
                ExecutionReportScope::Today,
                "{spelling} asked for every execution the venue holds",
            );
        }
        unsafe { std::env::set_var("IBKR_DX_EXECUTION_REPORTS", "yesterday") };
        assert_eq!(
            GatewaySettings::default().resolve().execution_reports,
            ExecutionReportScope::All,
            "a value naming neither keeps the default",
        );
        unsafe { std::env::remove_var("IBKR_DX_EXECUTION_REPORTS") };

        for spelling in ["false", "False", "NO", "0"] {
            unsafe { std::env::set_var("IBKR_DX_ISLAND_FOR_NASDAQ", spelling) };
            assert!(
                !GatewaySettings::default().resolve().island_for_nasdaq,
                "{spelling} left the older spelling on",
            );
        }
        unsafe { std::env::remove_var("IBKR_DX_ISLAND_FOR_NASDAQ") };

        for (stated, file) in [("ids.json", Some("ids.json")), ("", None)] {
            unsafe { std::env::set_var("IBKR_DX_ORDER_ID_FILE", stated) };
            assert_eq!(
                GatewaySettings::default().resolve().order_id_file.as_deref(),
                file.map(std::path::Path::new),
                "{stated:?}, where an empty path keeps no file",
            );
        }
        unsafe { std::env::remove_var("IBKR_DX_ORDER_ID_FILE") };
        // A data directory that is not absolute names no default file.
        #[cfg(not(target_os = "windows"))]
        {
            unsafe { std::env::remove_var("XDG_DATA_HOME") };
            unsafe { std::env::set_var("HOME", "relative") };
            assert_eq!(GatewaySettings::default().resolve().order_id_file, None);
        }
    }

    /// A setting is one field on the wire, and a value that would end that
    /// field early does not become one.
    ///
    /// Two characters end a field on the wires this client writes: the
    /// separator FIX is written with, and the semicolon the connection request
    /// is written with. Carried through, a zone of `UTC<SOH>6900=0` reaches
    /// the venue as a zone and a tag nobody set, and a client string with a
    /// semicolon in it adds a position to the request the venue reads
    /// positionally.
    #[test]
    fn a_setting_that_would_cut_itself_into_fields_is_refused() {
        let cut = GatewaySettings {
            locale: Some(crate::config::IB_LOCALE.to_string()),
            timezone: Some("UTC\u{1}6900=0".into()),
            encoded: Some("j/p/en_US/S;extra".into()),
            build: Some("9999".into()),
            ..Default::default()
        };
        let resolved = cut.resolve();
        assert_eq!(resolved.timezone, "UTC", "the default zone stands");
        assert_eq!(
            resolved.encoded, crate::config::IB_ENCODED,
            "the client string this session announces is the settled one",
        );
        assert_eq!(resolved.build, "9999", "and a value that states one thing is kept");
    }

    /// Every field of the stated form reaches the resolved one. A field added
    /// to one and not the other is a setting a caller can state and nothing
    /// reads.
    #[test]
    fn every_setting_is_resolved() {
        let all = GatewaySettings {
            timezone: Some("t".into()),
            locale: Some("l".into()),
            build: Some("b".into()),
            version: Some("v".into()),
            encoded: Some("e".into()),
            hardware_id: Some("h".into()),
            mac_address: Some("AA:BB:CC:DD:EE:FF".into()),
            lan_ip: Some("10.0.0.9".into()),
            market_data_host: Some("m".into()),
            port: Some(1),
            log_level: Some("debug".into()),
            log_dir: Some("d".into()),
            log_queue: Some(4096),
            order_id_file: Some("state/order-ids.json".into()),
            execution_reports: Some(ExecutionReportScope::Today),
            island_for_nasdaq: Some(false),
            reconnect_on_socket_err: Some(false),
        };
        let resolved = all.resolve();
        assert_eq!(resolved.order_id_file.as_deref(), Some(std::path::Path::new("state/order-ids.json")));
        assert_eq!(resolved.timezone, "t");
        assert_eq!(resolved.locale, "l");
        assert_eq!(resolved.build, "b");
        assert_eq!(resolved.version, "v");
        assert_eq!(resolved.encoded, "e");
        assert_eq!(resolved.hardware_id.as_deref(), Some("h"));
        assert_eq!(resolved.mac_address.as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        assert_eq!(resolved.lan_ip.as_deref(), Some("10.0.0.9"));
        assert_eq!(resolved.market_data_host.as_deref(), Some("m"));
        assert_eq!(resolved.port, 1);
        assert_eq!(resolved.execution_reports, ExecutionReportScope::Today);
        assert!(!resolved.island_for_nasdaq);
        assert!(!resolved.reconnect_on_socket_err);
        // The three log settings are process-scoped by nature — one logger per
        // process — so they are not on the resolved form. They reach the
        // logger instead, and are checked here for the same reason: stated
        // and reaching neither, they were a setting a caller could state and
        // nothing read.
        let logging = crate::logging::LogConfig::stated(&all);
        assert_eq!(logging.level.as_deref(), Some("debug"));
        assert_eq!(logging.log_dir.as_deref(), Some(std::path::Path::new("d")));
        assert_eq!(logging.queue_capacity, 4096);

        // Destructured without `..`, so a field added to the stated form stops
        // compiling here until it is resolved above.
        let GatewaySettings {
            timezone: _, locale: _, build: _, version: _, encoded: _, hardware_id: _,
            mac_address: _, lan_ip: _,
            market_data_host: _, port: _,
            log_level: _, log_dir: _, log_queue: _,
            order_id_file: _,
            execution_reports: _, island_for_nasdaq: _, reconnect_on_socket_err: _,
        } = all;
    }
}
