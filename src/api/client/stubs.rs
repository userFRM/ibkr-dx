//! Methods answered from what the session was told at logon, and the calls a
//! gateway answers itself rather than asking the venue.

use crate::error_codes::{BAD_MESSAGE, LOG_LEVEL_INVALID, Refusal};

use super::EClient;


impl EClient {
    /// Request configuration. Reports 10357 through [`Wrapper::error`](crate::api::wrapper::Wrapper::error).
    pub fn req_config(&self, req_id: i64) {
        self.report_reason(req_id, &if self.session_over() {
            Refusal::not_connected("Not connected")
        } else {
            Refusal::stated(
                crate::error_codes::CONFIGURATION_ACCESS_UNAVAILABLE,
                crate::error_codes::CONFIGURATION_ACCESS_MESSAGE,
            )
        });
    }

    /// Request a configuration update. Reports 10357 through [`Wrapper::error`](crate::api::wrapper::Wrapper::error).
    pub fn update_config(&self, req_id: i64) {
        self.req_config(req_id);
    }

    // ── Smart Components ──

    /// Request smart routing components for a BBO exchange. Matches
    /// `reqSmartComponents` in C++.
    ///
    /// Answered from the map of venues the venue stated beside the
    /// subscription whose acknowledgement named that BBO exchange — the one
    /// `tick_req_params` states. The venue states a map per BBO exchange and
    /// security type, so one contract's venues are not another's.
    ///
    /// A BBO exchange no subscription named is refused as a gateway refuses
    /// it. One whose map has not arrived yet is waited for up to two seconds,
    /// as a gateway waits, and answered from `process_msgs` rather than by
    /// holding this call.
    pub fn req_smart_components(&self, req_id: i64, bbo_exchange: &str) {
        if self.session_over() { return self.refuse_session(&Refusal::not_connected("Not connected")); }
        match self.shared.reference.ask_smart_components(req_id, bbo_exchange) {
            Ok(Some(components)) => {
                self.reply(crate::bridge::Reply::SmartComponents(req_id, components));
            }
            Ok(None) => {}
            Err(why) => self.refuse_request(req_id, &why),
        }
    }

    // ── News Providers ──

    /// Request available news providers. Matches `reqNewsProviders` in C++.
    /// Gateway-local — returns provider list from init data.
    pub fn req_news_providers(&self) {
        use crate::types::model::Question;
        if self.session_over() {
            return self.refuse_question(Question::NewsProviders, &Refusal::not_connected("Not connected"));
        }
        let providers = self.shared.reference.news_providers();
        self.reply(crate::bridge::Reply::NewsProviders(providers));
    }

    // ── Server Time ──

    /// The venue's clock, as `reqCurrentTime` reports it.
    ///
    /// The venue is never asked. There is no request for this on the wire, so
    /// the answer is worked out here: this machine's clock, shifted by what
    /// the venue has stated about its own — on the logon it stamps, and in the
    /// clock it pushes afterwards. A session that has been told nothing is
    /// shifted by nothing and answers this machine's clock, which is what a
    /// caller who asks before the venue has said anything gets. This is a
    /// question that always has an answer, and never a refusal.
    pub fn req_current_time(&self) {
        use crate::types::model::Question;
        if self.session_over() {
            return self.refuse_question(Question::CurrentTime, &Refusal::not_connected("Not connected"));
        }
        self.reply(crate::bridge::Reply::CurrentTime(
            self.shared.market.venue_time_millis().div_euclid(1_000),
        ));
    }

    /// The venue's clock in milliseconds, as `reqCurrentTimeInMillis` reports it.
    ///
    /// The same clock [`req_current_time`](Self::req_current_time) reports and
    /// worked out the same way. What differs is the precision kept: asking in
    /// seconds throws away the fraction this one keeps.
    pub fn req_current_time_in_millis(&self) {
        use crate::types::model::Question;
        if self.session_over() {
            return self.refuse_question(
                Question::CurrentTimeInMillis, &Refusal::not_connected("Not connected"),
            );
        }
        self.reply(crate::bridge::Reply::CurrentTimeInMillis(self.shared.market.venue_time_millis()));
    }

    // ── FA (Financial Advisor) ──

    /// Ask the venue for a partition of the advisor's own configuration.
    ///
    /// The reference client names the partition by a number — its aliases, its
    /// groups, its allocation profiles — and the venue names it by a word, so
    /// the number is turned into the word it stands for. A number that stands
    /// for nothing is refused rather than sent as an empty partition.
    ///
    /// The venue's answer reaches [`Wrapper::receive_fa`](crate::api::wrapper::Wrapper::receive_fa) under the same
    /// number the partition was asked for by.
    pub fn request_fa(&self, fa_data_type: i32) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            let partition = advisor_partition(fa_data_type)
                .ok_or_else(|| format!("no advisor configuration is named by {fa_data_type}"))?;
            self.send(crate::types::ControlCommand::AdvisorConfig {
                // Nothing to carry back: the answer to a question about a
                // partition names the partition, not a request.
                req_id: -1,
                // Asking for it by name.
                command: 5,
                partition: partition.to_string(),
                fa_data_type,
                document: None,
            })
        })() {
            self.refuse_question(crate::types::model::Question::Fa, &why);
        }
    }


    /// Replace a partition of the advisor's configuration with the one given.
    ///
    /// [`Wrapper::replace_fa_end`](crate::api::wrapper::Wrapper::replace_fa_end) fires with `req_id` once the venue has
    /// taken it, and a venue that refuses states why on [`Wrapper::error`](crate::api::wrapper::Wrapper::error)
    /// under the same number.
    pub fn replace_fa(&self, req_id: i64, fa_data_type: i32, cxml: &str) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            let partition = advisor_partition(fa_data_type)
                .ok_or_else(|| format!("no advisor configuration is named by {fa_data_type}"))?;
            self.send(crate::types::ControlCommand::AdvisorConfig {
                req_id,
                // Replacing it with what is carried.
                command: 3,
                partition: partition.to_string(),
                fa_data_type,
                document: Some(cxml.to_string()),
            })
        })() {
            self.refuse_request(req_id, &why);
        }
    }


    // ── Option calculations ──
    //
    // A volatility inverted from a price, and a price implied by a volatility.
    // This protocol carries no request for either: nothing it sends takes a
    // caller-supplied option price or volatility for the venue to work back
    // from. They exist so a caller written against the reference client finds
    // the call and is told why it cannot be served, rather than finding
    // nothing at all.

    /// What volatility a price implies, under the venue's model.
    ///
    /// This protocol carries no request for it, so the value is computed
    /// here, anchored to the venue's last stated model output for this
    /// contract, and delivered on `tick_option_computation` under 53 in its
    /// place in the session's order. Where the venue has stated no model, the
    /// contract is watched so it states one, and the engine answers where the
    /// model arrives, right behind it — rather than the question being refused
    /// for having been asked first.
    ///
    /// Answered over a year, which is the scale `tick_option_computation`
    /// reports the venue's own volatility on, so the two read against each
    /// other.
    pub fn calculate_implied_volatility(
        &self, req_id: i64, contract: &super::Contract,
        option_price: f64, under_price: f64,
    ) {
        self.calculate(req_id, contract, true, option_price, under_price);
    }

    /// What price a volatility implies, under that same model.
    pub fn calculate_option_price(
        &self, req_id: i64, contract: &super::Contract,
        volatility: f64, under_price: f64,
    ) {
        self.calculate(req_id, contract, false, volatility, under_price);
    }

    /// Answer a calculation from the model stated, or keep it for the model
    /// the watch it opens will bring.
    fn calculate(
        &self, req_id: i64, contract: &super::Contract,
        wants_volatility: bool, asked: f64, under_price: f64,
    ) {
        let solved = self.solve_option(contract, None, |terms, model, schedule| {
            if wants_volatility {
                crate::control::option_model::implied_volatility(
                    terms, model, schedule, asked, under_price,
                )
            } else {
                crate::control::option_model::option_price(
                    terms, model, schedule, asked, under_price,
                )
            }
        });
        match solved {
            Ok(figure) => {
                let (implied_vol, opt_price) =
                    if wants_volatility { (figure, asked) } else { (asked, figure) };
                self.shared.market.push_option_computation(crate::types::OptionComputation {
                    implied_vol,
                    opt_price,
                    und_price: under_price,
                    ..crate::types::OptionComputation::solved(req_id)
                });
            }
            // The venue states a model for a contract that is watched. Asking
            // about one nobody is watching opens the watch and answers when
            // the model arrives, which is what the caller asked for.
            //
            // Only where that is the trouble. A model already stated and a
            // question it cannot answer is not something waiting will fix,
            // and kept anyway the caller was given neither an answer nor a
            // reason and waited on a model that had already arrived.
            Err(why) if why.message == crate::client_core::OPTION_MODEL_UNSTATED => {
                if let Err(why) = self.watch_for_option_model(
                    req_id, contract, wants_volatility, asked, under_price,
                ) {
                    self.report_reason(req_id, &why);
                }
            }
            Err(why) => self.report_reason(req_id, &why),
        }
    }

    /// Watch a contract so the venue states a model for it, and keep the
    /// question where the engine answers it when it does.
    ///
    /// The engine opens the watch under the question's own number and keeps
    /// the question on the slot it is served on; a watch it cannot open is
    /// refused under that number, and a request already watching the contract
    /// keeps the question on the watch it holds.
    fn watch_for_option_model(
        &self, req_id: i64, contract: &super::Contract,
        wants_volatility: bool, option_price: f64, under_price: f64,
    ) -> Result<(), Refusal> {
        let mode = self.core.subscription_mode();
        self.ask_for_mkt_data(
            req_id, contract, "", false, false, mode, None,
            Some(Box::new(crate::types::Calculation {
                contract: contract.clone(), wants_volatility, option_price, under_price,
            })),
        )
    }

    /// The contract's terms and the venue's model for it, or why neither
    /// question can be answered.
    fn solve_option(
        &self,
        contract: &super::Contract,
        watched_under: Option<i64>,
        solve: impl Fn(
            crate::control::option_model::OptionTerms,
            crate::control::option_model::VenueModel,
            &[(f64, f64)],
        ) -> Option<f64>,
    ) -> Result<f64, Refusal> {
        self.core.solve_option(&self.shared, contract, watched_under, solve)
    }

    /// Withdraw a question that was waiting on the venue to state a model.
    ///
    /// A question answered from a model already stated started nothing and
    /// stops nothing. One that opened a watch to get an answer withdraws it
    /// here, so a caller that changes its mind is not left watching a
    /// contract it no longer asks about.
    pub fn cancel_calculate_implied_volatility(&self, req_id: i64) {
        self.forget_option_calc(req_id);
    }

    /// As for [`cancel_calculate_implied_volatility`](Self::cancel_calculate_implied_volatility).
    pub fn cancel_calculate_option_price(&self, req_id: i64) {
        self.forget_option_calc(req_id);
    }

    /// Drop a kept question and the watch it opened.
    fn forget_option_calc(&self, req_id: i64) {
        // The engine forgets the question with the watch it opened for it, in
        // the order it took them; a watch the caller opened itself stays.
        let _ = self.send(crate::types::ControlCommand::CancelCalculation { req_id });
    }

    // ── Display Groups ──

    /// Query display groups. Matches `queryDisplayGroups` in C++.
    /// The display groups on offer. Answered on `display_group_list`.
    ///
    /// A display group is a way for several callers on one session to agree on
    /// a contract. Nothing about one crosses this wire, so they are kept here
    /// and served to callers from here.
    pub fn query_display_groups(&self, req_id: i64) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        self.core.query_display_groups(req_id);
        self.say_group_events();
    }

    /// Follow a display group. Answered on `display_group_updated`, at once
    /// with what the group holds and again whenever it changes.
    pub fn subscribe_to_group_events(&self, req_id: i64, group_id: i32) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        self.core.subscribe_to_group_events(req_id, group_id);
        self.say_group_events();
    }

    /// Stop following a display group.
    pub fn unsubscribe_from_group_events(&self, req_id: i64) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        self.core.unsubscribe_from_group_events(req_id);
    }

    /// Put a contract in the group this request follows, stated as
    /// `conId@exchange`, or `none` to empty it. Every follower of that group is
    /// told, including this one.
    pub fn update_display_group(&self, req_id: i64, contract_info: &str) {
        if let Err(why) = (|| -> Result<(), Refusal> {
            self.core.update_display_group(req_id, contract_info)
                .map_err(Refusal::from)?;
            self.say_group_events();
            Ok(())
        })() {
            self.refuse_request(req_id, &why);
        }
    }


    /// What the display groups answered at the call, pushed where the call
    /// stands in the session's order: the groups on offer, and what a group
    /// holds to everyone following it.
    fn say_group_events(&self) {
        for event in self.core.drain_group_events() {
            self.reply(match event {
                crate::client_core::GroupEvent::List(req_id, groups) => {
                    crate::bridge::Reply::DisplayGroupList(req_id, groups)
                }
                crate::client_core::GroupEvent::Updated(req_id, info) => {
                    crate::bridge::Reply::DisplayGroupUpdated(req_id, info)
                }
            });
        }
    }

    // ── Verification ──
    //
    // A handshake between a gateway and a program linking to it. The
    // reference client answers the two requests itself, because intent to
    // authenticate is stated on the initial connect and it never states it;
    // the two messages it does send, a gateway reads and discards.

    /// Answered as the reference client answers it: on `error`, under 508 and
    /// no request, because intent to authenticate is stated on the initial
    /// connect and was not. Nothing is sent, so `api_name` and `api_version`
    /// reach nothing.
    pub fn verify_request(&self, api_name: &str, api_version: &str) {
        let _ = (api_name, api_version);
        self.refuse_verification();
    }

    /// As [`verify_request`](Self::verify_request): `api_name`, `api_version`
    /// and `opaque_isv_key` reach nothing.
    pub fn verify_and_auth_request(&self, api_name: &str, api_version: &str, opaque_isv_key: &str) {
        let _ = (api_name, api_version, opaque_isv_key);
        self.refuse_verification();
    }

    /// Nothing is sent and nothing answers. A gateway reads this message and
    /// discards it, so a program on one is answered by nothing either, and
    /// `api_data` reaches nothing there or here.
    pub fn verify_message(&self, api_data: &str) {
        let _ = api_data;
        if self.session_over() { self.report_reason(-1, &Refusal::not_connected("Not connected")); }
    }

    /// As [`verify_message`](Self::verify_message): `api_data` and
    /// `xyz_response` reach nothing.
    pub fn verify_and_auth_message(&self, api_data: &str, xyz_response: &str) {
        let _ = (api_data, xyz_response);
        if self.session_over() { self.report_reason(-1, &Refusal::not_connected("Not connected")); }
    }

    fn refuse_verification(&self) {
        if self.session_over() { return self.report_reason(-1, &Refusal::not_connected("Not connected")); }
        self.report_reason(-1, &Refusal::stated(
            BAD_MESSAGE,
            "Bad message  Intent to authenticate needs to be expressed during initial connect request.",
        ));
    }

    // ── Soft Dollar Tiers ──

    /// Request soft dollar tiers. Matches `reqSoftDollarTiers` in C++.
    /// Gateway-local — returns tiers parsed from CCP logon tag 6560.
    pub fn req_soft_dollar_tiers(&self, req_id: i64) {
        if self.session_over() { return self.refuse_session(&Refusal::not_connected("Not connected")); }
        let tiers = self.shared.reference.soft_dollar_tiers();
        self.reply(crate::bridge::Reply::SoftDollarTiers(req_id, tiers));
    }

    // ── Family Codes ──

    /// Request family codes. Matches `reqFamilyCodes` in C++.
    /// Gateway-local — returns codes parsed from CCP logon tag 6823.
    pub fn req_family_codes(&self) {
        use crate::types::model::Question;
        if self.session_over() {
            return self.refuse_question(Question::FamilyCodes, &Refusal::not_connected("Not connected"));
        }
        let codes = self.shared.reference.family_codes();
        self.reply(crate::bridge::Reply::FamilyCodes(codes));
    }

    // ── Server Log Level ──

    /// Set server log level. Matches `setServerLogLevel` in C++.
    ///
    /// 1 to 5 are a gateway's System, Error, Warning, Info and Detail, and set
    /// this client's logger to error, error, warn, info and trace. A
    /// gateway applies the level to its own log; this client, which serves the
    /// caller in its place, applies it to the logger it installed. Nothing goes
    /// to the venue, which has no message for it.
    ///
    /// Where the program installed a logger of its own, that logger's level is
    /// the program's, and the call says so on the error callback rather than
    /// reporting a level it did not set. A level outside 1 to 5 is refused the
    /// same way.
    pub fn set_server_log_level(&self, log_level: i32) {
        let level = match crate::logging::gateway_level(log_level) {
            Some(level) => level,
            // Refused rather than substituted. Reading a level nobody asked
            // for as `warn` told the caller nothing and left them believing
            // they had set the level they named.
            _ => {
                return self.report_reason(crate::bridge::ReferenceState::NO_REQUEST as i64, &Refusal::stated(
                    LOG_LEVEL_INVALID,
                    format!("set_server_log_level: {log_level} is not a log level; it is 1 to 5"),
                ));
            }
        };
        // The level of the logger this client installed, moved where there is
        // one to move. Nothing goes to the venue: this protocol carries no
        // message asking one to change how loudly it talks, and a level a
        // caller states is about the thing serving that caller — which, on a
        // client that runs in the caller's own process, is this library.
        if crate::logging::set_level(level).is_ok() {
            log::info!("set_server_log_level: logging at {level} (level {log_level})");
            return;
        }
        // A program that installed its own logger keeps it, and saying the
        // level moved when it did not is worse than saying it did not.
        self.report_reason(crate::bridge::ReferenceState::NO_REQUEST as i64, &Refusal::stated(
            LOG_LEVEL_INVALID,
            format!(
                "set_server_log_level: {level} was not applied because this session did \
                 not install the logger; whoever did holds the level"
            ),
        ));
    }

    // ── User Info ──

    /// Request user info. Matches `reqUserInfo` in C++.
    /// Gateway-local — returns whiteBrandingId from CCP logon.
    pub fn req_user_info(&self, req_id: i64) {
        if self.session_over() { return self.refuse_session(&Refusal::not_connected("Not connected")); }
        let id = self.shared.reference.white_branding_id();
        self.reply(crate::bridge::Reply::UserInfo(req_id, id));
    }

    /// A request this client cannot serve is answered, not ignored. A caller
    /// waiting on a callback that will never come cannot tell that apart from
    /// a slow gateway, so it is told on the channel a venue uses to say it
    /// will not act on a request — in its place in the session's order, after
    /// everything pushed before the call. `-1` names no request, and so does
    /// a number too wide to carry: it is reported against no request rather
    /// than against its own low half.
    pub(crate) fn report_reason(&self, req_id: i64, reason: &Refusal) {
        match super::carried_under(req_id) {
            crate::bridge::ReferenceState::NO_REQUEST => self.refuse_session(reason),
            id => self.refuse_request(i64::from(id), reason),
        }
    }

    /// A request refused at its call, under its own number.
    pub(crate) fn refuse_request(&self, req_id: i64, why: &Refusal) {
        self.refuse(
            crate::types::model::ErrorOrigin::Request { id: req_id, ends: true },
            i64::from(why.code), &why.message,
        );
    }



    /// An operation on an order refused at its call, under the order's number.
    pub(crate) fn refuse_order(&self, order_id: i64, op: crate::types::model::OrderOp, why: &Refusal) {
        self.refuse(
            crate::types::model::ErrorOrigin::Order { id: order_id, op },
            i64::from(why.code), &why.message,
        );
    }

    /// A placement refused at its call: a new order, or a change to one the
    /// venue is working.
    pub(crate) fn refuse_placement(&self, order_id: i64, why: &Refusal) {
        let replacing = u64::try_from(order_id)
            .is_ok_and(|oid| self.core.is_working_at_the_venue(oid, Some(&self.shared)));
        let op = if replacing {
            crate::types::model::OrderOp::Modify
        } else {
            crate::types::model::OrderOp::Place
        };
        self.refuse_order(order_id, op, why);
    }

    /// A request with no number of its own, refused at its call.
    pub(crate) fn refuse_question(&self, q: crate::types::model::Question, why: &Refusal) {
        self.refuse(
            crate::types::model::ErrorOrigin::Question { q, ends: true },
            i64::from(why.code), &why.message,
        );
    }



    /// Refused against the session and no request.
    pub(crate) fn refuse_session(&self, why: &Refusal) {
        self.refuse(
            crate::types::model::ErrorOrigin::Session, i64::from(why.code), &why.message,
        );
    }

    /// Push an answer given at the call, in its place in the order.
    pub(crate) fn reply(&self, reply: crate::bridge::Reply) {
        if self.shared.admission_closed() { return; }
        self.shared.push_call_record(crate::bridge::Record::Reply(reply));
    }

    /// Push an answer this side composes where it is delivered.
    pub(crate) fn answer(&self, answer: crate::bridge::Answer) {
        if self.shared.admission_closed() { return; }
        self.shared.push_call_record(crate::bridge::Record::Answer(answer));
    }
}


/// The word the venue names an advisor's configuration partition by, from the
/// number the reference client names it by.
fn advisor_partition(fa_data_type: i32) -> Option<&'static str> {
    // The order the venue reads them in. Rotated by one here, every
    // advisor request asked for a different partition than the caller named:
    // a request for groups returned aliases, and one for aliases returned
    // nothing the caller could use.
    match fa_data_type {
        1 => Some("Group"),
        2 => Some("Profile"),
        3 => Some("Aliases"),
        _ => None,
    }
}




#[cfg(test)]
mod server_clock_tests {
    use crate::api::client::tests::test_client;
    use crate::api::Wrapper;

    #[derive(Default)]
    struct Heard { seconds: Vec<i64>, millis: Vec<i64>, errors: Vec<i64> }
    impl Wrapper for Heard {
        fn current_time(&mut self, t: i64) { self.seconds.push(t); }
        fn current_time_in_millis(&mut self, t: i64) { self.millis.push(t); }
        fn error(&mut self, _req_id: i64, code: i64, _msg: &str, _: &str) {
            self.errors.push(code);
        }
    }

    fn local_millis() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// A session that has heard nothing from the venue about its clock still
    /// answers, from this machine's.
    ///
    /// The venue is never asked what time it is, so there is nothing this
    /// question waits on and nothing to refuse over. Refused, a caller on a
    /// live session was handed a failure for a call that cannot fail — and one
    /// carrying the code that means there is no session at all.
    #[test]
    fn a_clock_the_venue_has_not_stated_is_answered_not_refused() {
        let (client, _rx, _shared) = test_client();
        let mut heard = Heard::default();

        client.req_current_time(); client.process_msgs(&mut heard);
        client.req_current_time_in_millis(); client.process_msgs(&mut heard);

        assert!(heard.errors.is_empty(), "nothing is refused: {:?}", heard.errors);
        assert!(
            (heard.seconds[0] - local_millis().div_euclid(1_000)).abs() <= 1,
            "this machine's clock, unshifted",
        );
        assert!((heard.millis[0] - local_millis()).abs() < 1_000, "and the same in milliseconds");
    }

    /// What the venue has stated shifts the answer, and the answer goes on
    /// running while the venue says nothing more.
    ///
    /// Answered with the stamp on the last message that happened to arrive,
    /// it stood still on a quiet connection: two readings a moment apart named
    /// the same instant, so a caller measuring the difference between the
    /// clocks watched it grow by exactly the time it had waited.
    #[test]
    fn a_stated_clock_shifts_the_answer_and_keeps_running() {
        let (client, _rx, shared) = test_client();
        let mut heard = Heard::default();

        shared.market.note_venue_time("20260815-12:00:00");
        client.req_current_time(); client.process_msgs(&mut heard);
        client.req_current_time_in_millis(); client.process_msgs(&mut heard);
        assert!(
            (heard.millis[0] - 1_786_795_200_000).abs() < 2_000,
            "the venue's clock, days from this machine's: {:?}", heard.millis,
        );
        assert_eq!(heard.seconds[0], heard.millis[0].div_euclid(1_000), "the same clock");

        std::thread::sleep(std::time::Duration::from_millis(5));
        client.req_current_time_in_millis(); client.process_msgs(&mut heard);
        assert!(heard.millis[1] > heard.millis[0], "and it ran on unasked");
    }
}

#[cfg(test)]
mod advisor_partition_tests {
    use super::advisor_partition;

    /// The reference client names a partition by a number and the venue names
    /// it by a word. Both clients here send the word, and a number that
    /// stands for nothing is refused rather than sent as an empty partition.
    ///
    /// The order the venue reads: one names the group, two the
    /// profile, three the aliases. Rotated by one, every advisor request asked
    /// for a partition the caller had not named, and this test agreed with it.
    #[test]
    fn each_number_names_the_partition_the_venue_knows() {
        assert_eq!(advisor_partition(1), Some("Group"));
        assert_eq!(advisor_partition(2), Some("Profile"));
        assert_eq!(advisor_partition(3), Some("Aliases"));
    }

    #[test]
    fn a_number_that_names_nothing_is_refused() {
        for unknown in [0, 4, -1, i32::MAX] {
            assert_eq!(advisor_partition(unknown), None, "{unknown} was taken");
        }
    }
}

#[cfg(test)]
mod expiry_tests {
    use crate::client_core::years_to_expiry;
    use crate::protocol::datetime::days_from_civil;

    /// A known date, against a known day count. Written out rather than pulled
    /// in, so it is checked rather than trusted.
    #[test]
    fn a_civil_date_counts_the_days_since_the_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2026, 1, 1), 20454);
    }

    /// An expiry already past is no expiry to measure to.
    #[test]
    fn an_expiry_in_the_past_measures_nothing() {
        assert!(years_to_expiry("19990101").is_none());
        assert!(years_to_expiry("").is_none());
        assert!(years_to_expiry("2026").is_none());
    }

    /// One ahead measures the years between, and a longer one measures more.
    #[test]
    fn an_expiry_ahead_measures_the_years_between() {
        let near = years_to_expiry("20301231").expect("a date ahead");
        let far = years_to_expiry("20351231").expect("a date further ahead");
        assert!(near > 0.0 && far > near, "{near} then {far}");
    }

    /// An expiry that cannot exist measures nothing. The day count is
    /// arithmetic and would place a thirteenth month or a thirty-second day
    /// somewhere regardless, so a solve measuring from it would answer from a
    /// day the venue never stated.
    #[test]
    fn an_impossible_expiry_measures_nothing() {
        assert!(years_to_expiry("20301301").is_none(), "a thirteenth month");
        assert!(years_to_expiry("20300001").is_none(), "a zeroth month");
        assert!(years_to_expiry("20300132").is_none(), "a thirty-second day");
        assert!(years_to_expiry("20300100").is_none(), "a zeroth day");
        assert!(years_to_expiry("20310229").is_none(), "February the 29th on a common year");
        assert!(years_to_expiry("21000229").is_none(), "February the 29th on a century year");
        // The calendar edge that must not be refused: leap day where one is.
        assert!(years_to_expiry("20320229").is_some(), "February the 29th on a leap year");
    }

    fn option_contract(con_id: i64, symbol: &str) -> crate::types::model::Contract {
        crate::types::model::Contract {
            con_id,
            symbol: symbol.into(),
            sec_type: "OPT".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            last_trade_date_or_contract_month: "20301220".into(),
            strike: 100.0,
            right: "C".into(),
            ..Default::default()
        }
    }

    /// Questions on one contract share its subscription, and either order of
    /// withdrawal takes it down only when the last question goes.
    #[test]
    fn the_last_option_calculation_withdraws_the_shared_watch() {
        use crate::api::client::tests::{settled, test_client};

        for [first, last] in [[1, 2], [2, 1]] {
            let (client, rx, shared) = test_client();
            let option = option_contract(756733, "SPY");
            client.calculate_implied_volatility(1, &option, 5.0, 100.0);
            client.calculate_implied_volatility(2, &option, 5.0, 100.0);
            let heard = settled(&client, &rx);
            assert_eq!(shared.market.kept_calculation_count(), 2);
            assert!(heard.iter().all(|e| !e.starts_with("error")), "{heard:?}");
            let slot = rx.engine().md_requests[&1].slot;
            assert_eq!(rx.engine().md_requests[&2].slot, slot, "the second question shares the watch");

            client.cancel_calculate_implied_volatility(first);
            settled(&client, &rx);
            assert!(rx.engine().farm.holds_market_data(slot), "the remaining question still needs the model");
            assert!(!client.core.instrument_to_req.lock().unwrap().is_empty());

            client.cancel_calculate_implied_volatility(last);
            settled(&client, &rx);
            assert!(!rx.engine().farm.holds_market_data(slot), "the last withdrawal takes the watch down");
            assert_eq!(shared.market.kept_calculation_count(), 0);
            assert!(!client.core.holds_mkt_data(1));
            assert!(!client.core.holds_mkt_data(2));
            assert!(client.core.instrument_to_req.lock().unwrap().is_empty());
        }
    }

    /// Descriptions carry no conId, so their zeroes say nothing about whether
    /// two questions share a watch. The engine's subscriptions say which goes.
    #[test]
    fn described_option_calculations_withdraw_their_own_watches() {
        use crate::api::client::tests::{settled, test_client};

        let (client, rx, shared) = test_client();
        for (req_id, symbol) in [(1, "SPY"), (2, "QQQ")] {
            client.calculate_option_price(req_id, &option_contract(0, symbol), 0.2, 100.0);
        }
        rx.pump();
        assert!(rx.engine().md_requests.is_empty(), "naming precedes registration");
        rx.name_subscription(1, &option_contract(700001, "SPY"), &shared);
        rx.name_subscription(2, &option_contract(700002, "QQQ"), &shared);
        settled(&client, &rx);
        assert_eq!(shared.market.kept_calculation_count(), 2);
        let (one, two) = {
            let engine = rx.engine();
            (engine.md_requests[&1].slot, engine.md_requests[&2].slot)
        };
        assert_ne!(one, two, "each description is a watch of its own");

        client.cancel_calculate_option_price(1);
        settled(&client, &rx);
        assert!(!rx.engine().md_requests.contains_key(&1));
        assert!(!client.core.holds_mkt_data(1));
        assert!(client.core.holds_mkt_data(2), "the other description remains watched");

        client.cancel_calculate_option_price(2);
        settled(&client, &rx);
        assert!(rx.engine().md_requests.is_empty(), "each watch is withdrawn with its question");
        assert_eq!(shared.market.kept_calculation_count(), 0);
        assert!(!client.core.holds_mkt_data(2));
        assert!(client.core.instrument_to_req.lock().unwrap().is_empty());
    }

    /// A number already watching another contract needs a different number,
    /// and waiting for an option model cannot make that number available.
    #[test]
    fn option_calculations_report_the_duplicate_watch_refusal() {
        use crate::api::client::tests::{engine_refused, settled, test_client};
        use crate::error_codes::DUPLICATE_TICKER_ID;

        for wants_volatility in [true, false] {
            let (client, rx, shared) = test_client();
            let watched = option_contract(756733, "SPY");
            client.try_req_mkt_data(7, &watched, "", false, false).unwrap();
            rx.pump();
            let other = option_contract(0, "QQQ");
            client.try_req_mkt_data(7, &other, "", false, false).unwrap();
            let refused = engine_refused(&rx, &shared);
            assert!(
                matches!(refused.as_slice(), [(7, code, _)] if *code == i64::from(DUPLICATE_TICKER_ID)),
                "{refused:?}",
            );

            if wants_volatility {
                client.calculate_implied_volatility(7, &other, 5.0, 100.0);
            } else {
                client.calculate_option_price(7, &other, 0.2, 100.0);
            }
            assert_eq!(
                engine_refused(&rx, &shared), refused,
                "the caller needs the watch's refusal, not a reason to wait for a model",
            );
            assert_eq!(shared.market.kept_calculation_count(), 0);
            client.cancel_calculate_implied_volatility(7);
            client.cancel_calculate_option_price(7);
            settled(&client, &rx);
            assert!(client.core.holds_mkt_data(7), "a refused question owns no watch to withdraw");
            assert!(rx.engine().md_requests.contains_key(&7), "the original watch stays up");
            crate::api::client::tests::reported(&client, || client.cancel_mkt_data(7)).unwrap();
            settled(&client, &rx);
            assert!(!rx.engine().md_requests.contains_key(&7));
        }
    }

    /// A contract the venue cannot name cannot be watched, so the question
    /// reports that refusal under its own number, and is not kept waiting on
    /// a model the watch will never bring.
    #[test]
    fn option_calculations_report_the_qualification_refusal() {
        use crate::api::client::tests::{settled, test_client};

        for wants_volatility in [true, false] {
            let (client, rx, shared) = test_client();
            let mut option = option_contract(756734, "QQQ");
            option.sec_type.clear();
            if wants_volatility {
                client.calculate_implied_volatility(7, &option, 5.0, 100.0);
            } else {
                client.calculate_option_price(7, &option, 0.2, 100.0);
            }
            settled(&client, &rx);
            assert_eq!(shared.market.kept_calculation_count(), 0, "the contract is named before waiting for its model");

            // The venue names nothing for it.
            {
                let mut engine = rx.engine();
                let engine = &mut *engine;
                for (_, _, asked_at) in &mut engine.ccp.pending_named {
                    *asked_at -= std::time::Duration::from_secs(3600);
                }
                engine.ccp.sweep_pending_named(&shared);
                engine.reclaim_slots_no_order_holds();
            }
            let heard = settled(&client, &rx);
            assert!(
                heard.iter().any(|e| e.starts_with("error:7:200:")),
                "the caller needs the qualification refusal: {heard:?}",
            );
            assert_eq!(
                shared.market.kept_calculation_count(), 0,
                "not a reason to wait for a model",
            );
            assert!(!rx.engine().md_requests.contains_key(&7), "and the watch is gone");
        }
    }
}
