use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use neoethos_core::{
    BrokerCloseDeal, BrokerPositionFinancialRow, BrokerPositionFinancialSnapshot,
    CloseDealReconciliation, CommissionTypeExact, EvidenceProvenance, ExactCommissionSchedule,
    ExactHolidayWindow, ExactProtoOaSymbolContract, ExactSwapSchedule, ExactTradingInterval,
    ExpectedLocalClose, HistoricalBrokerTruthEvidence, MinimumCommissionCurrency,
    PositionDirection, SwapCalculationExact, SynchronizedBidAsk, SynchronizedConversionLeg,
    SynchronizedQuote, reconcile_close_deals,
};

use super::ctrader_account::{
    CTraderDealSnapshot, CTraderReconcileSnapshot, CTraderTraderSnapshot,
    parse_deal_list_by_position_id_response, parse_deal_list_response, parse_reconcile_response,
    parse_trader_response,
};
use super::ctrader_data::{
    CTraderAssetInfo, CTraderResolvedSymbol, CTraderSymbolInfo, CommissionType, DayOfWeek,
    HistoricalTicksResult, MinCommissionType, SwapCalculationType, TradingModeProto,
    parse_asset_list_response, parse_symbol_by_id_response, parse_symbols_list_response,
    parse_tick_data_response,
};
use super::ctrader_messages::parse_get_position_unrealized_pnl_response;
use super::pnl::AuthoritativeUnrealizedPnL;

pub fn source_payload_hash(payloads: &[&str]) -> String {
    let mut bytes = Vec::new();
    for payload in payloads {
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(payload.as_bytes());
    }
    format!("fnv64:{:016x}", neoethos_core::utils::fnv1a64(&bytes))
}

pub fn exact_symbol_contract_from_proto_oa(
    resolved: &CTraderResolvedSymbol,
    assets: &[CTraderAssetInfo],
    trader: &CTraderTraderSnapshot,
    raw_response_json: &str,
    raw_symbols_response_json: &str,
    raw_assets_response_json: &str,
    raw_trader_response_json: &str,
    captured_at_ms: i64,
) -> Result<ExactProtoOaSymbolContract> {
    let raw_symbols = parse_symbols_list_response(raw_symbols_response_json)?;
    if raw_symbols.account_id != resolved.account_id
        || raw_symbols
            .symbols
            .iter()
            .find(|candidate| candidate.symbol_id == resolved.light_symbol.symbol_id)
            != Some(&resolved.light_symbol)
    {
        bail!("light symbol differs from raw ProtoOA response");
    }
    if parse_asset_list_response(raw_assets_response_json)? != assets {
        bail!("asset catalog differs from raw ProtoOA response");
    }
    let raw_trader = parse_trader_response(raw_trader_response_json)?;
    if raw_trader.account_id != trader.account_id
        || raw_trader.deposit_asset_id != trader.deposit_asset_id
        || raw_trader.money_digits != trader.money_digits
        || trader.account_id != resolved.account_id
    {
        bail!("trader account differs from raw ProtoOA response or resolved symbol account");
    }
    let light = &resolved.light_symbol;
    let symbol = &resolved.symbol;
    let raw_symbol = parse_symbol_by_id_response(raw_response_json)?
        .into_iter()
        .find(|candidate| candidate.symbol_id == symbol.symbol_id)
        .context("raw ProtoOA response does not contain resolved symbol")?;
    if raw_symbol != *symbol {
        bail!("resolved symbol differs from raw ProtoOA response");
    }
    if light.symbol_id != symbol.symbol_id
        || light.symbol_name != symbol.symbol_name
        || !light.enabled
        || symbol.is_archived
        || !symbol.is_trading_enabled
    {
        bail!("ProtoOA light/full symbol identity or trading state mismatch");
    }
    let base_asset_id = light
        .base_asset_id
        .context("ProtoOA light symbol has no baseAssetId")?;
    let quote_asset_id = light
        .quote_asset_id
        .context("ProtoOA light symbol has no quoteAssetId")?;
    let base_asset = asset_name(assets, base_asset_id, "base")?;
    let quote_asset = asset_name(assets, quote_asset_id, "quote")?;
    let account_asset_id = trader
        .deposit_asset_id
        .context("ProtoOA trader has no depositAssetId")?;
    let account_currency = asset_name(assets, account_asset_id, "account")?;
    let financials = symbol
        .financials
        .as_ref()
        .context("full ProtoOA symbol financial contract is missing")?;
    if !matches!(financials.trading_mode, Some(TradingModeProto::Enabled)) {
        bail!("ProtoOA symbol is not enabled for opening positions");
    }
    if financials.rollover_commission.unwrap_or(0) != 0 {
        bail!("unsupported non-zero ProtoOA rollover commission");
    }

    let commission_type = match financials
        .commission_type
        .context("ProtoOA commissionType is missing")?
    {
        CommissionType::UsdPerMillionUsd => CommissionTypeExact::UsdPerMillionUsd,
        CommissionType::UsdPerLot => CommissionTypeExact::UsdPerLot,
        CommissionType::PercentageOfValue => CommissionTypeExact::PercentageOfValue,
        CommissionType::QuoteCcyPerLot => CommissionTypeExact::QuoteCurrencyPerLot,
    };
    let commission_rate = financials
        .commission_rate_decimal()
        .context("ProtoOA preciseTradingCommissionRate is missing")?;
    let minimum_per_deal = financials
        .precise_min_commission
        .context("ProtoOA preciseMinCommission is missing")? as f64
        / 1e8;
    let minimum_currency = match financials
        .min_commission_type
        .context("ProtoOA minCommissionType is missing")?
    {
        MinCommissionType::Currency => MinimumCommissionCurrency::Account,
        MinCommissionType::QuoteCurrency => MinimumCommissionCurrency::Quote,
    };
    let minimum_asset = financials
        .min_commission_asset
        .as_deref()
        .context("ProtoOA minCommissionAsset is missing")?
        .trim()
        .to_ascii_uppercase();
    let expected_minimum_asset = match minimum_currency {
        MinimumCommissionCurrency::Account => account_currency,
        MinimumCommissionCurrency::Quote => quote_asset,
    };
    if !minimum_asset.eq_ignore_ascii_case(expected_minimum_asset) {
        bail!(
            "ProtoOA minimum commission asset {minimum_asset} does not match {expected_minimum_asset}"
        );
    }
    let swap_calculation = match financials
        .swap_calculation_type
        .unwrap_or(SwapCalculationType::Pips)
    {
        SwapCalculationType::Pips => SwapCalculationExact::Pips,
        SwapCalculationType::Percentage => SwapCalculationExact::Percentage,
        SwapCalculationType::Points => SwapCalculationExact::Points,
    };
    let swap = ExactSwapSchedule {
        calculation: swap_calculation,
        long: financials
            .swap_long
            .context("ProtoOA swapLong is missing")?,
        short: financials
            .swap_short
            .context("ProtoOA swapShort is missing")?,
        period_hours: financials
            .swap_period_hours
            .context("ProtoOA swapPeriod is missing")?,
        time_minutes_from_utc_midnight: financials
            .swap_time_minutes_from_utc_midnight
            .context("ProtoOA swapTime is missing")?,
        triple_day: financials.swap_rollover_3_days.map(day_number),
        skip_periods: financials.skip_swap_periods.unwrap_or(0),
        charge_at_weekends: financials.charge_swap_at_weekends.unwrap_or(false),
    };
    let pnl_conversion_fee_rate = financials
        .pnl_conversion_fee_rate
        .context("ProtoOA pnlConversionFeeRate is missing")?
        as f64
        / 10_000.0;
    let trading_intervals = financials
        .trading_intervals
        .iter()
        .map(|interval| ExactTradingInterval {
            start_second_from_sunday: interval.start_second_from_sunday,
            end_second_from_sunday: interval.end_second_from_sunday,
        })
        .collect();
    let holidays = financials
        .holidays
        .iter()
        .map(|holiday| ExactHolidayWindow {
            holiday_id: holiday.holiday_id,
            name: holiday.name.clone(),
            schedule_time_zone: holiday.schedule_time_zone.clone(),
            days_since_epoch: holiday.days_since_epoch,
            is_recurring: holiday.is_recurring,
            start_second_from_midnight: holiday.start_second_from_midnight,
            end_second_from_midnight: holiday.end_second_from_midnight,
        })
        .collect();

    ExactProtoOaSymbolContract::new(
        resolved.account_id,
        account_asset_id,
        account_currency,
        symbol.symbol_id,
        symbol.symbol_name.clone(),
        base_asset_id,
        base_asset,
        quote_asset_id,
        quote_asset,
        u32::try_from(symbol.digits).context("negative ProtoOA digits")?,
        u32::try_from(symbol.pip_position).context("negative ProtoOA pipPosition")?,
        symbol.lot_size.context("ProtoOA lotSize is missing")?,
        symbol.min_volume.context("ProtoOA minVolume is missing")?,
        symbol.max_volume.context("ProtoOA maxVolume is missing")?,
        symbol
            .step_volume
            .context("ProtoOA stepVolume is missing")?,
        ExactCommissionSchedule {
            commission_type,
            rate: commission_rate,
            minimum_per_deal,
            minimum_currency,
            minimum_asset,
        },
        swap,
        pnl_conversion_fee_rate,
        financials.enable_short_selling.unwrap_or(false),
        financials
            .schedule_time_zone
            .clone()
            .context("ProtoOA scheduleTimeZone is missing")?,
        trading_intervals,
        holidays,
        EvidenceProvenance::new(
            "ctrader_proto_oa_symbol_contract_bundle",
            captured_at_ms,
            source_payload_hash(&[
                raw_response_json,
                raw_symbols_response_json,
                raw_assets_response_json,
                raw_trader_response_json,
            ]),
        )?,
    )
    .map_err(anyhow::Error::new)
}

pub fn synchronize_historical_bid_ask(
    symbol: &ExactProtoOaSymbolContract,
    proto_symbol: &CTraderSymbolInfo,
    bid: &HistoricalTicksResult,
    ask: &HistoricalTicksResult,
    bid_response_json: &str,
    ask_response_json: &str,
    captured_at_ms: i64,
) -> Result<SynchronizedBidAsk> {
    if proto_symbol.symbol_id != symbol.symbol_id
        || proto_symbol.symbol_name != symbol.symbol
        || u32::try_from(proto_symbol.digits).ok() != Some(symbol.digits)
    {
        bail!("ProtoOA symbol used to decode BID/ASK does not match exact symbol contract");
    }
    let parsed_bid = parse_tick_data_response(bid_response_json, proto_symbol)?;
    let parsed_ask = parse_tick_data_response(ask_response_json, proto_symbol)?;
    if parsed_bid != *bid || parsed_ask != *ask {
        bail!("historical BID/ASK values do not match their raw ProtoOA responses");
    }
    if bid.symbol_id != symbol.symbol_id || ask.symbol_id != symbol.symbol_id {
        bail!("historical BID/ASK symbol id does not match exact symbol contract");
    }
    if bid.has_more || ask.has_more {
        bail!("historical BID/ASK response is incomplete (hasMore=true)");
    }
    if bid.ticks.len() != ask.ticks.len() || bid.ticks.is_empty() {
        bail!("historical BID/ASK sides do not have complete one-to-one coverage");
    }
    let mut quotes = Vec::with_capacity(bid.ticks.len());
    for (bid_tick, ask_tick) in bid.ticks.iter().zip(&ask.ticks) {
        if bid_tick.timestamp_ms != ask_tick.timestamp_ms {
            bail!(
                "historical BID/ASK counterpart missing at bid={} ask={}",
                bid_tick.timestamp_ms,
                ask_tick.timestamp_ms
            );
        }
        quotes.push(SynchronizedQuote {
            timestamp_ms: bid_tick.timestamp_ms,
            bid: bid_tick.price,
            ask: ask_tick.price,
        });
    }
    SynchronizedBidAsk::new(
        symbol.symbol_id,
        symbol.symbol.clone(),
        quotes,
        source_payload_hash(&["BID", bid_response_json]),
        source_payload_hash(&["ASK", ask_response_json]),
        EvidenceProvenance::new(
            "ctrader_proto_oa_get_tick_data_bid_ask",
            captured_at_ms,
            source_payload_hash(&["BID", bid_response_json, "ASK", ask_response_json]),
        )?,
    )
    .map_err(anyhow::Error::new)
}

pub fn broker_position_financial_snapshot(
    reconcile: &CTraderReconcileSnapshot,
    broker_pnl: &AuthoritativeUnrealizedPnL,
    contracts: &[ExactProtoOaSymbolContract],
    account_currency: &str,
    reconcile_response_json: &str,
    broker_pnl_response_json: &str,
    captured_at_ms: i64,
    max_age_ms: i64,
) -> Result<BrokerPositionFinancialSnapshot> {
    if parse_reconcile_response(reconcile_response_json)? != *reconcile {
        bail!("reconcile snapshot does not match raw ProtoOA response");
    }
    let parsed_pnl = parse_get_position_unrealized_pnl_response(broker_pnl_response_json)?;
    if parsed_pnl.account_id != broker_pnl.account_id
        || parsed_pnl.money_digits != broker_pnl.money_digits
        || parsed_pnl.positions.len() != broker_pnl.by_position.len()
        || parsed_pnl.positions.iter().any(|row| {
            broker_pnl
                .by_position
                .get(&row.position_id)
                .is_none_or(|actual| {
                    actual.money_digits != parsed_pnl.money_digits
                        || actual.gross_unrealized_pnl.to_bits()
                            != row.gross_unrealized_pnl.to_bits()
                        || actual.net_unrealized_pnl.to_bits() != row.net_unrealized_pnl.to_bits()
                })
        })
    {
        bail!("broker PnL snapshot does not match raw ProtoOA response");
    }
    if reconcile.account_id != broker_pnl.account_id {
        bail!("reconcile and broker PnL account ids differ");
    }
    if contracts.is_empty()
        || contracts.iter().any(|contract| {
            contract.validate().is_err()
                || contract.account_id != reconcile.account_id
                || !contract
                    .account_currency
                    .eq_ignore_ascii_case(account_currency)
        })
    {
        bail!("live account identity lacks matching exact ProtoOA symbol contracts");
    }
    let contract_count = contracts.len();
    let contracts: BTreeMap<_, _> = contracts
        .iter()
        .map(|contract| (contract.symbol_id, contract))
        .collect();
    if contracts.len() != contract_count {
        bail!("duplicate exact ProtoOA symbol contract id");
    }
    let reconcile_ids = reconcile
        .positions
        .iter()
        .map(|position| position.position_id)
        .collect::<std::collections::BTreeSet<_>>();
    let pnl_ids = broker_pnl
        .by_position
        .keys()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if reconcile_ids != pnl_ids {
        bail!("broker PnL and reconcile position sets differ");
    }
    let mut rows = Vec::with_capacity(reconcile.positions.len());
    for position in &reconcile.positions {
        let contract = contracts
            .get(&position.symbol_id)
            .context("position has no exact ProtoOA symbol contract")?;
        if contract.account_id != reconcile.account_id {
            bail!("position symbol contract belongs to another account");
        }
        let broker = broker_pnl
            .by_position
            .get(&position.position_id)
            .context("position has no broker unrealized PnL row")?;
        rows.push(BrokerPositionFinancialRow {
            position_id: position.position_id,
            symbol_id: position.symbol_id,
            direction: parse_direction(&position.trade_side)?,
            volume_units: position.volume,
            entry_price: position.price.context("position entry price is missing")?,
            gross_unrealized_pnl_account: broker.gross_unrealized_pnl,
            net_unrealized_pnl_account: broker.net_unrealized_pnl,
            swap_account: position.swap,
            commission_account: position.commission,
        });
    }
    BrokerPositionFinancialSnapshot::new(
        reconcile.account_id,
        account_currency,
        captured_at_ms,
        max_age_ms,
        rows,
        EvidenceProvenance::new(
            "ctrader_reconcile_plus_position_unrealized_pnl",
            captured_at_ms,
            source_payload_hash(&[reconcile_response_json, broker_pnl_response_json]),
        )?,
    )
    .map_err(anyhow::Error::new)
}

pub fn synchronized_conversion_leg(
    contract: &ExactProtoOaSymbolContract,
    bid_ask: SynchronizedBidAsk,
) -> Result<SynchronizedConversionLeg> {
    SynchronizedConversionLeg::new(contract, bid_ask).map_err(anyhow::Error::new)
}

pub fn close_deal_reconciliation_from_ctrader(
    expected: ExpectedLocalClose,
    deals: &[CTraderDealSnapshot],
    raw_response_json: &str,
    captured_at_ms: i64,
) -> Result<CloseDealReconciliation> {
    let parsed_deals = parse_deal_list_response(raw_response_json)
        .or_else(|_| parse_deal_list_by_position_id_response(raw_response_json))?;
    if parsed_deals != deals {
        bail!("close-deal snapshots do not match raw ProtoOA response");
    }
    let mut broker_deals = Vec::new();
    for deal in deals
        .iter()
        .filter(|deal| deal.position_id == expected.position_id)
    {
        if !deal.deal_status.eq_ignore_ascii_case("filled") {
            bail!("close deal {} is not FILLED", deal.deal_id);
        }
        broker_deals.push(BrokerCloseDeal {
            deal_id: deal.deal_id,
            order_id: deal.order_id,
            position_id: deal.position_id,
            symbol_id: deal.symbol_id,
            direction: parse_direction(&deal.trade_side)?,
            filled_volume_units: deal.filled_volume,
            execution_timestamp_ms: deal.execution_timestamp_ms,
            gross_profit_account: deal
                .gross_profit
                .context("broker close deal grossProfit is missing")?,
            commission_account: deal
                .fee
                .context("broker close deal commission is missing")?,
            swap_account: deal.swap.context("broker close deal swap is missing")?,
            conversion_fee_account: deal
                .pnl_conversion_fee
                .context("broker close deal pnlConversionFee is missing")?,
            net_profit_account: deal
                .net_profit
                .context("broker close deal netProfit is missing")?,
        });
    }
    reconcile_close_deals(
        expected,
        broker_deals,
        EvidenceProvenance::new(
            "ctrader_proto_oa_deal_list",
            captured_at_ms,
            source_payload_hash(&[raw_response_json]),
        )?,
    )
    .map_err(anyhow::Error::new)
}

pub fn historical_truth_from_ctrader(
    symbol_contract: ExactProtoOaSymbolContract,
    bid_ask: SynchronizedBidAsk,
    conversions: neoethos_core::ConversionBook,
    initial_equity: f64,
    risk_config_hash: &str,
    strategy_hash: &str,
    slippage_policy_hash: &str,
) -> Result<HistoricalBrokerTruthEvidence> {
    HistoricalBrokerTruthEvidence::new(
        symbol_contract,
        bid_ask,
        conversions,
        initial_equity,
        risk_config_hash,
        strategy_hash,
        slippage_policy_hash,
    )
    .map_err(anyhow::Error::new)
}

fn asset_name<'a>(assets: &'a [CTraderAssetInfo], id: i64, side: &str) -> Result<&'a str> {
    assets
        .iter()
        .find(|asset| asset.asset_id == id)
        .map(|asset| asset.name.as_str())
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| anyhow!("ProtoOA {side} asset id {id} is not present in asset catalog"))
}

fn day_number(value: DayOfWeek) -> u8 {
    value as u8
}

fn parse_direction(value: &str) -> Result<PositionDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "buy" | "long" => Ok(PositionDirection::Long),
        "sell" | "short" => Ok(PositionDirection::Short),
        other => Err(anyhow!("unknown cTrader trade side {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_services::ctrader_account::{
        CTraderPendingOrderSnapshot, CTraderPositionSnapshot,
    };
    use crate::app_services::ctrader_data::CTraderLightSymbolInfo;
    use crate::app_services::pnl::BrokerPositionPnL;
    use std::collections::HashMap;

    fn fixture_resolved() -> CTraderResolvedSymbol {
        let raw = include_str!("../../tests/fixtures/ctrader_symbol_EURUSD.raw.json");
        let symbol = parse_symbol_by_id_response(raw).unwrap().remove(0);
        CTraderResolvedSymbol {
            account_id: 47_367_144,
            light_symbol: CTraderLightSymbolInfo {
                symbol_id: 1,
                symbol_name: "EURUSD".to_string(),
                enabled: true,
                description: None,
                symbol_category_id: None,
                base_asset_id: Some(4),
                quote_asset_id: Some(8),
            },
            symbol,
        }
    }

    fn fixture_symbols_raw() -> String {
        serde_json::json!({
            "payloadType": 2115,
            "payload": {
                "ctidTraderAccountId": 47_367_144,
                "symbol": [{
                    "symbolId": 1,
                    "symbolName": "EURUSD",
                    "enabled": true,
                    "baseAssetId": 4,
                    "quoteAssetId": 8,
                }]
            }
        })
        .to_string()
    }

    fn fixture_assets() -> (Vec<CTraderAssetInfo>, String) {
        let raw = serde_json::json!({
            "payloadType": 2113,
            "payload": {
                "asset": [
                    { "assetId": 4, "name": "EUR", "digits": 2 },
                    { "assetId": 8, "name": "USD", "digits": 2 }
                ]
            }
        })
        .to_string();
        (parse_asset_list_response(&raw).unwrap(), raw)
    }

    fn fixture_trader() -> (CTraderTraderSnapshot, String) {
        let raw = serde_json::json!({
            "payloadType": 2122,
            "payload": {
                "ctidTraderAccountId": 47_367_144,
                "trader": {
                    "balance": 1_000_000,
                    "moneyDigits": 2,
                    "depositAssetId": 8,
                }
            }
        })
        .to_string();
        (parse_trader_response(&raw).unwrap(), raw)
    }

    fn fixture_contract() -> ExactProtoOaSymbolContract {
        let raw = include_str!("../../tests/fixtures/ctrader_symbol_EURUSD.raw.json");
        let resolved = fixture_resolved();
        let symbols_raw = fixture_symbols_raw();
        let (assets, assets_raw) = fixture_assets();
        let (trader, trader_raw) = fixture_trader();
        exact_symbol_contract_from_proto_oa(
            &resolved,
            &assets,
            &trader,
            raw,
            &symbols_raw,
            &assets_raw,
            &trader_raw,
            1_700_000_000_000,
        )
        .unwrap()
    }

    fn tick_response(result: &HistoricalTicksResult) -> String {
        let mut previous = None;
        let tick_data = result
            .ticks
            .iter()
            .rev()
            .map(|tick| {
                let timestamp = previous
                    .map(|previous_timestamp| previous_timestamp - tick.timestamp_ms)
                    .unwrap_or(tick.timestamp_ms);
                previous = Some(tick.timestamp_ms);
                serde_json::json!({
                    "timestamp": timestamp,
                    "tick": (tick.price * 100_000.0).round() as i64,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "payloadType": 2146,
            "payload": {
                "symbolId": result.symbol_id,
                "hasMore": result.has_more,
                "tickData": tick_data,
            }
        })
        .to_string()
    }

    fn deal_list_response(deals: &[CTraderDealSnapshot]) -> String {
        let deals = deals
            .iter()
            .map(|deal| {
                serde_json::json!({
                    "dealId": deal.deal_id,
                    "orderId": deal.order_id,
                    "positionId": deal.position_id,
                    "volume": (deal.volume * 100.0).round() as i64,
                    "filledVolume": (deal.filled_volume * 100.0).round() as i64,
                    "symbolId": deal.symbol_id,
                    "executionTimestamp": deal.execution_timestamp_ms,
                    "executionPrice": deal.execution_price,
                    "tradeSide": if deal.trade_side.eq_ignore_ascii_case("BUY") { 1 } else { 2 },
                    "dealStatus": 2,
                    "closePositionDetail": {
                        "entryPrice": deal.entry_price,
                        "grossProfit": (deal.gross_profit.unwrap() * 10.0).round() as i64,
                        "swap": (deal.swap.unwrap() * 10.0).round() as i64,
                        "commission": (deal.fee.unwrap() * 10.0).round() as i64,
                        "pnlConversionFee":
                            (deal.pnl_conversion_fee.unwrap() * 10.0).round() as i64,
                        "moneyDigits": 1,
                    }
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "payloadType": 2134,
            "payload": { "deal": deals }
        })
        .to_string()
    }

    #[test]
    fn real_proto_oa_fixture_maps_exact_contract_and_units() {
        let contract = fixture_contract();
        assert_eq!(contract.symbol, "EURUSD");
        assert_eq!(contract.account_currency, "USD");
        assert_eq!(contract.base_asset, "EUR");
        assert_eq!(contract.quote_asset, "USD");
        assert_eq!(contract.digits, 5);
        assert_eq!(contract.pip_size, 0.0001);
        assert_eq!(contract.contract_units_per_lot, 100_000.0);
        assert_eq!(contract.min_volume_cents, 100_000);
        assert_eq!(contract.max_volume_cents, 1_000_000_000);
        assert_eq!(contract.step_volume_cents, 100_000);
        assert_eq!(contract.commission.rate, 45.0);
        assert_eq!(contract.swap.long, -2.445);
        assert_eq!(contract.swap.short, -0.105);
        assert_eq!(contract.swap.triple_day, Some(3));
        assert_eq!(
            contract.provenance.producer,
            "ctrader_proto_oa_symbol_contract_bundle"
        );
    }

    #[test]
    fn incomplete_or_unsupported_proto_contract_fails_closed() {
        let raw = include_str!("../../tests/fixtures/ctrader_symbol_EURUSD.raw.json");
        let mut resolved = fixture_resolved();
        resolved.symbol.financials = None;
        let symbols_raw = fixture_symbols_raw();
        let (assets, assets_raw) = fixture_assets();
        let (trader, trader_raw) = fixture_trader();
        assert!(
            exact_symbol_contract_from_proto_oa(
                &resolved,
                &assets,
                &trader,
                raw,
                &symbols_raw,
                &assets_raw,
                &trader_raw,
                1_700_000_000_000,
            )
            .is_err()
        );

        let mut no_short_flag = fixture_resolved();
        no_short_flag
            .symbol
            .financials
            .as_mut()
            .unwrap()
            .enable_short_selling = None;
        let mut raw_value: serde_json::Value = serde_json::from_str(raw).unwrap();
        raw_value["payload"]["symbol"][0]
            .as_object_mut()
            .unwrap()
            .remove("enableShortSelling");
        let raw_no_short_flag = raw_value.to_string();
        let contract = exact_symbol_contract_from_proto_oa(
            &no_short_flag,
            &fixture_assets().0,
            &trader,
            &raw_no_short_flag,
            &symbols_raw,
            &assets_raw,
            &trader_raw,
            1_700_000_000_000,
        )
        .unwrap();
        assert!(!contract.short_selling_enabled);
        let mut forged_assets = assets;
        forged_assets[0].name = "GBP".to_string();
        assert!(
            exact_symbol_contract_from_proto_oa(
                &fixture_resolved(),
                &forged_assets,
                &trader,
                raw,
                &symbols_raw,
                &assets_raw,
                &trader_raw,
                1_700_000_000_000,
            )
            .is_err()
        );
    }

    #[test]
    fn bid_ask_synchronization_is_exact_and_fail_closed() {
        let contract = fixture_contract();
        let proto_symbol = fixture_resolved().symbol;
        let bid = HistoricalTicksResult {
            symbol_id: 1,
            ticks: vec![
                super::super::ctrader_data::HistoricalTick {
                    timestamp_ms: 1_000,
                    price: 1.1,
                },
                super::super::ctrader_data::HistoricalTick {
                    timestamp_ms: 2_000,
                    price: 1.2,
                },
            ],
            has_more: false,
        };
        let mut ask = HistoricalTicksResult {
            symbol_id: 1,
            ticks: vec![
                super::super::ctrader_data::HistoricalTick {
                    timestamp_ms: 1_000,
                    price: 1.1002,
                },
                super::super::ctrader_data::HistoricalTick {
                    timestamp_ms: 2_000,
                    price: 1.2002,
                },
            ],
            has_more: false,
        };
        let bid_raw = tick_response(&bid);
        let ask_raw = tick_response(&ask);
        let synced = synchronize_historical_bid_ask(
            &contract,
            &proto_symbol,
            &bid,
            &ask,
            &bid_raw,
            &ask_raw,
            3_000,
        )
        .unwrap();
        assert_eq!(synced.quotes.len(), 2);
        ask.ticks[1].timestamp_ms = 2_001;
        assert!(
            synchronize_historical_bid_ask(
                &contract,
                &proto_symbol,
                &bid,
                &ask,
                &bid_raw,
                &ask_raw,
                3_000
            )
            .is_err()
        );
        ask.ticks.pop();
        assert!(
            synchronize_historical_bid_ask(
                &contract,
                &proto_symbol,
                &bid,
                &ask,
                &bid_raw,
                &ask_raw,
                3_000
            )
            .is_err()
        );
    }

    #[test]
    fn broker_position_snapshot_requires_exact_reconcile_and_pnl_identity() {
        let contract = fixture_contract();
        let position = CTraderPositionSnapshot {
            position_id: 9,
            symbol_id: 1,
            trade_side: "BUY".to_string(),
            volume: 1_000.0,
            open_timestamp_ms: Some(1_000),
            price: Some(1.1),
            stop_loss: None,
            take_profit: None,
            swap: Some(-0.2),
            commission: Some(-0.4),
            mirroring_commission: None,
            used_margin: None,
            label: None,
            comment: None,
            client_order_id: None,
        };
        let reconcile = CTraderReconcileSnapshot {
            account_id: 47_367_144,
            positions: vec![position],
            pending_orders: Vec::<CTraderPendingOrderSnapshot>::new(),
        };
        let mut by_position = HashMap::new();
        by_position.insert(
            9,
            BrokerPositionPnL {
                position_id: 9,
                gross_unrealized_pnl: 12.0,
                net_unrealized_pnl: 11.4,
                money_digits: 2,
            },
        );
        let pnl = AuthoritativeUnrealizedPnL {
            account_id: 47_367_144,
            money_digits: 2,
            by_position,
        };
        let reconcile_raw = serde_json::json!({
            "payloadType": 2125,
            "payload": {
                "ctidTraderAccountId": 47_367_144,
                "position": [{
                    "positionId": 9,
                    "tradeData": {
                        "symbolId": 1,
                        "volume": 100_000,
                        "tradeSide": 1,
                        "openTimestamp": 1_000,
                    },
                    "price": 1.1,
                    "swap": -20,
                    "commission": -40,
                    "moneyDigits": 2,
                }]
            }
        })
        .to_string();
        let pnl_raw = serde_json::json!({
            "payloadType": 2188,
            "payload": {
                "ctidTraderAccountId": 47_367_144,
                "moneyDigits": 2,
                "positionUnrealizedPnL": [{
                    "positionId": 9,
                    "grossUnrealizedPnL": 1_200,
                    "netUnrealizedPnL": 1_140,
                }]
            }
        })
        .to_string();
        let snapshot = broker_position_financial_snapshot(
            &reconcile,
            &pnl,
            &[contract],
            "USD",
            &reconcile_raw,
            &pnl_raw,
            1_700_000_000_000,
            30_000,
        )
        .unwrap();
        assert_eq!(snapshot.positions[0].net_unrealized_pnl_account, 11.4);
        let missing_pnl = AuthoritativeUnrealizedPnL {
            account_id: 47_367_144,
            money_digits: 2,
            by_position: HashMap::new(),
        };
        assert!(
            broker_position_financial_snapshot(
                &reconcile,
                &missing_pnl,
                &[],
                "USD",
                &reconcile_raw,
                &pnl_raw,
                1_700_000_000_000,
                30_000,
            )
            .is_err()
        );
    }

    #[test]
    fn close_deal_adapter_preserves_broker_money_and_partial_state() {
        let expected = ExpectedLocalClose {
            account_id: 47_367_144,
            symbol_id: 1,
            position_id: 9,
            direction: PositionDirection::Long,
            expected_volume_units: 1_000.0,
            local_estimated_pnl_account: Some(999.0),
        };
        let deal = CTraderDealSnapshot {
            deal_id: 10,
            order_id: 11,
            position_id: 9,
            symbol_id: 1,
            trade_side: "SELL".to_string(),
            deal_status: "FILLED".to_string(),
            volume: 400.0,
            filled_volume: 400.0,
            execution_timestamp_ms: 2_000,
            execution_price: Some(1.2),
            entry_price: Some(1.1),
            gross_profit: Some(10.0),
            fee: Some(-1.0),
            swap: Some(-0.5),
            pnl_conversion_fee: Some(-0.1),
            net_profit: Some(8.4),
        };
        let partial_deals = vec![deal.clone(), deal.clone()];
        let partial_raw = deal_list_response(&partial_deals);
        let partial = close_deal_reconciliation_from_ctrader(
            expected.clone(),
            &partial_deals,
            &partial_raw,
            3_000,
        )
        .unwrap();
        assert_eq!(partial.state, neoethos_core::ReconciliationState::Partial);
        assert_eq!(partial.unique_deals.len(), 1);
        assert_eq!(partial.broker_net_profit_account, None);
        let first = deal.clone();
        let mut rest = deal;
        rest.deal_id = 12;
        rest.order_id = 13;
        rest.filled_volume = 600.0;
        rest.volume = 600.0;
        rest.gross_profit = Some(5.6);
        rest.net_profit = Some(4.0);
        let complete_deals = vec![first, rest];
        let complete_raw = deal_list_response(&complete_deals);
        let complete =
            close_deal_reconciliation_from_ctrader(expected, &complete_deals, &complete_raw, 3_000)
                .unwrap();
        assert_eq!(
            complete.state,
            neoethos_core::ReconciliationState::Reconciled
        );
        assert_eq!(complete.broker_net_profit_account, Some(12.4));
    }
}
