# API Coverage Matrix (v0.1.0)

*Auto-generated from source — do not edit.*

Canonical IB API methods vs ibkr-dx implementation status.

- **Y** = Implemented: a call is served; a callback is declared and fired
  whenever what it reports arrives
- **STUB** = Present and not served: a call reports why on the error
  callback; a callback is declared and not fired here, although a gateway
  sends it
- **-** = Not present

The callback table also says whether each callback fires at all for a
program on a gateway. Seven declared by the TWS API never do, so they fire
neither there nor here.

The evidence column says how each status was established, and is
derived rather than asserted: a call is credited to the live session
only when a suite that runs against a real account names it, and to
the offline suites only when a test names it.

## Summary

| | IB API | Rust | Python |
|---|:---:|:---:|:---:|
| **EClient methods** | 87 | 87 impl, 0 stub | 87 impl, 0 stub |
| **EWrapper callbacks** | 90 | 88 impl, 2 stub | 88 impl, 2 stub |

## EClient Methods

| Category | IB API Method | C++ Name | Rust | Python | Evidence |
|----------|---------------|----------|:----:|:------:|----------|
| Connection | `connect` | `eConnect` | Y | Y | Live session |
|  | `disconnect` | `eDisconnect` | Y | Y | Live session |
|  | `is_connected` | `isConnected` | Y | Y | Live session |
|  | `start_api` | `startApi` | Y | Y | Offline suites |
|  | `set_server_log_level` | `setServerLogLevel` | Y | Y | Live session |
|  | `req_current_time` | `reqCurrentTime` | Y | Y | Live session |
|  | `req_current_time_in_millis` | `reqCurrentTimeInMillis` | Y | Y | Live session |
|  | `verify_request` | `verifyRequest` | Y | Y | Offline suites |
|  | `verify_message` | `verifyMessage` | Y | Y | Offline suites |
|  | `verify_and_auth_request` | `verifyAndAuthRequest` | Y | Y | Offline suites |
|  | `verify_and_auth_message` | `verifyAndAuthMessage` | Y | Y | Offline suites |
| Market Data | `req_mkt_data` | `reqMktData` | Y | Y | Live session |
|  | `cancel_mkt_data` | `cancelMktData` | Y | Y | Live session |
|  | `req_market_data_type` | `reqMarketDataType` | Y | Y | Live session |
|  | `req_tick_by_tick_data` | `reqTickByTickData` | Y | Y | Live session |
|  | `cancel_tick_by_tick_data` | `cancelTickByTickData` | Y | Y | Live session |
|  | `req_mkt_depth` | `reqMktDepth` | Y | Y | Live session |
|  | `cancel_mkt_depth` | `cancelMktDepth` | Y | Y | Live session |
|  | `req_mkt_depth_exchanges` | `reqMktDepthExchanges` | Y | Y | Live session |
|  | `req_smart_components` | `reqSmartComponents` | Y | Y | Live session |
|  | `req_real_time_bars` | `reqRealTimeBars` | Y | Y | Live session |
|  | `cancel_real_time_bars` | `cancelRealTimeBars` | Y | Y | Live session |
| Historical Data | `req_historical_data` | `reqHistoricalData` | Y | Y | Live session |
|  | `cancel_historical_data` | `cancelHistoricalData` | Y | Y | Live session |
|  | `req_head_time_stamp` | `reqHeadTimeStamp` | Y | Y | Live session |
|  | `cancel_head_time_stamp` | `cancelHeadTimestamp` | Y | Y | Live session |
|  | `req_historical_ticks` | `reqHistoricalTicks` | Y | Y | Live session |
|  | `cancel_historical_ticks` | `cancelHistoricalTicks` | Y | Y | Offline suites |
|  | `req_histogram_data` | `reqHistogramData` | Y | Y | Live session |
|  | `cancel_histogram_data` | `cancelHistogramData` | Y | Y | Live session |
|  | `req_historical_schedule` | `reqHistoricalSchedule` | Y | Y | Live session |
| Orders | `place_order` | `placeOrder` | Y | Y | Live session |
|  | `cancel_order` | `cancelOrder` | Y | Y | Live session |
|  | `req_open_orders` | `reqOpenOrders` | Y | Y | Live session |
|  | `req_all_open_orders` | `reqAllOpenOrders` | Y | Y | Live session |
|  | `req_auto_open_orders` | `reqAutoOpenOrders` | Y | Y | Live session |
|  | `req_ids` | `reqIds` | Y | Y | Live session |
|  | `req_global_cancel` | `reqGlobalCancel` | Y | Y | Live session |
|  | `req_completed_orders` | `reqCompletedOrders` | Y | Y | Live session |
| Executions | `req_executions` | `reqExecutions` | Y | Y | Live session |
| Account | `req_account_updates` | `reqAccountUpdates` | Y | Y | Live session |
|  | `req_account_summary` | `reqAccountSummary` | Y | Y | Live session |
|  | `cancel_account_summary` | `cancelAccountSummary` | Y | Y | Live session |
|  | `req_positions` | `reqPositions` | Y | Y | Live session |
|  | `cancel_positions` | `cancelPositions` | Y | Y | Live session |
|  | `req_pnl` | `reqPnL` | Y | Y | Live session |
|  | `cancel_pnl` | `cancelPnL` | Y | Y | Live session |
|  | `req_pnl_single` | `reqPnLSingle` | Y | Y | Live session |
|  | `cancel_pnl_single` | `cancelPnLSingle` | Y | Y | Live session |
|  | `req_managed_accts` | `reqManagedAccts` | Y | Y | Live session |
|  | `req_account_updates_multi` | `reqAccountUpdatesMulti` | Y | Y | Live session |
|  | `cancel_account_updates_multi` | `cancelAccountUpdatesMulti` | Y | Y | Live session |
|  | `req_positions_multi` | `reqPositionsMulti` | Y | Y | Live session |
|  | `cancel_positions_multi` | `cancelPositionsMulti` | Y | Y | Live session |
| Contract | `req_contract_details` | `reqContractDetails` | Y | Y | Live session |
|  | `cancel_contract_data` | `cancelContractData` | Y | Y | Offline suites |
|  | `req_matching_symbols` | `reqMatchingSymbols` | Y | Y | Live session |
|  | `req_market_rule` | `reqMarketRule` | Y | Y | Live session |
| Scanner | `req_scanner_parameters` | `reqScannerParameters` | Y | Y | Live session |
|  | `req_scanner_subscription` | `reqScannerSubscription` | Y | Y | Live session |
|  | `cancel_scanner_subscription` | `cancelScannerSubscription` | Y | Y | Live session |
| News | `req_news_providers` | `reqNewsProviders` | Y | Y | Live session |
|  | `req_news_article` | `reqNewsArticle` | Y | Y | Live session |
|  | `req_historical_news` | `reqHistoricalNews` | Y | Y | Live session |
|  | `req_news_bulletins` | `reqNewsBulletins` | Y | Y | Live session |
|  | `cancel_news_bulletins` | `cancelNewsBulletins` | Y | Y | Live session |
| Fundamental | `req_fundamental_data` | `reqFundamentalData` | Y | Y | Live session |
|  | `cancel_fundamental_data` | `cancelFundamentalData` | Y | Y | Live session |
| Options | `calculate_implied_volatility` | `calculateImpliedVolatility` | Y | Y | Live session |
|  | `cancel_calculate_implied_volatility` | `cancelCalculateImpliedVolatility` | Y | Y | Live session |
|  | `calculate_option_price` | `calculateOptionPrice` | Y | Y | Live session |
|  | `cancel_calculate_option_price` | `cancelCalculateOptionPrice` | Y | Y | Live session |
|  | `exercise_options` | `exerciseOptions` | Y | Y | Live session |
|  | `req_sec_def_opt_params` | `reqSecDefOptParams` | Y | Y | Live session |
| Reference | `req_soft_dollar_tiers` | `reqSoftDollarTiers` | Y | Y | Live session |
|  | `req_family_codes` | `reqFamilyCodes` | Y | Y | Live session |
|  | `req_user_info` | `reqUserInfo` | Y | Y | Live session |
| Financial Advisor | `request_fa` | `requestFA` | Y | Y | Offline suites |
|  | `replace_fa` | `replaceFA` | Y | Y | Offline suites |
| Display Groups | `query_display_groups` | `queryDisplayGroups` | Y | Y | Live session |
|  | `subscribe_to_group_events` | `subscribeToGroupEvents` | Y | Y | Live session |
|  | `unsubscribe_from_group_events` | `unsubscribeFromGroupEvents` | Y | Y | Live session |
|  | `update_display_group` | `updateDisplayGroup` | Y | Y | Live session |
| WSH | `req_wsh_meta_data` | `reqWshMetaData` | Y | Y | Live session |
|  | `cancel_wsh_meta_data` | `cancelWshMetaData` | Y | Y | Live session |
|  | `req_wsh_event_data` | `reqWshEventData` | Y | Y | Live session |
|  | `cancel_wsh_event_data` | `cancelWshEventData` | Y | Y | Live session |

## EWrapper Callbacks

| Category | Callback | Rust | Python | Fires on a gateway |
|----------|----------|:----:|:------:|:------------------:|
| Connection | `connect_ack` | Y | Y | yes |
|  | `connection_closed` | Y | Y | yes |
|  | `next_valid_id` | Y | Y | yes |
|  | `managed_accounts` | Y | Y | yes |
|  | `error` | Y | Y | yes |
|  | `current_time` | Y | Y | yes |
|  | `current_time_in_millis` | Y | Y | yes |
| Market Data | `tick_price` | Y | Y | yes |
|  | `tick_size` | Y | Y | yes |
|  | `tick_string` | Y | Y | yes |
|  | `tick_generic` | Y | Y | yes |
|  | `tick_snapshot_end` | Y | Y | yes |
|  | `market_data_type` | Y | Y | yes |
|  | `tick_req_params` | Y | Y | yes |
| Orders | `order_status` | Y | Y | yes |
|  | `open_order` | Y | Y | yes |
|  | `open_order_end` | Y | Y | yes |
|  | `order_bound` | Y | Y | yes |
| Executions | `exec_details` | Y | Y | yes |
|  | `exec_details_end` | Y | Y | yes |
|  | `commission_and_fees_report` | Y | Y | yes |
| Account | `update_account_value` | Y | Y | yes |
|  | `update_portfolio` | Y | Y | yes |
|  | `update_account_time` | Y | Y | yes |
|  | `account_download_end` | Y | Y | yes |
|  | `account_summary` | Y | Y | yes |
|  | `account_summary_end` | Y | Y | yes |
|  | `position` | Y | Y | yes |
|  | `position_end` | Y | Y | yes |
|  | `pnl` | Y | Y | yes |
|  | `pnl_single` | Y | Y | yes |
|  | `position_multi` | Y | Y | yes |
|  | `position_multi_end` | Y | Y | yes |
|  | `account_update_multi` | Y | Y | yes |
|  | `account_update_multi_end` | Y | Y | yes |
| Contract | `contract_details` | Y | Y | yes |
|  | `contract_details_end` | Y | Y | yes |
|  | `bond_contract_details` | Y | Y | yes |
|  | `symbol_samples` | Y | Y | yes |
| Historical Data | `historical_data` | Y | Y | yes |
|  | `historical_data_end` | Y | Y | yes |
|  | `historical_data_update` | Y | Y | yes |
|  | `head_timestamp` | Y | Y | yes |
|  | `historical_ticks` | Y | Y | yes |
|  | `historical_ticks_bid_ask` | Y | Y | yes |
|  | `historical_ticks_last` | Y | Y | yes |
|  | `histogram_data` | Y | Y | yes |
|  | `historical_schedule` | Y | Y | yes |
| Market Depth | `update_mkt_depth` | Y | Y | yes |
|  | `update_mkt_depth_l2` | Y | Y | yes |
|  | `mkt_depth_exchanges` | Y | Y | yes |
| Tick-by-Tick | `tick_by_tick_all_last` | Y | Y | yes |
|  | `tick_by_tick_bid_ask` | Y | Y | yes |
|  | `tick_by_tick_mid_point` | Y | Y | yes |
| Scanner | `scanner_data` | Y | Y | yes |
|  | `scanner_data_end` | Y | Y | yes |
|  | `scanner_parameters` | Y | Y | yes |
| News | `news_providers` | Y | Y | yes |
|  | `news_article` | Y | Y | yes |
|  | `historical_news` | Y | Y | yes |
|  | `historical_news_end` | Y | Y | yes |
|  | `tick_news` | Y | Y | yes |
|  | `update_news_bulletin` | Y | Y | yes |
| Real-Time Bars | `real_time_bar` | Y | Y | yes |
| Fundamental | `fundamental_data` | Y | Y | yes |
| Market Rules | `market_rule` | Y | Y | yes |
| Completed Orders | `completed_order` | Y | Y | yes |
|  | `completed_orders_end` | Y | Y | yes |
| Options | `tick_option_computation` | Y | Y | yes |
|  | `security_definition_option_parameter` | Y | Y | yes |
|  | `security_definition_option_parameter_end` | Y | Y | yes |
| Reference | `smart_components` | Y | Y | yes |
|  | `soft_dollar_tiers` | Y | Y | yes |
|  | `family_codes` | Y | Y | yes |
|  | `user_info` | Y | Y | yes |
| FA | `receive_fa` | Y | Y | yes |
|  | `replace_fa_end` | Y | Y | yes |
| Display Groups | `display_group_list` | Y | Y | yes |
|  | `display_group_updated` | Y | Y | yes |
| Other | `delta_neutral_validation` | Y | Y | no |
| WSH | `wsh_meta_data` | Y | Y | yes |
|  | `wsh_event_data` | Y | Y | yes |
| Market Data | `reroute_mkt_data_req` | STUB | STUB | yes |
|  | `reroute_mkt_depth_req` | STUB | STUB | yes |
|  | `tick_efp` | Y | Y | no |
| Connection | `verify_message_api` | Y | Y | no |
|  | `verify_completed` | Y | Y | no |
|  | `verify_and_auth_message_api` | Y | Y | no |
|  | `verify_and_auth_completed` | Y | Y | no |
|  | `win_error` | Y | Y | no |
