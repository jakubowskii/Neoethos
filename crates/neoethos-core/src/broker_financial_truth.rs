use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use chrono::{Datelike, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::storage::json::{read_json, stable_json_hash, write_json_atomic};

pub const BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION: u16 = 1;
pub const BROKER_FINANCIAL_TRUTH_UNAVAILABLE_V1: &str = "BROKER_FINANCIAL_TRUTH_UNAVAILABLE_V1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinancialTruthMode {
    Mechanical,
    BrokerExactHistorical,
    LiveBroker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerFinancialCapabilityKind {
    SynchronizedHistoricalBidAsk,
    SynchronizedConversionLegs,
    ExactProtoOaSymbolContract,
    BrokerPositionUnrealizedPnl,
    CloseDealReconciliation,
}

impl BrokerFinancialCapabilityKind {
    pub const ALL: [Self; 5] = [
        Self::SynchronizedHistoricalBidAsk,
        Self::SynchronizedConversionLegs,
        Self::ExactProtoOaSymbolContract,
        Self::BrokerPositionUnrealizedPnl,
        Self::CloseDealReconciliation,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SynchronizedHistoricalBidAsk => "synchronized_historical_bid_ask",
            Self::SynchronizedConversionLegs => "synchronized_conversion_legs",
            Self::ExactProtoOaSymbolContract => "exact_proto_oa_symbol_contract",
            Self::BrokerPositionUnrealizedPnl => "broker_position_unrealized_pnl",
            Self::CloseDealReconciliation => "close_deal_reconciliation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerFinancialCapabilityState {
    Present,
    Missing,
    Invalid,
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerFinancialCapability {
    pub kind: BrokerFinancialCapabilityKind,
    pub state: BrokerFinancialCapabilityState,
    pub reason: String,
    pub provenance_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceProvenance {
    pub producer: String,
    pub captured_at_ms: i64,
    pub source_hash: String,
}

impl EvidenceProvenance {
    pub fn new(
        producer: impl Into<String>,
        captured_at_ms: i64,
        source_hash: impl Into<String>,
    ) -> Result<Self, BrokerFinancialTruthError> {
        let value = Self {
            producer: producer.into(),
            captured_at_ms,
            source_hash: source_hash.into(),
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        if self.producer.trim().is_empty()
            || self.captured_at_ms <= 0
            || !valid_hash(&self.source_hash)
        {
            return Err(invalid("evidence provenance is incomplete"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommissionTypeExact {
    UsdPerMillionUsd,
    UsdPerLot,
    PercentageOfValue,
    QuoteCurrencyPerLot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinimumCommissionCurrency {
    Account,
    Quote,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExactCommissionSchedule {
    pub commission_type: CommissionTypeExact,
    pub rate: f64,
    pub minimum_per_deal: f64,
    pub minimum_currency: MinimumCommissionCurrency,
    pub minimum_asset: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwapCalculationExact {
    Pips,
    Percentage,
    Points,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExactSwapSchedule {
    pub calculation: SwapCalculationExact,
    pub long: f64,
    pub short: f64,
    pub period_hours: i32,
    pub time_minutes_from_utc_midnight: i32,
    pub triple_day: Option<u8>,
    pub skip_periods: i32,
    pub charge_at_weekends: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactTradingInterval {
    pub start_second_from_sunday: u32,
    pub end_second_from_sunday: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactHolidayWindow {
    pub holiday_id: i64,
    pub name: String,
    pub schedule_time_zone: String,
    pub days_since_epoch: i64,
    pub is_recurring: bool,
    pub start_second_from_midnight: Option<i32>,
    pub end_second_from_midnight: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExactProtoOaSymbolContract {
    pub schema_version: u16,
    pub account_id: i64,
    pub account_asset_id: i64,
    pub account_currency: String,
    pub symbol_id: i64,
    pub symbol: String,
    pub base_asset_id: i64,
    pub base_asset: String,
    pub quote_asset_id: i64,
    pub quote_asset: String,
    pub digits: u32,
    pub pip_position: u32,
    pub pip_size: f64,
    pub lot_size_cents: i64,
    pub contract_units_per_lot: f64,
    pub min_volume_cents: i64,
    pub max_volume_cents: i64,
    pub step_volume_cents: i64,
    pub commission: ExactCommissionSchedule,
    pub swap: ExactSwapSchedule,
    pub pnl_conversion_fee_rate: f64,
    pub short_selling_enabled: bool,
    pub schedule_time_zone: String,
    pub trading_intervals: Vec<ExactTradingInterval>,
    pub holidays: Vec<ExactHolidayWindow>,
    pub provenance: EvidenceProvenance,
    pub content_hash: String,
}

#[derive(Serialize)]
struct SymbolHash<'a> {
    schema_version: u16,
    account_id: i64,
    account_asset_id: i64,
    account_currency: &'a str,
    symbol_id: i64,
    symbol: &'a str,
    base_asset_id: i64,
    base_asset: &'a str,
    quote_asset_id: i64,
    quote_asset: &'a str,
    digits: u32,
    pip_position: u32,
    pip_size: f64,
    lot_size_cents: i64,
    contract_units_per_lot: f64,
    min_volume_cents: i64,
    max_volume_cents: i64,
    step_volume_cents: i64,
    commission: &'a ExactCommissionSchedule,
    swap: &'a ExactSwapSchedule,
    pnl_conversion_fee_rate: f64,
    short_selling_enabled: bool,
    schedule_time_zone: &'a str,
    trading_intervals: &'a [ExactTradingInterval],
    holidays: &'a [ExactHolidayWindow],
    provenance: &'a EvidenceProvenance,
}

impl ExactProtoOaSymbolContract {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        account_id: i64,
        account_asset_id: i64,
        account_currency: impl Into<String>,
        symbol_id: i64,
        symbol: impl Into<String>,
        base_asset_id: i64,
        base_asset: impl Into<String>,
        quote_asset_id: i64,
        quote_asset: impl Into<String>,
        digits: u32,
        pip_position: u32,
        lot_size_cents: i64,
        min_volume_cents: i64,
        max_volume_cents: i64,
        step_volume_cents: i64,
        commission: ExactCommissionSchedule,
        swap: ExactSwapSchedule,
        pnl_conversion_fee_rate: f64,
        short_selling_enabled: bool,
        schedule_time_zone: impl Into<String>,
        trading_intervals: Vec<ExactTradingInterval>,
        holidays: Vec<ExactHolidayWindow>,
        provenance: EvidenceProvenance,
    ) -> Result<Self, BrokerFinancialTruthError> {
        let pip_size = 10_f64.powi(-(pip_position as i32));
        let mut value = Self {
            schema_version: BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION,
            account_id,
            account_asset_id,
            account_currency: canonical_asset(account_currency.into())?,
            symbol_id,
            symbol: symbol.into(),
            base_asset_id,
            base_asset: canonical_asset(base_asset.into())?,
            quote_asset_id,
            quote_asset: canonical_asset(quote_asset.into())?,
            digits,
            pip_position,
            pip_size,
            lot_size_cents,
            contract_units_per_lot: lot_size_cents as f64 / 100.0,
            min_volume_cents,
            max_volume_cents,
            step_volume_cents,
            commission,
            swap,
            pnl_conversion_fee_rate,
            short_selling_enabled,
            schedule_time_zone: schedule_time_zone.into(),
            trading_intervals,
            holidays,
            provenance,
            content_hash: String::new(),
        };
        value.validate_payload()?;
        value.content_hash = value.recomputed_hash()?;
        Ok(value)
    }

    pub fn wire_volume_to_lots(
        &self,
        wire_volume_cents: i64,
    ) -> Result<f64, BrokerFinancialTruthError> {
        if wire_volume_cents < self.min_volume_cents
            || wire_volume_cents > self.max_volume_cents
            || wire_volume_cents % self.step_volume_cents != 0
        {
            return Err(invalid("wire volume violates broker min/max/step"));
        }
        let lots = wire_volume_cents as f64 / self.lot_size_cents as f64;
        if !lots.is_finite() || lots <= 0.0 {
            return Err(invalid("wire volume cannot be converted to lots"));
        }
        Ok(lots)
    }

    pub fn lots_to_wire_volume(&self, lots: f64) -> Result<i64, BrokerFinancialTruthError> {
        if !lots.is_finite() || lots <= 0.0 {
            return Err(invalid("lots must be finite and positive"));
        }
        let raw = lots * self.lot_size_cents as f64;
        if raw > i64::MAX as f64 || (raw.round() - raw).abs() > 1e-8 {
            return Err(invalid("lots do not map exactly to broker wire volume"));
        }
        let wire = raw.round() as i64;
        if wire < self.min_volume_cents
            || wire > self.max_volume_cents
            || wire % self.step_volume_cents != 0
        {
            return Err(invalid("lots violate broker min/max/step volume"));
        }
        Ok(wire)
    }

    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        self.validate_payload()?;
        if self.content_hash != self.recomputed_hash()? {
            return Err(invalid("symbol contract provenance hash mismatch"));
        }
        Ok(())
    }

    fn validate_payload(&self) -> Result<(), BrokerFinancialTruthError> {
        self.provenance.validate()?;
        if self.schema_version != BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION
            || self.account_id <= 0
            || self.account_asset_id <= 0
            || self.symbol_id <= 0
            || self.base_asset_id <= 0
            || self.quote_asset_id <= 0
            || self.base_asset_id == self.quote_asset_id
            || self.base_asset == self.quote_asset
            || self.symbol.trim().is_empty()
            || self.digits > 18
            || self.pip_position > 18
            || !positive(self.pip_size)
            || !positive(self.contract_units_per_lot)
            || self.lot_size_cents <= 0
            || self.min_volume_cents <= 0
            || self.max_volume_cents < self.min_volume_cents
            || self.step_volume_cents <= 0
            || self.min_volume_cents % self.step_volume_cents != 0
            || self.max_volume_cents % self.step_volume_cents != 0
            || self.contract_units_per_lot.to_bits()
                != (self.lot_size_cents as f64 / 100.0).to_bits()
            || self.pip_size.to_bits() != 10_f64.powi(-(self.pip_position as i32)).to_bits()
            || !nonnegative(self.commission.rate)
            || !nonnegative(self.commission.minimum_per_deal)
            || self.commission.minimum_asset.trim().is_empty()
            || !self.swap.long.is_finite()
            || !self.swap.short.is_finite()
            || self.swap.period_hours <= 0
            || self.swap.period_hours != 24
            || !(0..1440).contains(&self.swap.time_minutes_from_utc_midnight)
            || self.swap.skip_periods < 0
            || self
                .swap
                .triple_day
                .is_some_and(|day| !(1..=7).contains(&day))
            || !nonnegative(self.pnl_conversion_fee_rate)
            || self.pnl_conversion_fee_rate >= 1.0
            || self.schedule_time_zone.trim().is_empty()
            || self.trading_intervals.is_empty()
        {
            return Err(invalid(
                "exact ProtoOA symbol contract is incomplete or invalid",
            ));
        }
        let expected_minimum_asset = match self.commission.minimum_currency {
            MinimumCommissionCurrency::Account => &self.account_currency,
            MinimumCommissionCurrency::Quote => &self.quote_asset,
        };
        if !self
            .commission
            .minimum_asset
            .eq_ignore_ascii_case(expected_minimum_asset)
        {
            return Err(invalid(
                "minimum commission asset does not match its currency semantics",
            ));
        }
        if self.swap.calculation != SwapCalculationExact::Pips {
            return Err(invalid(
                "swap calculation type is not supported by exact historical execution",
            ));
        }
        for interval in &self.trading_intervals {
            if interval.start_second_from_sunday >= interval.end_second_from_sunday
                || interval.end_second_from_sunday > 7 * 86_400
            {
                return Err(invalid("broker trading interval is invalid"));
            }
        }
        for holiday in &self.holidays {
            if holiday.holiday_id <= 0
                || holiday.name.trim().is_empty()
                || holiday.schedule_time_zone.trim().is_empty()
            {
                return Err(invalid("broker holiday contract is invalid"));
            }
        }
        Ok(())
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&SymbolHash {
            schema_version: self.schema_version,
            account_id: self.account_id,
            account_asset_id: self.account_asset_id,
            account_currency: &self.account_currency,
            symbol_id: self.symbol_id,
            symbol: &self.symbol,
            base_asset_id: self.base_asset_id,
            base_asset: &self.base_asset,
            quote_asset_id: self.quote_asset_id,
            quote_asset: &self.quote_asset,
            digits: self.digits,
            pip_position: self.pip_position,
            pip_size: self.pip_size,
            lot_size_cents: self.lot_size_cents,
            contract_units_per_lot: self.contract_units_per_lot,
            min_volume_cents: self.min_volume_cents,
            max_volume_cents: self.max_volume_cents,
            step_volume_cents: self.step_volume_cents,
            commission: &self.commission,
            swap: &self.swap,
            pnl_conversion_fee_rate: self.pnl_conversion_fee_rate,
            short_selling_enabled: self.short_selling_enabled,
            schedule_time_zone: &self.schedule_time_zone,
            trading_intervals: &self.trading_intervals,
            holidays: &self.holidays,
            provenance: &self.provenance,
        })
        .map_err(|error| invalid(error.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SynchronizedQuote {
    pub timestamp_ms: i64,
    pub bid: f64,
    pub ask: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SynchronizedBidAsk {
    pub schema_version: u16,
    pub symbol_id: i64,
    pub symbol: String,
    pub quotes: Vec<SynchronizedQuote>,
    pub bid_source_hash: String,
    pub ask_source_hash: String,
    pub provenance: EvidenceProvenance,
    pub content_hash: String,
}

impl SynchronizedBidAsk {
    pub fn new(
        symbol_id: i64,
        symbol: impl Into<String>,
        quotes: Vec<SynchronizedQuote>,
        bid_source_hash: impl Into<String>,
        ask_source_hash: impl Into<String>,
        provenance: EvidenceProvenance,
    ) -> Result<Self, BrokerFinancialTruthError> {
        let mut value = Self {
            schema_version: BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION,
            symbol_id,
            symbol: symbol.into(),
            quotes,
            bid_source_hash: bid_source_hash.into(),
            ask_source_hash: ask_source_hash.into(),
            provenance,
            content_hash: String::new(),
        };
        value.validate_payload()?;
        value.content_hash = value.recomputed_hash()?;
        Ok(value)
    }

    pub fn at(&self, timestamp_ms: i64) -> Result<SynchronizedQuote, BrokerFinancialTruthError> {
        self.quotes
            .binary_search_by_key(&timestamp_ms, |quote| quote.timestamp_ms)
            .ok()
            .map(|index| self.quotes[index])
            .ok_or_else(|| missing(format!("no synchronized quote at {timestamp_ms}")))
    }

    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        self.validate_payload()?;
        if self.content_hash != self.recomputed_hash()? {
            return Err(invalid("bid/ask evidence hash mismatch"));
        }
        Ok(())
    }

    fn validate_payload(&self) -> Result<(), BrokerFinancialTruthError> {
        self.provenance.validate()?;
        if self.schema_version != BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION
            || self.symbol_id <= 0
            || self.symbol.trim().is_empty()
            || self.quotes.is_empty()
            || !valid_hash(&self.bid_source_hash)
            || !valid_hash(&self.ask_source_hash)
            || self.bid_source_hash == self.ask_source_hash
        {
            return Err(invalid("synchronized bid/ask identity is incomplete"));
        }
        let mut previous = None;
        for quote in &self.quotes {
            if quote.timestamp_ms <= 0
                || !positive(quote.bid)
                || !positive(quote.ask)
                || quote.ask < quote.bid
                || previous.is_some_and(|ts| quote.timestamp_ms <= ts)
            {
                return Err(invalid(
                    "bid/ask quotes are crossed, duplicate, or out of order",
                ));
            }
            previous = Some(quote.timestamp_ms);
        }
        Ok(())
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&(
            self.schema_version,
            self.symbol_id,
            &self.symbol,
            &self.quotes,
            &self.bid_source_hash,
            &self.ask_source_hash,
            &self.provenance,
        ))
        .map_err(|error| invalid(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversionBook {
    pub account_currency: String,
    pub legs: Vec<SynchronizedConversionLeg>,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SynchronizedConversionLeg {
    pub symbol_contract_hash: String,
    pub symbol_id: i64,
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub quotes: SynchronizedBidAsk,
    pub content_hash: String,
}

impl SynchronizedConversionLeg {
    pub fn new(
        contract: &ExactProtoOaSymbolContract,
        quotes: SynchronizedBidAsk,
    ) -> Result<Self, BrokerFinancialTruthError> {
        contract.validate()?;
        quotes.validate()?;
        if contract.symbol_id != quotes.symbol_id || contract.symbol != quotes.symbol {
            return Err(invalid(
                "conversion quote evidence does not match its exact symbol contract",
            ));
        }
        let mut value = Self {
            symbol_contract_hash: contract.content_hash.clone(),
            symbol_id: contract.symbol_id,
            symbol: contract.symbol.clone(),
            base_asset: contract.base_asset.clone(),
            quote_asset: contract.quote_asset.clone(),
            quotes,
            content_hash: String::new(),
        };
        value.content_hash = value.recomputed_hash()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        self.quotes.validate()?;
        if !valid_hash(&self.symbol_contract_hash)
            || self.symbol_id != self.quotes.symbol_id
            || self.symbol != self.quotes.symbol
            || canonical_asset(self.base_asset.clone())? != self.base_asset
            || canonical_asset(self.quote_asset.clone())? != self.quote_asset
            || self.content_hash != self.recomputed_hash()?
        {
            return Err(invalid(
                "synchronized conversion leg is invalid or modified",
            ));
        }
        Ok(())
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&(
            &self.symbol_contract_hash,
            self.symbol_id,
            &self.symbol,
            &self.base_asset,
            &self.quote_asset,
            &self.quotes.content_hash,
        ))
        .map_err(|error| invalid(error.to_string()))
    }
}

impl ConversionBook {
    pub fn new(
        account_currency: impl Into<String>,
        legs: Vec<SynchronizedConversionLeg>,
    ) -> Result<Self, BrokerFinancialTruthError> {
        let mut value = Self {
            account_currency: canonical_asset(account_currency.into())?,
            legs,
            content_hash: String::new(),
        };
        value.validate_payload()?;
        value.content_hash = value.recomputed_hash()?;
        Ok(value)
    }

    pub fn convert_to_account(
        &self,
        amount: f64,
        source_currency: &str,
        timestamp_ms: i64,
    ) -> Result<f64, BrokerFinancialTruthError> {
        if !amount.is_finite() {
            return Err(invalid("conversion amount is non-finite"));
        }
        let source = canonical_asset(source_currency.to_string())?;
        if amount == 0.0 {
            return Ok(0.0);
        }
        if source == self.account_currency {
            return Ok(amount);
        }
        for leg in &self.legs {
            if leg.base_asset == source && leg.quote_asset == self.account_currency {
                let tick = leg.quotes.at(timestamp_ms)?;
                return Ok(amount * if amount >= 0.0 { tick.bid } else { tick.ask });
            }
            if leg.quote_asset == source && leg.base_asset == self.account_currency {
                let tick = leg.quotes.at(timestamp_ms)?;
                return Ok(amount / if amount >= 0.0 { tick.ask } else { tick.bid });
            }
        }
        Err(missing(format!(
            "no exact direct or inverse conversion from {source} to {} at {timestamp_ms}",
            self.account_currency
        )))
    }

    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        self.validate_payload()?;
        if self.content_hash != self.recomputed_hash()? {
            return Err(invalid("conversion evidence hash mismatch"));
        }
        Ok(())
    }

    fn validate_payload(&self) -> Result<(), BrokerFinancialTruthError> {
        let mut symbols = BTreeSet::new();
        for leg in &self.legs {
            leg.validate()?;
            if !symbols.insert(leg.symbol.clone()) {
                return Err(invalid("duplicate conversion leg"));
            }
        }
        Ok(())
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&(&self.account_currency, &self.legs))
            .map_err(|error| invalid(error.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionDirection {
    Long,
    Short,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerExactTradeRequest {
    pub direction: PositionDirection,
    pub lots: f64,
    pub entry_timestamp_ms: i64,
    pub exit_timestamp_ms: i64,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub adverse_slippage_price: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerExactTradeResult {
    pub entry_price: f64,
    pub exit_price: f64,
    pub exit_timestamp_ms: i64,
    pub gross_pnl_quote: f64,
    pub commission_account: f64,
    pub swap_account: f64,
    pub conversion_fee_account: f64,
    pub net_pnl_account: f64,
    pub truth_hash: String,
}

pub fn evaluate_broker_exact_trade(
    contract: &ExactProtoOaSymbolContract,
    quotes: &SynchronizedBidAsk,
    conversions: &ConversionBook,
    request: &BrokerExactTradeRequest,
) -> Result<BrokerExactTradeResult, BrokerFinancialTruthError> {
    contract.validate()?;
    quotes.validate()?;
    conversions.validate()?;
    if quotes.symbol_id != contract.symbol_id || quotes.symbol != contract.symbol {
        return Err(invalid("quote evidence does not match symbol contract"));
    }
    if conversions.account_currency != contract.account_currency {
        return Err(invalid(
            "conversion account currency does not match symbol contract",
        ));
    }
    if !positive(request.lots)
        || request.entry_timestamp_ms <= 0
        || request.exit_timestamp_ms <= request.entry_timestamp_ms
        || !nonnegative(request.adverse_slippage_price)
    {
        return Err(invalid("broker-exact trade request is invalid"));
    }
    contract.lots_to_wire_volume(request.lots)?;
    if request.direction == PositionDirection::Short && !contract.short_selling_enabled {
        return Err(invalid("broker contract forbids short selling"));
    }

    let entry_quote = quotes.at(request.entry_timestamp_ms)?;
    let entry_price = match request.direction {
        PositionDirection::Long => entry_quote.ask + request.adverse_slippage_price,
        PositionDirection::Short => entry_quote.bid - request.adverse_slippage_price,
    };
    let mut exit_quote = quotes.at(request.exit_timestamp_ms)?;
    for quote in quotes.quotes.iter().copied().filter(|quote| {
        quote.timestamp_ms > request.entry_timestamp_ms
            && quote.timestamp_ms <= request.exit_timestamp_ms
    }) {
        let liquidation = match request.direction {
            PositionDirection::Long => quote.bid,
            PositionDirection::Short => quote.ask,
        };
        let stop_hit = request
            .stop_loss
            .is_some_and(|stop| match request.direction {
                PositionDirection::Long => liquidation <= stop,
                PositionDirection::Short => liquidation >= stop,
            });
        let take_hit = request
            .take_profit
            .is_some_and(|take| match request.direction {
                PositionDirection::Long => liquidation >= take,
                PositionDirection::Short => liquidation <= take,
            });
        if stop_hit || take_hit {
            exit_quote = quote;
            break;
        }
    }
    let exit_price = match request.direction {
        PositionDirection::Long => exit_quote.bid - request.adverse_slippage_price,
        PositionDirection::Short => exit_quote.ask + request.adverse_slippage_price,
    };
    if !positive(entry_price) || !positive(exit_price) {
        return Err(invalid("adverse slippage produced invalid execution price"));
    }
    let signed_delta = match request.direction {
        PositionDirection::Long => exit_price - entry_price,
        PositionDirection::Short => entry_price - exit_price,
    };
    let gross_pnl_quote = signed_delta * contract.contract_units_per_lot * request.lots;
    let gross_account = conversions.convert_to_account(
        gross_pnl_quote,
        &contract.quote_asset,
        exit_quote.timestamp_ms,
    )?;
    let entry_commission = commission_for_deal(
        contract,
        conversions,
        request.lots,
        entry_price,
        request.entry_timestamp_ms,
    )?;
    let exit_commission = commission_for_deal(
        contract,
        conversions,
        request.lots,
        exit_price,
        exit_quote.timestamp_ms,
    )?;
    let commission_account = entry_commission + exit_commission;
    let swap_quote = swap_quote_cashflow(contract, request, exit_quote.timestamp_ms)?;
    let swap_account = conversions.convert_to_account(
        swap_quote,
        &contract.quote_asset,
        exit_quote.timestamp_ms,
    )?;
    let conversion_fee_account = if contract.quote_asset == contract.account_currency {
        0.0
    } else {
        gross_account.abs() * contract.pnl_conversion_fee_rate
    };
    let before_fee = gross_account + swap_account - commission_account;
    let net_pnl_account = before_fee - conversion_fee_account;
    let truth_hash = stable_json_hash(&(
        &contract.content_hash,
        &quotes.content_hash,
        &conversions.content_hash,
        request,
        entry_price,
        exit_price,
        gross_pnl_quote,
        commission_account,
        swap_account,
        conversion_fee_account,
        net_pnl_account,
    ))
    .map_err(|error| invalid(error.to_string()))?;
    Ok(BrokerExactTradeResult {
        entry_price,
        exit_price,
        exit_timestamp_ms: exit_quote.timestamp_ms,
        gross_pnl_quote,
        commission_account,
        swap_account,
        conversion_fee_account,
        net_pnl_account,
        truth_hash,
    })
}

fn commission_for_deal(
    contract: &ExactProtoOaSymbolContract,
    conversions: &ConversionBook,
    lots: f64,
    price: f64,
    timestamp_ms: i64,
) -> Result<f64, BrokerFinancialTruthError> {
    let schedule = &contract.commission;
    let (raw, currency) = match schedule.commission_type {
        CommissionTypeExact::UsdPerMillionUsd => {
            if schedule.rate == 0.0 {
                return minimum_commission_account(contract, conversions, timestamp_ms);
            }
            let notional_quote = contract.contract_units_per_lot * lots * price;
            let notional_usd = conversions.convert_to_account_for(
                notional_quote,
                &contract.quote_asset,
                "USD",
                timestamp_ms,
            )?;
            (schedule.rate * notional_usd / 1_000_000.0, "USD")
        }
        CommissionTypeExact::UsdPerLot => (schedule.rate * lots, "USD"),
        CommissionTypeExact::PercentageOfValue => (
            contract.contract_units_per_lot * lots * price * schedule.rate / 100.0,
            contract.quote_asset.as_str(),
        ),
        CommissionTypeExact::QuoteCurrencyPerLot => {
            (schedule.rate * lots, contract.quote_asset.as_str())
        }
    };
    let converted = conversions.convert_to_account(raw, currency, timestamp_ms)?;
    let minimum = minimum_commission_account(contract, conversions, timestamp_ms)?;
    Ok(converted.max(minimum))
}

fn minimum_commission_account(
    contract: &ExactProtoOaSymbolContract,
    conversions: &ConversionBook,
    timestamp_ms: i64,
) -> Result<f64, BrokerFinancialTruthError> {
    match contract.commission.minimum_currency {
        MinimumCommissionCurrency::Account => Ok(contract.commission.minimum_per_deal),
        MinimumCommissionCurrency::Quote => conversions.convert_to_account(
            contract.commission.minimum_per_deal,
            &contract.quote_asset,
            timestamp_ms,
        ),
    }
}

impl ConversionBook {
    fn convert_to_account_for(
        &self,
        amount: f64,
        source_currency: &str,
        target_currency: &str,
        timestamp_ms: i64,
    ) -> Result<f64, BrokerFinancialTruthError> {
        let target = canonical_asset(target_currency.to_string())?;
        if target == self.account_currency {
            return self.convert_to_account(amount, source_currency, timestamp_ms);
        }
        let temporary = Self::new(target, self.legs.clone())?;
        temporary.convert_to_account(amount, source_currency, timestamp_ms)
    }
}

fn swap_quote_cashflow(
    contract: &ExactProtoOaSymbolContract,
    request: &BrokerExactTradeRequest,
    exit_timestamp_ms: i64,
) -> Result<f64, BrokerFinancialTruthError> {
    if contract.swap.calculation != SwapCalculationExact::Pips {
        return Err(invalid("unsupported exact swap calculation"));
    }
    let mut charged = 0_i64;
    let mut cashflow_pips = 0.0;
    let start = Utc
        .timestamp_millis_opt(request.entry_timestamp_ms)
        .single()
        .ok_or_else(|| invalid("entry timestamp is outside chrono range"))?;
    let end = Utc
        .timestamp_millis_opt(exit_timestamp_ms)
        .single()
        .ok_or_else(|| invalid("exit timestamp is outside chrono range"))?;
    let first_date = start.date_naive();
    let last_date = end.date_naive();
    let days = (last_date - first_date).num_days();
    if contract.swap.period_hours != 24 {
        return Err(invalid(
            "exact historical execution currently supports only 24-hour swap periods",
        ));
    }
    for day_offset in 0..=days {
        let date = first_date + chrono::Duration::days(day_offset);
        for _ in 0..1 {
            let minute = i64::from(contract.swap.time_minutes_from_utc_midnight);
            let date_time = Utc
                .with_ymd_and_hms(
                    date.year(),
                    date.month(),
                    date.day(),
                    (minute / 60) as u32,
                    (minute % 60) as u32,
                    0,
                )
                .single()
                .ok_or_else(|| invalid("invalid swap rollover time"))?;
            let ts = date_time.timestamp_millis();
            if ts <= request.entry_timestamp_ms || ts > exit_timestamp_ms {
                continue;
            }
            if !contract.swap.charge_at_weekends
                && matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
            {
                continue;
            }
            charged += 1;
            if charged <= i64::from(contract.swap.skip_periods) {
                continue;
            }
            let weekday = weekday_number(date.weekday());
            let multiplier = if contract.swap.triple_day == Some(weekday) {
                3.0
            } else {
                1.0
            };
            cashflow_pips += match request.direction {
                PositionDirection::Long => contract.swap.long,
                PositionDirection::Short => contract.swap.short,
            } * multiplier;
        }
    }
    Ok(cashflow_pips * contract.pip_size * contract.contract_units_per_lot * request.lots)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerPositionFinancialRow {
    pub position_id: i64,
    pub symbol_id: i64,
    pub direction: PositionDirection,
    pub volume_units: f64,
    pub entry_price: f64,
    pub gross_unrealized_pnl_account: f64,
    pub net_unrealized_pnl_account: f64,
    pub swap_account: Option<f64>,
    pub commission_account: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerPositionFinancialSnapshot {
    pub account_id: i64,
    pub account_currency: String,
    pub captured_at_ms: i64,
    pub max_age_ms: i64,
    pub positions: Vec<BrokerPositionFinancialRow>,
    pub provenance: EvidenceProvenance,
    pub content_hash: String,
}

impl BrokerPositionFinancialSnapshot {
    pub fn new(
        account_id: i64,
        account_currency: impl Into<String>,
        captured_at_ms: i64,
        max_age_ms: i64,
        positions: Vec<BrokerPositionFinancialRow>,
        provenance: EvidenceProvenance,
    ) -> Result<Self, BrokerFinancialTruthError> {
        let mut value = Self {
            account_id,
            account_currency: canonical_asset(account_currency.into())?,
            captured_at_ms,
            max_age_ms,
            positions,
            provenance,
            content_hash: String::new(),
        };
        value.validate_payload()?;
        value.content_hash = value.recomputed_hash()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        self.validate_payload()?;
        if self.content_hash != self.recomputed_hash()? {
            return Err(invalid("broker position PnL evidence hash mismatch"));
        }
        Ok(())
    }

    fn validate_payload(&self) -> Result<(), BrokerFinancialTruthError> {
        self.provenance.validate()?;
        if self.account_id <= 0
            || self.captured_at_ms <= 0
            || self.max_age_ms <= 0
            || self.provenance.captured_at_ms != self.captured_at_ms
        {
            return Err(invalid("broker position snapshot identity is incomplete"));
        }
        let mut ids = BTreeSet::new();
        for row in &self.positions {
            if row.position_id <= 0
                || row.symbol_id <= 0
                || !ids.insert(row.position_id)
                || !positive(row.volume_units)
                || !positive(row.entry_price)
                || !row.gross_unrealized_pnl_account.is_finite()
                || !row.net_unrealized_pnl_account.is_finite()
                || row.swap_account.is_some_and(|value| !value.is_finite())
                || row
                    .commission_account
                    .is_some_and(|value| !value.is_finite())
            {
                return Err(invalid("broker position snapshot contains invalid row"));
            }
        }
        Ok(())
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&(
            self.account_id,
            &self.account_currency,
            self.captured_at_ms,
            self.max_age_ms,
            &self.positions,
            &self.provenance,
        ))
        .map_err(|error| invalid(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedLocalClose {
    pub account_id: i64,
    pub symbol_id: i64,
    pub position_id: i64,
    pub direction: PositionDirection,
    pub expected_volume_units: f64,
    pub local_estimated_pnl_account: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokerCloseDeal {
    pub deal_id: i64,
    pub order_id: i64,
    pub position_id: i64,
    pub symbol_id: i64,
    pub direction: PositionDirection,
    pub filled_volume_units: f64,
    pub execution_timestamp_ms: i64,
    pub gross_profit_account: f64,
    pub commission_account: f64,
    pub swap_account: f64,
    pub conversion_fee_account: f64,
    pub net_profit_account: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationState {
    Reconciled,
    Partial,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloseDealReconciliation {
    pub expected: ExpectedLocalClose,
    pub state: ReconciliationState,
    pub unique_deals: Vec<BrokerCloseDeal>,
    pub reconciled_volume_units: f64,
    pub broker_net_profit_account: Option<f64>,
    pub provenance: EvidenceProvenance,
    pub content_hash: String,
}

pub fn reconcile_close_deals(
    expected: ExpectedLocalClose,
    deals: Vec<BrokerCloseDeal>,
    provenance: EvidenceProvenance,
) -> Result<CloseDealReconciliation, BrokerFinancialTruthError> {
    if expected.account_id <= 0
        || expected.symbol_id <= 0
        || expected.position_id <= 0
        || !positive(expected.expected_volume_units)
        || expected
            .local_estimated_pnl_account
            .is_some_and(|value| !value.is_finite())
    {
        return Err(invalid("expected local close is invalid"));
    }
    provenance.validate()?;
    let mut by_id = BTreeMap::new();
    for deal in deals {
        if deal.deal_id <= 0
            || deal.order_id <= 0
            || deal.position_id != expected.position_id
            || deal.symbol_id != expected.symbol_id
            || deal.direction == expected.direction
            || !positive(deal.filled_volume_units)
            || deal.execution_timestamp_ms <= 0
            || !deal.gross_profit_account.is_finite()
            || !deal.commission_account.is_finite()
            || !deal.swap_account.is_finite()
            || !deal.conversion_fee_account.is_finite()
            || !deal.net_profit_account.is_finite()
        {
            return Err(invalid("broker close deal does not match expected closure"));
        }
        if let Some(existing) = by_id.get(&deal.deal_id) {
            if existing != &deal {
                return Err(invalid("duplicate deal id has conflicting contents"));
            }
            continue;
        }
        by_id.insert(deal.deal_id, deal);
    }
    let unique_deals: Vec<_> = by_id.into_values().collect();
    let reconciled_volume_units: f64 = unique_deals
        .iter()
        .map(|deal| deal.filled_volume_units)
        .sum();
    if reconciled_volume_units > expected.expected_volume_units + 1e-8 {
        return Err(invalid(
            "broker close volume exceeds expected position volume",
        ));
    }
    let state = if unique_deals.is_empty() {
        ReconciliationState::Unresolved
    } else if (reconciled_volume_units - expected.expected_volume_units).abs() <= 1e-8 {
        ReconciliationState::Reconciled
    } else {
        ReconciliationState::Partial
    };
    let broker_net_profit_account = (state == ReconciliationState::Reconciled).then(|| {
        unique_deals
            .iter()
            .map(|deal| deal.net_profit_account)
            .sum()
    });
    let mut value = CloseDealReconciliation {
        expected,
        state,
        unique_deals,
        reconciled_volume_units,
        broker_net_profit_account,
        provenance,
        content_hash: String::new(),
    };
    value.content_hash = value.recomputed_hash()?;
    Ok(value)
}

impl CloseDealReconciliation {
    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        let rebuilt = reconcile_close_deals(
            self.expected.clone(),
            self.unique_deals.clone(),
            self.provenance.clone(),
        )?;
        if rebuilt.content_hash != self.content_hash
            || rebuilt.state != self.state
            || rebuilt.reconciled_volume_units.to_bits() != self.reconciled_volume_units.to_bits()
            || rebuilt.broker_net_profit_account.map(f64::to_bits)
                != self.broker_net_profit_account.map(f64::to_bits)
        {
            return Err(invalid("close reconciliation evidence was modified"));
        }
        Ok(())
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&(
            &self.expected,
            self.state,
            &self.unique_deals,
            self.reconciled_volume_units,
            self.broker_net_profit_account,
            &self.provenance,
        ))
        .map_err(|error| invalid(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoricalBrokerTruthEvidence {
    pub symbol_contract: ExactProtoOaSymbolContract,
    pub bid_ask: SynchronizedBidAsk,
    pub conversions: ConversionBook,
    pub initial_equity: f64,
    pub risk_config_hash: String,
    pub strategy_hash: String,
    pub slippage_policy_hash: String,
    pub content_hash: String,
}

impl HistoricalBrokerTruthEvidence {
    pub fn new(
        symbol_contract: ExactProtoOaSymbolContract,
        bid_ask: SynchronizedBidAsk,
        conversions: ConversionBook,
        initial_equity: f64,
        risk_config_hash: impl Into<String>,
        strategy_hash: impl Into<String>,
        slippage_policy_hash: impl Into<String>,
    ) -> Result<Self, BrokerFinancialTruthError> {
        let mut value = Self {
            symbol_contract,
            bid_ask,
            conversions,
            initial_equity,
            risk_config_hash: risk_config_hash.into(),
            strategy_hash: strategy_hash.into(),
            slippage_policy_hash: slippage_policy_hash.into(),
            content_hash: String::new(),
        };
        value.validate_payload()?;
        value.content_hash = value.recomputed_hash()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        self.validate_payload()?;
        if self.content_hash != self.recomputed_hash()? {
            return Err(invalid("historical broker truth hash mismatch"));
        }
        Ok(())
    }

    fn validate_payload(&self) -> Result<(), BrokerFinancialTruthError> {
        self.symbol_contract.validate()?;
        self.bid_ask.validate()?;
        self.conversions.validate()?;
        if self.symbol_contract.symbol_id != self.bid_ask.symbol_id
            || self.symbol_contract.symbol != self.bid_ask.symbol
            || self.symbol_contract.account_currency != self.conversions.account_currency
            || !positive(self.initial_equity)
            || !valid_hash(&self.risk_config_hash)
            || !valid_hash(&self.strategy_hash)
            || !valid_hash(&self.slippage_policy_hash)
        {
            return Err(invalid("historical broker truth binding is incomplete"));
        }
        if self.symbol_contract.quote_asset != self.conversions.account_currency {
            for quote in &self.bid_ask.quotes {
                self.conversions.convert_to_account(
                    1.0,
                    &self.symbol_contract.quote_asset,
                    quote.timestamp_ms,
                )?;
            }
        }
        for quote in &self.bid_ask.quotes {
            commission_for_deal(
                &self.symbol_contract,
                &self.conversions,
                self.symbol_contract
                    .wire_volume_to_lots(self.symbol_contract.min_volume_cents)?,
                quote.ask,
                quote.timestamp_ms,
            )?;
        }
        Ok(())
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&(
            &self.symbol_contract.content_hash,
            &self.bid_ask.content_hash,
            &self.conversions.content_hash,
            self.initial_equity,
            &self.risk_config_hash,
            &self.strategy_hash,
            &self.slippage_policy_hash,
        ))
        .map_err(|error| invalid(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveBrokerTruthEvidence {
    pub historical: HistoricalBrokerTruthEvidence,
    pub positions: BrokerPositionFinancialSnapshot,
    pub close_reconciliations: Vec<CloseDealReconciliation>,
    pub content_hash: String,
}

impl LiveBrokerTruthEvidence {
    pub fn new(
        historical: HistoricalBrokerTruthEvidence,
        positions: BrokerPositionFinancialSnapshot,
        close_reconciliations: Vec<CloseDealReconciliation>,
    ) -> Result<Self, BrokerFinancialTruthError> {
        let mut value = Self {
            historical,
            positions,
            close_reconciliations,
            content_hash: String::new(),
        };
        value.validate_payload()?;
        value.content_hash = value.recomputed_hash()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        self.validate_payload()?;
        if self.content_hash != self.recomputed_hash()? {
            return Err(invalid("live broker truth hash mismatch"));
        }
        Ok(())
    }

    fn validate_payload(&self) -> Result<(), BrokerFinancialTruthError> {
        self.historical.validate()?;
        self.positions.validate()?;
        if self.positions.account_id != self.historical.symbol_contract.account_id
            || self.positions.account_currency != self.historical.conversions.account_currency
            || self.close_reconciliations.is_empty()
        {
            return Err(missing(
                "live broker truth is missing account-bound reconciliation",
            ));
        }
        for reconciliation in &self.close_reconciliations {
            reconciliation.validate()?;
            if reconciliation.state != ReconciliationState::Reconciled
                || reconciliation.expected.account_id != self.positions.account_id
            {
                return Err(missing(
                    "close-deal reconciliation is unresolved or partial",
                ));
            }
        }
        if !self.close_reconciliations.iter().any(|reconciliation| {
            reconciliation.expected.symbol_id == self.historical.symbol_contract.symbol_id
        }) {
            return Err(missing(
                "close-deal reconciliation does not bind the historical symbol",
            ));
        }
        Ok(())
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&(
            &self.historical.content_hash,
            &self.positions.content_hash,
            self.close_reconciliations
                .iter()
                .map(|value| value.content_hash.as_str())
                .collect::<Vec<_>>(),
        ))
        .map_err(|error| invalid(error.to_string()))
    }
}

fn contract_requires_conversion(
    contract: &ExactProtoOaSymbolContract,
    account_currency: &str,
) -> bool {
    if !contract.quote_asset.eq_ignore_ascii_case(account_currency) {
        return true;
    }
    contract.commission.rate > 0.0
        && matches!(
            contract.commission.commission_type,
            CommissionTypeExact::UsdPerMillionUsd | CommissionTypeExact::UsdPerLot
        )
        && !account_currency.eq_ignore_ascii_case("USD")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerFinancialTruthReport {
    pub schema_version: u16,
    pub mode: FinancialTruthMode,
    pub broker_historical_ready: bool,
    pub live_broker_ready: bool,
    pub evidence_binding_hash: Option<String>,
    pub account_id: Option<i64>,
    pub account_currency: Option<String>,
    pub symbol_id: Option<i64>,
    pub symbol: Option<String>,
    pub strategy_hash: Option<String>,
    pub risk_config_hash: Option<String>,
    pub slippage_policy_hash: Option<String>,
    pub live_valid_until_ms: Option<i64>,
    pub capabilities: Vec<BrokerFinancialCapability>,
    pub truth_hash: String,
}

impl Default for BrokerFinancialTruthReport {
    fn default() -> Self {
        Self::mechanical("legacy_or_close_only_artifact")
    }
}

impl BrokerFinancialTruthReport {
    pub fn mechanical(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let capabilities = BrokerFinancialCapabilityKind::ALL
            .iter()
            .copied()
            .map(|kind| BrokerFinancialCapability {
                kind,
                state: BrokerFinancialCapabilityState::Missing,
                reason: reason.clone(),
                provenance_hash: None,
            })
            .collect::<Vec<_>>();
        let truth_hash = stable_json_hash(&(
            BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION,
            FinancialTruthMode::Mechanical,
            false,
            false,
            Option::<String>::None,
            Option::<i64>::None,
            Option::<String>::None,
            Option::<i64>::None,
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None,
            Option::<i64>::None,
            &capabilities,
        ))
        .unwrap_or_default();
        Self {
            schema_version: BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION,
            mode: FinancialTruthMode::Mechanical,
            broker_historical_ready: false,
            live_broker_ready: false,
            evidence_binding_hash: None,
            account_id: None,
            account_currency: None,
            symbol_id: None,
            symbol: None,
            strategy_hash: None,
            risk_config_hash: None,
            slippage_policy_hash: None,
            live_valid_until_ms: None,
            capabilities,
            truth_hash,
        }
    }

    pub fn historical(
        evidence: &HistoricalBrokerTruthEvidence,
    ) -> Result<Self, BrokerFinancialTruthError> {
        evidence.validate()?;
        let conversion_required = contract_requires_conversion(
            &evidence.symbol_contract,
            &evidence.conversions.account_currency,
        );
        let capabilities = vec![
            present(
                BrokerFinancialCapabilityKind::SynchronizedHistoricalBidAsk,
                &evidence.bid_ask.content_hash,
            ),
            if conversion_required {
                present(
                    BrokerFinancialCapabilityKind::SynchronizedConversionLegs,
                    &evidence.conversions.content_hash,
                )
            } else {
                not_required(
                    BrokerFinancialCapabilityKind::SynchronizedConversionLegs,
                    "quote currency equals account currency",
                )
            },
            present(
                BrokerFinancialCapabilityKind::ExactProtoOaSymbolContract,
                &evidence.symbol_contract.content_hash,
            ),
            missing_cap(
                BrokerFinancialCapabilityKind::BrokerPositionUnrealizedPnl,
                "live broker position snapshot not supplied",
            ),
            missing_cap(
                BrokerFinancialCapabilityKind::CloseDealReconciliation,
                "broker close-deal reconciliation not supplied",
            ),
        ];
        Self::from_parts(
            FinancialTruthMode::BrokerExactHistorical,
            true,
            false,
            Some(evidence.content_hash.clone()),
            Some(evidence.symbol_contract.account_id),
            Some(evidence.symbol_contract.account_currency.clone()),
            Some(evidence.symbol_contract.symbol_id),
            Some(evidence.symbol_contract.symbol.clone()),
            Some(evidence.strategy_hash.clone()),
            Some(evidence.risk_config_hash.clone()),
            Some(evidence.slippage_policy_hash.clone()),
            None,
            capabilities,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn assess_historical(
        symbol_contract: Option<&ExactProtoOaSymbolContract>,
        bid_ask: Option<&SynchronizedBidAsk>,
        conversions: Option<&ConversionBook>,
        account_currency: &str,
        initial_equity: f64,
        risk_config_hash: &str,
        strategy_hash: &str,
        slippage_policy_hash: &str,
    ) -> Self {
        let contract_capability = assess_component(
            BrokerFinancialCapabilityKind::ExactProtoOaSymbolContract,
            symbol_contract,
            ExactProtoOaSymbolContract::validate,
            |value| &value.content_hash,
        );
        let bid_ask_capability = assess_component(
            BrokerFinancialCapabilityKind::SynchronizedHistoricalBidAsk,
            bid_ask,
            SynchronizedBidAsk::validate,
            |value| &value.content_hash,
        );
        let conversion_required = symbol_contract
            .filter(|value| value.validate().is_ok())
            .is_none_or(|value| contract_requires_conversion(value, account_currency));
        let conversion_capability = if conversion_required {
            assess_component(
                BrokerFinancialCapabilityKind::SynchronizedConversionLegs,
                conversions,
                ConversionBook::validate,
                |value| &value.content_hash,
            )
        } else {
            not_required(
                BrokerFinancialCapabilityKind::SynchronizedConversionLegs,
                "quote currency equals account currency",
            )
        };
        let mut binding_error = canonical_asset(account_currency.to_string())
            .err()
            .map(|error| {
                (
                    BrokerFinancialCapabilityKind::SynchronizedConversionLegs,
                    error.to_string(),
                )
            });
        if let (Some(contract), Some(quotes)) = (symbol_contract, bid_ask) {
            let conversion = conversions.cloned().or_else(|| {
                (!conversion_required)
                    .then(|| ConversionBook::new(account_currency, vec![]).ok())
                    .flatten()
            });
            if contract.symbol_id != quotes.symbol_id || contract.symbol != quotes.symbol {
                binding_error = Some((
                    BrokerFinancialCapabilityKind::SynchronizedHistoricalBidAsk,
                    "historical BID/ASK identity does not match symbol contract".to_string(),
                ));
            } else if let Some(conversion) = conversion.as_ref()
                && !conversion
                    .account_currency
                    .eq_ignore_ascii_case(account_currency)
            {
                binding_error = Some((
                    BrokerFinancialCapabilityKind::SynchronizedConversionLegs,
                    "conversion account currency does not match requested account".to_string(),
                ));
            } else if binding_error.is_none()
                && let Some(conversion) = conversion
            {
                match HistoricalBrokerTruthEvidence::new(
                    contract.clone(),
                    quotes.clone(),
                    conversion,
                    initial_equity,
                    risk_config_hash,
                    strategy_hash,
                    slippage_policy_hash,
                ) {
                    Ok(evidence) => match Self::historical(&evidence) {
                        Ok(report) => return report,
                        Err(error) => {
                            binding_error = Some((
                                BrokerFinancialCapabilityKind::ExactProtoOaSymbolContract,
                                error.to_string(),
                            ));
                        }
                    },
                    Err(error) => {
                        binding_error = Some((
                            if conversion_required {
                                BrokerFinancialCapabilityKind::SynchronizedConversionLegs
                            } else {
                                BrokerFinancialCapabilityKind::ExactProtoOaSymbolContract
                            },
                            error.to_string(),
                        ));
                    }
                }
            }
        }
        let mut capabilities = vec![
            bid_ask_capability,
            conversion_capability,
            contract_capability,
            missing_cap(
                BrokerFinancialCapabilityKind::BrokerPositionUnrealizedPnl,
                "live broker position snapshot not supplied",
            ),
            missing_cap(
                BrokerFinancialCapabilityKind::CloseDealReconciliation,
                "broker close-deal reconciliation not supplied",
            ),
        ];
        if let Some((kind, error)) = binding_error {
            if let Some(capability) = capabilities.iter_mut().find(|value| value.kind == kind) {
                *capability = invalid_cap(
                    kind,
                    &format!("historical evidence binding failed: {error}"),
                );
            }
        }
        match Self::from_parts(
            FinancialTruthMode::BrokerExactHistorical,
            false,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            capabilities,
        ) {
            Ok(report) => report,
            Err(error) => Self::mechanical(format!("historical assessment invalid: {error}")),
        }
    }

    pub fn live(evidence: &LiveBrokerTruthEvidence) -> Result<Self, BrokerFinancialTruthError> {
        evidence.validate()?;
        let mut report = Self::historical(&evidence.historical)?;
        report.mode = FinancialTruthMode::LiveBroker;
        for capability in &mut report.capabilities {
            match capability.kind {
                BrokerFinancialCapabilityKind::BrokerPositionUnrealizedPnl => {
                    *capability = present(capability.kind, &evidence.positions.content_hash)
                }
                BrokerFinancialCapabilityKind::CloseDealReconciliation => {
                    *capability = present(capability.kind, &evidence.content_hash)
                }
                _ => {}
            }
        }
        report.live_broker_ready = true;
        report.evidence_binding_hash = Some(evidence.content_hash.clone());
        report.live_valid_until_ms = Some(
            evidence
                .positions
                .captured_at_ms
                .checked_add(evidence.positions.max_age_ms)
                .ok_or_else(|| invalid("broker position PnL validity overflow"))?,
        );
        report.truth_hash = report.recomputed_hash()?;
        Ok(report)
    }

    pub fn require_historical(&self) -> Result<(), BrokerFinancialTruthError> {
        self.validate()?;
        if !self.broker_historical_ready {
            return Err(self.unavailable("historical_evaluation"));
        }
        Ok(())
    }

    pub fn require_live(&self) -> Result<(), BrokerFinancialTruthError> {
        self.require_live_at(Utc::now().timestamp_millis())
    }

    pub fn require_live_at(&self, now_ms: i64) -> Result<(), BrokerFinancialTruthError> {
        self.validate()?;
        if !self.live_broker_ready {
            return Err(self.unavailable("live_trading"));
        }
        if self
            .live_valid_until_ms
            .is_none_or(|valid_until| now_ms > valid_until)
        {
            return Err(missing("broker position unrealized PnL snapshot is stale"));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), BrokerFinancialTruthError> {
        if self.schema_version != BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION
            || self.capabilities.len() != BrokerFinancialCapabilityKind::ALL.len()
            || self.truth_hash != self.recomputed_hash()?
        {
            return Err(invalid(
                "broker financial truth report is invalid or modified",
            ));
        }
        let by_kind: BTreeMap<_, _> = self
            .capabilities
            .iter()
            .map(|value| (value.kind, value))
            .collect();
        if by_kind.len() != BrokerFinancialCapabilityKind::ALL.len() {
            return Err(invalid(
                "broker financial truth report has duplicate capabilities",
            ));
        }
        for capability in &self.capabilities {
            if capability.reason.trim().is_empty()
                || (capability.state == BrokerFinancialCapabilityState::NotRequired
                    && capability.kind != BrokerFinancialCapabilityKind::SynchronizedConversionLegs)
                || match capability.state {
                    BrokerFinancialCapabilityState::Present => capability
                        .provenance_hash
                        .as_deref()
                        .is_none_or(|hash| !valid_hash(hash)),
                    BrokerFinancialCapabilityState::Missing
                    | BrokerFinancialCapabilityState::Invalid
                    | BrokerFinancialCapabilityState::NotRequired => {
                        capability.provenance_hash.is_some()
                    }
                }
            {
                return Err(invalid("broker financial capability state is invalid"));
            }
        }
        let historical_present = by_kind
            .get(&BrokerFinancialCapabilityKind::SynchronizedHistoricalBidAsk)
            .is_some_and(|value| value.state == BrokerFinancialCapabilityState::Present)
            && by_kind
                .get(&BrokerFinancialCapabilityKind::SynchronizedConversionLegs)
                .is_some_and(|value| {
                    matches!(
                        value.state,
                        BrokerFinancialCapabilityState::Present
                            | BrokerFinancialCapabilityState::NotRequired
                    )
                })
            && by_kind
                .get(&BrokerFinancialCapabilityKind::ExactProtoOaSymbolContract)
                .is_some_and(|value| value.state == BrokerFinancialCapabilityState::Present);
        let live_present = historical_present
            && [
                BrokerFinancialCapabilityKind::BrokerPositionUnrealizedPnl,
                BrokerFinancialCapabilityKind::CloseDealReconciliation,
            ]
            .iter()
            .all(|kind| {
                by_kind.get(kind).map(|value| value.state)
                    == Some(BrokerFinancialCapabilityState::Present)
            });
        if self.broker_historical_ready != historical_present
            || self.live_broker_ready != live_present
            || (self.mode == FinancialTruthMode::Mechanical
                && (self.broker_historical_ready || self.live_broker_ready))
            || (self.mode == FinancialTruthMode::BrokerExactHistorical && self.live_broker_ready)
            || (self.mode == FinancialTruthMode::LiveBroker && !self.live_broker_ready)
            || (self.broker_historical_ready && self.evidence_binding_hash.is_none())
            || self
                .evidence_binding_hash
                .as_deref()
                .is_some_and(|hash| !valid_hash(hash))
            || self
                .strategy_hash
                .as_deref()
                .is_some_and(|hash| !valid_hash(hash))
            || self
                .risk_config_hash
                .as_deref()
                .is_some_and(|hash| !valid_hash(hash))
            || self
                .slippage_policy_hash
                .as_deref()
                .is_some_and(|hash| !valid_hash(hash))
            || (self.broker_historical_ready
                && (self.account_id.is_none_or(|value| value <= 0)
                    || self
                        .account_currency
                        .as_deref()
                        .is_none_or(|value| canonical_asset(value.to_string()).is_err())
                    || self.symbol_id.is_none_or(|value| value <= 0)
                    || self.symbol.as_deref().is_none_or(str::is_empty)
                    || self.strategy_hash.is_none()
                    || self.risk_config_hash.is_none()
                    || self.slippage_policy_hash.is_none()))
            || (self.mode == FinancialTruthMode::Mechanical
                && (self.evidence_binding_hash.is_some()
                    || self.account_id.is_some()
                    || self.account_currency.is_some()
                    || self.symbol_id.is_some()
                    || self.symbol.is_some()
                    || self.strategy_hash.is_some()
                    || self.risk_config_hash.is_some()
                    || self.slippage_policy_hash.is_some()))
            || (self.mode != FinancialTruthMode::LiveBroker && self.live_valid_until_ms.is_some())
            || (self.mode == FinancialTruthMode::LiveBroker && self.live_valid_until_ms.is_none())
        {
            return Err(invalid(
                "broker financial truth readiness is not evidence-derived",
            ));
        }
        Ok(())
    }

    pub fn missing_capabilities(&self) -> Vec<BrokerFinancialCapabilityKind> {
        self.capabilities
            .iter()
            .filter(|value| {
                matches!(
                    value.state,
                    BrokerFinancialCapabilityState::Missing
                        | BrokerFinancialCapabilityState::Invalid
                )
            })
            .map(|value| value.kind)
            .collect()
    }

    fn from_parts(
        mode: FinancialTruthMode,
        broker_historical_ready: bool,
        live_broker_ready: bool,
        evidence_binding_hash: Option<String>,
        account_id: Option<i64>,
        account_currency: Option<String>,
        symbol_id: Option<i64>,
        symbol: Option<String>,
        strategy_hash: Option<String>,
        risk_config_hash: Option<String>,
        slippage_policy_hash: Option<String>,
        live_valid_until_ms: Option<i64>,
        capabilities: Vec<BrokerFinancialCapability>,
    ) -> Result<Self, BrokerFinancialTruthError> {
        let mut value = Self {
            schema_version: BROKER_FINANCIAL_TRUTH_SCHEMA_VERSION,
            mode,
            broker_historical_ready,
            live_broker_ready,
            evidence_binding_hash,
            account_id,
            account_currency,
            symbol_id,
            symbol,
            strategy_hash,
            risk_config_hash,
            slippage_policy_hash,
            live_valid_until_ms,
            capabilities,
            truth_hash: String::new(),
        };
        value.truth_hash = value.recomputed_hash()?;
        value.validate()?;
        Ok(value)
    }

    fn recomputed_hash(&self) -> Result<String, BrokerFinancialTruthError> {
        stable_json_hash(&(
            self.schema_version,
            self.mode,
            self.broker_historical_ready,
            self.live_broker_ready,
            &self.evidence_binding_hash,
            self.account_id,
            &self.account_currency,
            self.symbol_id,
            &self.symbol,
            &self.strategy_hash,
            &self.risk_config_hash,
            &self.slippage_policy_hash,
            self.live_valid_until_ms,
            &self.capabilities,
        ))
        .map_err(|error| invalid(error.to_string()))
    }

    fn unavailable(&self, operation: &str) -> BrokerFinancialTruthError {
        BrokerFinancialTruthError::Unavailable {
            operation: operation.to_string(),
            missing: self
                .missing_capabilities()
                .into_iter()
                .map(|kind| kind.as_str().to_string())
                .collect(),
        }
    }
}

pub fn save_historical_broker_truth(
    path: impl AsRef<Path>,
    evidence: &HistoricalBrokerTruthEvidence,
) -> anyhow::Result<()> {
    evidence.validate().map_err(anyhow::Error::new)?;
    write_json_atomic(path, evidence)
}

pub fn load_historical_broker_truth(
    path: impl AsRef<Path>,
) -> anyhow::Result<HistoricalBrokerTruthEvidence> {
    let evidence: HistoricalBrokerTruthEvidence =
        read_json(path, "historical broker financial truth")?;
    evidence.validate().map_err(anyhow::Error::new)?;
    Ok(evidence)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerFinancialTruthError {
    Invalid(String),
    Missing(String),
    Unavailable {
        operation: String,
        missing: Vec<String>,
    },
}

impl fmt::Display for BrokerFinancialTruthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(
                formatter,
                "{BROKER_FINANCIAL_TRUTH_UNAVAILABLE_V1} invalid={reason}"
            ),
            Self::Missing(reason) => write!(
                formatter,
                "{BROKER_FINANCIAL_TRUTH_UNAVAILABLE_V1} missing={reason}"
            ),
            Self::Unavailable { operation, missing } => write!(
                formatter,
                "{BROKER_FINANCIAL_TRUTH_UNAVAILABLE_V1} operation={operation} missing={}",
                missing.join(",")
            ),
        }
    }
}

impl std::error::Error for BrokerFinancialTruthError {}

fn present(kind: BrokerFinancialCapabilityKind, hash: &str) -> BrokerFinancialCapability {
    BrokerFinancialCapability {
        kind,
        state: BrokerFinancialCapabilityState::Present,
        reason: "validated authoritative evidence".to_string(),
        provenance_hash: Some(hash.to_string()),
    }
}

fn missing_cap(kind: BrokerFinancialCapabilityKind, reason: &str) -> BrokerFinancialCapability {
    BrokerFinancialCapability {
        kind,
        state: BrokerFinancialCapabilityState::Missing,
        reason: reason.to_string(),
        provenance_hash: None,
    }
}

fn invalid_cap(kind: BrokerFinancialCapabilityKind, reason: &str) -> BrokerFinancialCapability {
    BrokerFinancialCapability {
        kind,
        state: BrokerFinancialCapabilityState::Invalid,
        reason: reason.to_string(),
        provenance_hash: None,
    }
}

fn not_required(kind: BrokerFinancialCapabilityKind, reason: &str) -> BrokerFinancialCapability {
    BrokerFinancialCapability {
        kind,
        state: BrokerFinancialCapabilityState::NotRequired,
        reason: reason.to_string(),
        provenance_hash: None,
    }
}

fn assess_component<'a, T>(
    kind: BrokerFinancialCapabilityKind,
    value: Option<&'a T>,
    validate: impl Fn(&T) -> Result<(), BrokerFinancialTruthError>,
    hash: impl Fn(&'a T) -> &'a str,
) -> BrokerFinancialCapability {
    match value {
        None => missing_cap(kind, "authoritative evidence not supplied"),
        Some(value) => match validate(value) {
            Ok(()) => present(kind, hash(value)),
            Err(error) => invalid_cap(kind, &error.to_string()),
        },
    }
}

fn canonical_asset(value: String) -> Result<String, BrokerFinancialTruthError> {
    let value = value.trim().to_ascii_uppercase();
    if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(invalid("asset must be exactly three ASCII letters"));
    }
    Ok(value)
}

fn positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn nonnegative(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

fn valid_hash(value: &str) -> bool {
    value
        .strip_prefix("fnv64:")
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn weekday_number(value: Weekday) -> u8 {
    match value {
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
        Weekday::Sun => 7,
    }
}

fn invalid(reason: impl Into<String>) -> BrokerFinancialTruthError {
    BrokerFinancialTruthError::Invalid(reason.into())
}

fn missing(reason: impl Into<String>) -> BrokerFinancialTruthError {
    BrokerFinancialTruthError::Missing(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(label: &str) -> EvidenceProvenance {
        EvidenceProvenance::new(
            label,
            1_700_000_000_000,
            format!("fnv64:{:016x}", crate::utils::fnv1a64(label.as_bytes())),
        )
        .unwrap()
    }

    fn binding_hash(label: &str) -> String {
        format!("fnv64:{:016x}", crate::utils::fnv1a64(label.as_bytes()))
    }

    fn contract(quote: &str) -> ExactProtoOaSymbolContract {
        contract_pair("EUR", quote)
    }

    fn contract_pair(base: &str, quote: &str) -> ExactProtoOaSymbolContract {
        ExactProtoOaSymbolContract::new(
            7,
            8,
            "USD",
            1,
            format!("{base}{quote}"),
            4,
            base,
            8,
            quote,
            5,
            4,
            10_000_000,
            100_000,
            1_000_000_000,
            100_000,
            ExactCommissionSchedule {
                commission_type: CommissionTypeExact::QuoteCurrencyPerLot,
                rate: 0.0,
                minimum_per_deal: 0.0,
                minimum_currency: MinimumCommissionCurrency::Quote,
                minimum_asset: quote.to_string(),
            },
            ExactSwapSchedule {
                calculation: SwapCalculationExact::Pips,
                long: 0.0,
                short: 0.0,
                period_hours: 24,
                time_minutes_from_utc_midnight: 0,
                triple_day: Some(3),
                skip_periods: 0,
                charge_at_weekends: false,
            },
            0.0,
            true,
            "UTC",
            vec![ExactTradingInterval {
                start_second_from_sunday: 1,
                end_second_from_sunday: 604_800,
            }],
            vec![],
            provenance("symbol"),
        )
        .unwrap()
    }

    fn conversion_leg(
        base: &str,
        quote: &str,
        values: &[(i64, f64, f64)],
    ) -> SynchronizedConversionLeg {
        let contract = contract_pair(base, quote);
        SynchronizedConversionLeg::new(&contract, quotes(&format!("{base}{quote}"), values))
            .unwrap()
    }

    fn with_costs(
        mut contract: ExactProtoOaSymbolContract,
        commission_type: CommissionTypeExact,
        commission_rate: f64,
        swap_long: f64,
        fee_rate: f64,
    ) -> ExactProtoOaSymbolContract {
        contract.commission.commission_type = commission_type;
        contract.commission.rate = commission_rate;
        contract.swap.long = swap_long;
        contract.pnl_conversion_fee_rate = fee_rate;
        contract.content_hash = contract.recomputed_hash().unwrap();
        contract.validate().unwrap();
        contract
    }

    fn quotes(symbol: &str, values: &[(i64, f64, f64)]) -> SynchronizedBidAsk {
        SynchronizedBidAsk::new(
            1,
            symbol,
            values
                .iter()
                .map(|&(timestamp_ms, bid, ask)| SynchronizedQuote {
                    timestamp_ms,
                    bid,
                    ask,
                })
                .collect(),
            format!(
                "fnv64:{:016x}",
                crate::utils::fnv1a64(format!("{symbol}-bid").as_bytes())
            ),
            format!(
                "fnv64:{:016x}",
                crate::utils::fnv1a64(format!("{symbol}-ask").as_bytes())
            ),
            provenance(symbol),
        )
        .unwrap()
    }

    #[test]
    fn symbol_contract_pins_units_and_round_trips_volume() {
        let contract = contract("USD");
        assert_eq!(contract.pip_size, 0.0001);
        assert_eq!(contract.contract_units_per_lot, 100_000.0);
        assert_eq!(contract.lots_to_wire_volume(0.01).unwrap(), 100_000);
        assert_eq!(contract.wire_volume_to_lots(100_000).unwrap(), 0.01);
        assert!(contract.lots_to_wire_volume(0.015).is_err());
        assert!(contract.wire_volume_to_lots(1_000_100_000).is_err());
    }

    #[test]
    fn synchronized_quotes_fail_closed_on_bad_geometry_or_order() {
        assert!(
            SynchronizedBidAsk::new(
                1,
                "EURUSD",
                vec![SynchronizedQuote {
                    timestamp_ms: 10,
                    bid: 1.2,
                    ask: 1.1
                }],
                "fnv64:0000000000000001",
                "fnv64:0000000000000002",
                provenance("crossed"),
            )
            .is_err()
        );
        assert!(
            SynchronizedBidAsk::new(
                1,
                "EURUSD",
                vec![
                    SynchronizedQuote {
                        timestamp_ms: 10,
                        bid: 1.0,
                        ask: 1.1
                    },
                    SynchronizedQuote {
                        timestamp_ms: 10,
                        bid: 1.0,
                        ask: 1.1
                    },
                ],
                "fnv64:0000000000000001",
                "fnv64:0000000000000002",
                provenance("duplicate"),
            )
            .is_err()
        );
    }

    #[test]
    fn execution_uses_ask_bid_for_long_and_bid_ask_for_short() {
        let contract = contract("USD");
        let quotes = quotes(
            "EURUSD",
            &[(1_000, 1.1000, 1.1002), (2_000, 1.1010, 1.1013)],
        );
        let conversions = ConversionBook::new("USD", vec![]).unwrap();
        let base = BrokerExactTradeRequest {
            direction: PositionDirection::Long,
            lots: 0.01,
            entry_timestamp_ms: 1_000,
            exit_timestamp_ms: 2_000,
            stop_loss: None,
            take_profit: None,
            adverse_slippage_price: 0.0,
        };
        let long = evaluate_broker_exact_trade(&contract, &quotes, &conversions, &base).unwrap();
        assert_eq!(long.entry_price, 1.1002);
        assert_eq!(long.exit_price, 1.1010);
        let short = evaluate_broker_exact_trade(
            &contract,
            &quotes,
            &conversions,
            &BrokerExactTradeRequest {
                direction: PositionDirection::Short,
                ..base
            },
        )
        .unwrap();
        assert_eq!(short.entry_price, 1.1000);
        assert_eq!(short.exit_price, 1.1013);
        assert!(long.net_pnl_account > 0.0);
        assert!(short.net_pnl_account < 0.0);
    }

    #[test]
    fn variable_spread_and_adverse_slippage_are_applied_once() {
        let contract = contract("USD");
        let conversions = ConversionBook::new("USD", vec![]).unwrap();
        let request = BrokerExactTradeRequest {
            direction: PositionDirection::Long,
            lots: 0.01,
            entry_timestamp_ms: 1_000,
            exit_timestamp_ms: 2_000,
            stop_loss: None,
            take_profit: None,
            adverse_slippage_price: 0.0001,
        };
        let narrow = quotes("EURUSD", &[(1_000, 1.0, 1.0001), (2_000, 1.0020, 1.0021)]);
        let wide = quotes("EURUSD", &[(1_000, 1.0, 1.0005), (2_000, 1.0015, 1.0021)]);
        let a = evaluate_broker_exact_trade(&contract, &narrow, &conversions, &request).unwrap();
        let b = evaluate_broker_exact_trade(&contract, &wide, &conversions, &request).unwrap();
        assert!((a.entry_price - 1.0002).abs() < 1e-12);
        assert!((a.exit_price - 1.0019).abs() < 1e-12);
        assert!(a.net_pnl_account > b.net_pnl_account);
    }

    #[test]
    fn commission_swap_and_conversion_fee_are_applied_exactly_once() {
        let contract = with_costs(
            contract("GBP"),
            CommissionTypeExact::QuoteCurrencyPerLot,
            2.0,
            -1.0,
            0.01,
        );
        let entry = Utc
            .with_ymd_and_hms(2026, 9, 14, 23, 0, 0)
            .unwrap()
            .timestamp_millis();
        let exit = Utc
            .with_ymd_and_hms(2026, 9, 15, 1, 0, 0)
            .unwrap()
            .timestamp_millis();
        let quotes = quotes("EURGBP", &[(entry, 0.85, 0.85), (exit, 0.851, 0.851)]);
        let conversions = ConversionBook::new(
            "USD",
            vec![conversion_leg(
                "GBP",
                "USD",
                &[(entry, 1.30, 1.30), (exit, 1.30, 1.30)],
            )],
        )
        .unwrap();
        let result = evaluate_broker_exact_trade(
            &contract,
            &quotes,
            &conversions,
            &BrokerExactTradeRequest {
                direction: PositionDirection::Long,
                lots: 1.0,
                entry_timestamp_ms: entry,
                exit_timestamp_ms: exit,
                stop_loss: None,
                take_profit: None,
                adverse_slippage_price: 0.0,
            },
        )
        .unwrap();
        assert!((result.gross_pnl_quote - 100.0).abs() < 1e-8);
        assert!((result.commission_account - 5.2).abs() < 1e-12);
        assert!((result.swap_account + 13.0).abs() < 1e-12);
        assert!((result.conversion_fee_account - 1.3).abs() < 1e-10);
        assert!((result.net_pnl_account - 110.5).abs() < 1e-8);
    }

    #[test]
    fn identity_currency_does_not_charge_pnl_conversion_fee() {
        let contract = with_costs(
            contract("USD"),
            CommissionTypeExact::QuoteCurrencyPerLot,
            0.0,
            0.0,
            0.01,
        );
        let result = evaluate_broker_exact_trade(
            &contract,
            &quotes("EURUSD", &[(1_000, 1.0, 1.0), (2_000, 1.001, 1.001)]),
            &ConversionBook::new("USD", vec![]).unwrap(),
            &BrokerExactTradeRequest {
                direction: PositionDirection::Long,
                lots: 1.0,
                entry_timestamp_ms: 1_000,
                exit_timestamp_ms: 2_000,
                stop_loss: None,
                take_profit: None,
                adverse_slippage_price: 0.0,
            },
        )
        .unwrap();
        assert_eq!(result.conversion_fee_account, 0.0);
    }

    #[test]
    fn evaluator_rejects_wrong_account_currency() {
        let error = evaluate_broker_exact_trade(
            &contract("USD"),
            &quotes("EURUSD", &[(1_000, 1.0, 1.0), (2_000, 1.001, 1.001)]),
            &ConversionBook::new("GBP", vec![]).unwrap(),
            &BrokerExactTradeRequest {
                direction: PositionDirection::Long,
                lots: 1.0,
                entry_timestamp_ms: 1_000,
                exit_timestamp_ms: 2_000,
                stop_loss: None,
                take_profit: None,
                adverse_slippage_price: 0.0,
            },
        )
        .expect_err("account-currency mismatch must fail closed");
        assert!(error.to_string().contains("account currency"));
    }

    #[test]
    fn usd_per_million_commission_charges_each_execution_deal_once() {
        let contract = with_costs(
            contract("USD"),
            CommissionTypeExact::UsdPerMillionUsd,
            45.0,
            0.0,
            0.0,
        );
        let quotes = quotes("EURUSD", &[(1_000, 1.10, 1.10), (2_000, 1.10, 1.10)]);
        let result = evaluate_broker_exact_trade(
            &contract,
            &quotes,
            &ConversionBook::new("USD", vec![]).unwrap(),
            &BrokerExactTradeRequest {
                direction: PositionDirection::Long,
                lots: 1.0,
                entry_timestamp_ms: 1_000,
                exit_timestamp_ms: 2_000,
                stop_loss: None,
                take_profit: None,
                adverse_slippage_price: 0.0,
            },
        )
        .unwrap();
        assert!((result.commission_account - 9.9).abs() < 1e-12);
    }

    #[test]
    fn stop_uses_liquidation_side_and_never_future_quote() {
        let contract = contract("USD");
        let quotes = quotes(
            "EURUSD",
            &[
                (1_000, 1.1000, 1.1002),
                (1_500, 1.0990, 1.0992),
                (2_000, 1.2000, 1.2002),
            ],
        );
        let result = evaluate_broker_exact_trade(
            &contract,
            &quotes,
            &ConversionBook::new("USD", vec![]).unwrap(),
            &BrokerExactTradeRequest {
                direction: PositionDirection::Long,
                lots: 0.01,
                entry_timestamp_ms: 1_000,
                exit_timestamp_ms: 2_000,
                stop_loss: Some(1.0995),
                take_profit: None,
                adverse_slippage_price: 0.0,
            },
        )
        .unwrap();
        assert_eq!(result.exit_timestamp_ms, 1_500);
        assert_eq!(result.exit_price, 1.0990);
        assert!(quotes.at(1_250).is_err());
    }

    #[test]
    fn direct_and_inverse_conversion_are_side_adverse() {
        let direct = conversion_leg("GBP", "USD", &[(1_000, 1.20, 1.21)]);
        let book = ConversionBook::new("USD", vec![direct]).unwrap();
        assert_eq!(book.convert_to_account(100.0, "GBP", 1_000).unwrap(), 120.0);
        assert_eq!(
            book.convert_to_account(-100.0, "GBP", 1_000).unwrap(),
            -121.0
        );
        let inverse = conversion_leg("USD", "GBP", &[(1_000, 0.82, 0.83)]);
        let book = ConversionBook::new("USD", vec![inverse]).unwrap();
        assert!((book.convert_to_account(83.0, "GBP", 1_000).unwrap() - 100.0).abs() < 1e-12);
        assert!((book.convert_to_account(-82.0, "GBP", 1_000).unwrap() + 100.0).abs() < 1e-12);
        assert!(book.convert_to_account(1.0, "GBP", 999).is_err());
    }

    #[test]
    fn historical_requirement_matrix_skips_identity_conversion_leg() {
        let contract = contract("USD");
        let evidence = HistoricalBrokerTruthEvidence::new(
            contract,
            quotes("EURUSD", &[(1_000, 1.0, 1.1)]),
            ConversionBook::new("USD", vec![]).unwrap(),
            10_000.0,
            binding_hash("risk"),
            binding_hash("strategy"),
            binding_hash("slippage"),
        )
        .unwrap();
        let report = BrokerFinancialTruthReport::historical(&evidence).unwrap();
        report.require_historical().unwrap();
        assert!(!report.live_broker_ready);
        assert_eq!(
            report.capabilities[1].state,
            BrokerFinancialCapabilityState::NotRequired
        );
    }

    #[test]
    fn non_account_commission_currency_requires_synchronized_conversion() {
        let mut contract = contract("GBP");
        contract.account_asset_id = 5;
        contract.account_currency = "GBP".to_string();
        contract.commission.commission_type = CommissionTypeExact::UsdPerLot;
        contract.commission.rate = 2.0;
        contract.content_hash = contract.recomputed_hash().unwrap();
        contract.validate().unwrap();
        let bid_ask = quotes("EURGBP", &[(1_000, 0.85, 0.8502)]);
        assert!(
            HistoricalBrokerTruthEvidence::new(
                contract.clone(),
                bid_ask.clone(),
                ConversionBook::new("GBP", vec![]).unwrap(),
                10_000.0,
                binding_hash("risk"),
                binding_hash("strategy"),
                binding_hash("slippage"),
            )
            .is_err()
        );
        let evidence = HistoricalBrokerTruthEvidence::new(
            contract,
            bid_ask,
            ConversionBook::new(
                "GBP",
                vec![conversion_leg("USD", "GBP", &[(1_000, 0.78, 0.79)])],
            )
            .unwrap(),
            10_000.0,
            binding_hash("risk"),
            binding_hash("strategy"),
            binding_hash("slippage"),
        )
        .unwrap();
        let report = BrokerFinancialTruthReport::historical(&evidence).unwrap();
        assert_eq!(
            report.capabilities[1].state,
            BrokerFinancialCapabilityState::Present
        );
    }

    #[test]
    fn historical_capability_fails_for_each_missing_or_invalid_component() {
        let contract = contract("GBP");
        let bid_ask = quotes("EURGBP", &[(1_000, 0.85, 0.8502)]);
        let conversion = ConversionBook::new(
            "USD",
            vec![conversion_leg("GBP", "USD", &[(1_000, 1.30, 1.31)])],
        )
        .unwrap();
        let assess = |contract, bid_ask, conversion| {
            BrokerFinancialTruthReport::assess_historical(
                contract,
                bid_ask,
                conversion,
                "USD",
                10_000.0,
                &binding_hash("risk"),
                &binding_hash("strategy"),
                &binding_hash("slippage"),
            )
        };
        assert!(
            assess(Some(&contract), Some(&bid_ask), Some(&conversion))
                .require_historical()
                .is_ok()
        );
        assert!(
            assess(None, Some(&bid_ask), Some(&conversion))
                .missing_capabilities()
                .contains(&BrokerFinancialCapabilityKind::ExactProtoOaSymbolContract)
        );
        assert!(
            assess(Some(&contract), None, Some(&conversion))
                .missing_capabilities()
                .contains(&BrokerFinancialCapabilityKind::SynchronizedHistoricalBidAsk)
        );
        assert!(
            assess(Some(&contract), Some(&bid_ask), None)
                .missing_capabilities()
                .contains(&BrokerFinancialCapabilityKind::SynchronizedConversionLegs)
        );
        let mut invalid_quotes = bid_ask.clone();
        invalid_quotes.content_hash.push('0');
        let report = assess(Some(&contract), Some(&invalid_quotes), Some(&conversion));
        assert_eq!(
            report
                .capabilities
                .iter()
                .find(|value| value.kind
                    == BrokerFinancialCapabilityKind::SynchronizedHistoricalBidAsk)
                .unwrap()
                .state,
            BrokerFinancialCapabilityState::Invalid
        );

        let wrong_symbol_quotes = quotes("USDJPY", &[(1_000, 150.0, 150.1)]);
        let report = assess(
            Some(&contract),
            Some(&wrong_symbol_quotes),
            Some(&conversion),
        );
        assert!(!report.broker_historical_ready);
        assert!(report.validate().is_ok());
        assert!(!report.missing_capabilities().is_empty());

        let wrong_time_conversion = ConversionBook::new(
            "USD",
            vec![conversion_leg("GBP", "USD", &[(2_000, 1.30, 1.31)])],
        )
        .unwrap();
        let report = assess(
            Some(&contract),
            Some(&bid_ask),
            Some(&wrong_time_conversion),
        );
        assert_eq!(
            report
                .capabilities
                .iter()
                .find(
                    |value| value.kind == BrokerFinancialCapabilityKind::SynchronizedConversionLegs
                )
                .unwrap()
                .state,
            BrokerFinancialCapabilityState::Invalid
        );

        let wrong_account_conversion = ConversionBook::new("EUR", vec![]).unwrap();
        let report = assess(
            Some(&contract),
            Some(&bid_ask),
            Some(&wrong_account_conversion),
        );
        assert!(!report.broker_historical_ready);
        assert!(report.validate().is_ok());

        let mut malformed_contract = contract.clone();
        malformed_contract.provenance.source_hash = "not-a-hash".to_string();
        let report = assess(Some(&malformed_contract), Some(&bid_ask), Some(&conversion));
        assert_eq!(
            report
                .capabilities
                .iter()
                .find(
                    |value| value.kind == BrokerFinancialCapabilityKind::ExactProtoOaSymbolContract
                )
                .unwrap()
                .state,
            BrokerFinancialCapabilityState::Invalid
        );
    }

    #[test]
    fn historical_bindings_must_be_canonical_hashes() {
        assert!(
            HistoricalBrokerTruthEvidence::new(
                contract("USD"),
                quotes("EURUSD", &[(1_000, 1.0, 1.1)]),
                ConversionBook::new("USD", vec![]).unwrap(),
                10_000.0,
                "risk",
                binding_hash("strategy"),
                binding_hash("slippage"),
            )
            .is_err()
        );
    }

    #[test]
    fn present_capability_requires_provenance_hash() {
        let evidence = HistoricalBrokerTruthEvidence::new(
            contract("USD"),
            quotes("EURUSD", &[(1_000, 1.0, 1.1)]),
            ConversionBook::new("USD", vec![]).unwrap(),
            10_000.0,
            binding_hash("risk"),
            binding_hash("strategy"),
            binding_hash("slippage"),
        )
        .unwrap();
        let mut report = BrokerFinancialTruthReport::historical(&evidence).unwrap();
        report.capabilities[0].provenance_hash = None;
        report.truth_hash = report.recomputed_hash().unwrap();
        assert!(report.validate().is_err());
    }

    #[test]
    fn strategy_or_cost_tampering_invalidates_historical_binding() {
        let mut evidence = HistoricalBrokerTruthEvidence::new(
            contract("USD"),
            quotes("EURUSD", &[(1_000, 1.0, 1.1)]),
            ConversionBook::new("USD", vec![]).unwrap(),
            10_000.0,
            binding_hash("risk"),
            binding_hash("strategy-a"),
            binding_hash("slippage"),
        )
        .unwrap();
        evidence.strategy_hash = binding_hash("strategy-b");
        assert!(evidence.validate().is_err());
        let mut evidence = HistoricalBrokerTruthEvidence::new(
            contract("USD"),
            quotes("EURUSD", &[(1_000, 1.0, 1.1)]),
            ConversionBook::new("USD", vec![]).unwrap(),
            10_000.0,
            binding_hash("risk"),
            binding_hash("strategy-a"),
            binding_hash("slippage"),
        )
        .unwrap();
        evidence.symbol_contract.commission.rate = 7.0;
        assert!(evidence.validate().is_err());
    }

    #[test]
    fn typed_historical_storage_round_trips_and_rejects_tampering() {
        let evidence = HistoricalBrokerTruthEvidence::new(
            contract("USD"),
            quotes("EURUSD", &[(1_000, 1.0, 1.1)]),
            ConversionBook::new("USD", vec![]).unwrap(),
            10_000.0,
            binding_hash("risk"),
            binding_hash("strategy"),
            binding_hash("slippage"),
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "neoethos-broker-truth-{}-{}.json",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        save_historical_broker_truth(&path, &evidence).unwrap();
        assert_eq!(load_historical_broker_truth(&path).unwrap(), evidence);
        let mut tampered: HistoricalBrokerTruthEvidence =
            crate::storage::json::read_json(&path, "test broker truth").unwrap();
        tampered.initial_equity += 1.0;
        crate::storage::json::write_json_atomic(&path, &tampered).unwrap();
        assert!(load_historical_broker_truth(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn live_capability_requires_fresh_broker_pnl_and_reconciled_close() {
        let historical = HistoricalBrokerTruthEvidence::new(
            contract("USD"),
            quotes("EURUSD", &[(1_000, 1.0, 1.1)]),
            ConversionBook::new("USD", vec![]).unwrap(),
            10_000.0,
            binding_hash("risk"),
            binding_hash("strategy"),
            binding_hash("slippage"),
        )
        .unwrap();
        let captured = 1_700_000_000_000;
        let positions = BrokerPositionFinancialSnapshot::new(
            7,
            "USD",
            captured,
            60_000,
            vec![BrokerPositionFinancialRow {
                position_id: 9,
                symbol_id: 1,
                direction: PositionDirection::Long,
                volume_units: 1_000.0,
                entry_price: 1.1,
                gross_unrealized_pnl_account: 10.0,
                net_unrealized_pnl_account: 9.0,
                swap_account: Some(0.0),
                commission_account: Some(-1.0),
            }],
            EvidenceProvenance::new("broker-pnl", captured, "fnv64:0000000000000001").unwrap(),
        )
        .unwrap();
        let reconciliation = reconcile_close_deals(
            ExpectedLocalClose {
                account_id: 7,
                symbol_id: 1,
                position_id: 10,
                direction: PositionDirection::Long,
                expected_volume_units: 1_000.0,
                local_estimated_pnl_account: None,
            },
            vec![BrokerCloseDeal {
                deal_id: 11,
                order_id: 12,
                position_id: 10,
                symbol_id: 1,
                direction: PositionDirection::Short,
                filled_volume_units: 1_000.0,
                execution_timestamp_ms: captured,
                gross_profit_account: 10.0,
                commission_account: -1.0,
                swap_account: 0.0,
                conversion_fee_account: 0.0,
                net_profit_account: 9.0,
            }],
            EvidenceProvenance::new("broker-deals", captured, "fnv64:0000000000000002").unwrap(),
        )
        .unwrap();
        let live =
            LiveBrokerTruthEvidence::new(historical, positions, vec![reconciliation]).unwrap();
        let report = BrokerFinancialTruthReport::live(&live).unwrap();
        assert!(report.require_live_at(captured + 60_000).is_ok());
        assert!(report.require_live_at(captured + 60_001).is_err());

        let wrong_symbol = reconcile_close_deals(
            ExpectedLocalClose {
                account_id: 7,
                symbol_id: 2,
                position_id: 10,
                direction: PositionDirection::Long,
                expected_volume_units: 1_000.0,
                local_estimated_pnl_account: None,
            },
            vec![BrokerCloseDeal {
                deal_id: 11,
                order_id: 12,
                position_id: 10,
                symbol_id: 2,
                direction: PositionDirection::Short,
                filled_volume_units: 1_000.0,
                execution_timestamp_ms: captured,
                gross_profit_account: 10.0,
                commission_account: -1.0,
                swap_account: 0.0,
                conversion_fee_account: 0.0,
                net_profit_account: 9.0,
            }],
            EvidenceProvenance::new("other-symbol-deals", captured, "fnv64:0000000000000003")
                .unwrap(),
        )
        .unwrap();
        assert!(
            LiveBrokerTruthEvidence::new(live.historical, live.positions, vec![wrong_symbol])
                .is_err()
        );
    }

    #[test]
    fn synthetic_mode_never_claims_broker_or_live_readiness() {
        let report = BrokerFinancialTruthReport::mechanical("explicit_spread_pips=1.0");
        assert_eq!(report.mode, FinancialTruthMode::Mechanical);
        assert!(report.require_historical().is_err());
        assert!(report.require_live().is_err());
        assert!(report.validate().is_ok());

        let close_only = BrokerFinancialTruthReport::mechanical("close_only_vortex_ohlcv");
        assert!(close_only.require_historical().is_err());
        assert!(
            close_only
                .missing_capabilities()
                .contains(&BrokerFinancialCapabilityKind::SynchronizedHistoricalBidAsk)
        );
    }

    #[test]
    fn reconciliation_is_idempotent_partial_aware_and_broker_authoritative() {
        let expected = ExpectedLocalClose {
            account_id: 7,
            symbol_id: 1,
            position_id: 9,
            direction: PositionDirection::Long,
            expected_volume_units: 1_000.0,
            local_estimated_pnl_account: Some(999.0),
        };
        let deal = BrokerCloseDeal {
            deal_id: 1,
            order_id: 2,
            position_id: 9,
            symbol_id: 1,
            direction: PositionDirection::Short,
            filled_volume_units: 400.0,
            execution_timestamp_ms: 1_000,
            gross_profit_account: 10.0,
            commission_account: -1.0,
            swap_account: 0.0,
            conversion_fee_account: 0.0,
            net_profit_account: 9.0,
        };
        let partial = reconcile_close_deals(
            expected.clone(),
            vec![deal.clone(), deal.clone()],
            provenance("deals"),
        )
        .unwrap();
        assert_eq!(partial.state, ReconciliationState::Partial);
        assert_eq!(partial.unique_deals.len(), 1);
        let mut rest = deal.clone();
        rest.deal_id = 3;
        rest.filled_volume_units = 600.0;
        rest.net_profit_account = 4.0;
        let complete =
            reconcile_close_deals(expected, vec![deal, rest], provenance("deals")).unwrap();
        assert_eq!(complete.state, ReconciliationState::Reconciled);
        assert_eq!(complete.broker_net_profit_account, Some(13.0));

        let unresolved = reconcile_close_deals(
            complete.expected.clone(),
            vec![],
            provenance("missing-deals"),
        )
        .unwrap();
        assert_eq!(unresolved.state, ReconciliationState::Unresolved);
        assert_eq!(unresolved.broker_net_profit_account, None);

        let mut mismatched = complete.unique_deals[0].clone();
        mismatched.symbol_id += 1;
        assert!(
            reconcile_close_deals(
                complete.expected.clone(),
                vec![mismatched],
                provenance("mismatched-deal"),
            )
            .is_err()
        );
    }

    #[test]
    fn tampering_invalidates_component_and_report_hashes() {
        let mut contract = contract("USD");
        contract.pip_size = 0.01;
        assert!(contract.validate().is_err());
        let mut report = BrokerFinancialTruthReport::mechanical("synthetic");
        report.broker_historical_ready = true;
        assert!(report.validate().is_err());
    }
}
