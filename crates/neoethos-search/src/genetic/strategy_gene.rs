use super::runtime_overrides::current_strategy_evaluation_runtime_overrides;
use anyhow::{Result, bail};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Gene {
    pub indices: Vec<usize>,
    pub weights: Vec<f32>,
    pub long_threshold: f32,
    pub short_threshold: f32,
    pub fitness: f64,
    pub sharpe_ratio: f64,
    pub win_rate: f64,
    pub max_drawdown: f64,
    pub profit_factor: f64,
    pub expectancy: f64,
    pub trades_count: usize,
    pub generation: usize,
    pub strategy_id: String,
    pub use_ob: bool,
    pub use_fvg: bool,
    pub use_liq_sweep: bool,
    pub mtf_confirmation: bool,
    pub use_premium_discount: bool,
    pub use_inducement: bool,
    #[serde(default)]
    pub use_bos: bool,
    #[serde(default)]
    pub use_choch: bool,
    #[serde(default)]
    pub use_eqh: bool,
    #[serde(default)]
    pub use_eql: bool,
    #[serde(default)]
    pub use_displacement: bool,
    pub tp_pips: f64,
    pub sl_pips: f64,
    pub slice_pass_rate: f64,
    pub consistency: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FilteringConfig {
    pub max_dd: f64,
    pub min_profit: f64,
    pub min_trades: f64,
    pub min_sharpe: f64,
    pub min_win_rate: f64,
    pub min_profit_factor: f64,
    pub min_positive_months: usize,
    pub min_trades_per_month: f64,
    pub min_monthly_return_pct: f64,
    pub log_trades: bool,
    pub trade_log_max: usize,
    pub opportunistic_enabled: bool,
    pub use_opportunistic_candidates: bool,
    pub opportunistic_min_positive_months: usize,
    pub opportunistic_min_trades_per_month: f64,
    pub opportunistic_min_trade_return_pct: f64,
    pub opportunistic_max_dd: f64,
    pub anomaly_guard: bool,
    pub elite_mode: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketCostProfile {
    pub symbol: String,
    pub account_currency: String,
    pub pip_value: f64,
    pub pip_value_per_lot: f64,
    pub spread_pips: f64,
    pub commission_per_trade: f64,
    /// **Phase C (2026-05-28)** — broker-supplied daily SWAP charges
    /// and cross-currency conversion fee. `0.0` means "no charge
    /// known"; non-zero comes from the broker's `ProtoOASymbol`
    /// projection (`SymbolFinancials::daily_swap_long/short`,
    /// `pnl_conversion_fee_rate`). The CPU + GPU eval kernels
    /// subtract `swap_pips × overnight_days × pip_value_per_lot` at
    /// each trade exit and subtract `abs(pnl) × pnl_conversion_fee_rate`
    /// once. Defaulting to 0 is **deliberate fail-safe** so a
    /// missing-broker-data run still produces a backtest; the GA
    /// fitness function additionally penalises strategies that lean
    /// heavily on overnight positions when these costs are unknown
    /// (TODO follow-up — currently a no-op).
    pub swap_long_pips_per_day: f64,
    pub swap_short_pips_per_day: f64,
    pub pnl_conversion_fee_rate: f64,
}

impl Default for FilteringConfig {
    fn default() -> Self {
        Self {
            max_dd: 0.15,
            min_profit: 10.0,
            min_trades: 10.0,
            min_sharpe: 0.3,
            min_win_rate: 0.50,
            // 2026-05-26 operator directive (dual-mode product): canonical
            // value across the workspace. Previously 1.05 here, 1.5 in
            // quality.rs, 1.2 in gauntlet.rs (now deleted). 1.2 matches the
            // FTMO industry baseline and avoids a divergent default per code
            // path. If you change this, also change the matching default in
            // `quality.rs::QualityRuntimeOverrides` / similar.
            min_profit_factor: 1.2,
            min_positive_months: 0,
            min_trades_per_month: 0.0,
            min_monthly_return_pct: 0.0,
            log_trades: false,
            trade_log_max: 20,
            opportunistic_enabled: false,
            use_opportunistic_candidates: false,
            opportunistic_min_positive_months: 0,
            opportunistic_min_trades_per_month: 0.0,
            opportunistic_min_trade_return_pct: 0.0,
            opportunistic_max_dd: 1.0,
            anomaly_guard: true,
            elite_mode: false,
        }
    }
}

fn normalized_symbol(symbol: &str) -> String {
    symbol
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

fn split_symbol_parts(symbol: &str) -> Option<(String, String)> {
    let normalized = normalized_symbol(symbol);
    if normalized.len() >= 6 {
        Some((normalized[..3].to_string(), normalized[3..6].to_string()))
    } else {
        None
    }
}

fn symbol_kind(symbol: &str) -> &'static str {
    let normalized = normalized_symbol(symbol);
    if normalized.starts_with("XAU") || normalized.starts_with("XAG") {
        return "metal";
    }
    if normalized.contains("BTC") || normalized.contains("ETH") || normalized.contains("LTC") {
        return "crypto";
    }
    if split_symbol_parts(&normalized).is_some() {
        return "fx";
    }
    "other"
}

/// Symbol-aware canonical pip size. Empty / unparseable symbol returns
/// NaN so downstream pip math collapses and the fitness guard rejects
/// the strategy (GROUP C remediation).
///
/// Exposed as `pub(crate)` for use in `search_engine.rs::resolve_stop_target_arrays`
/// (F-761 closure — replaces the hardcoded `0.0001` EURUSD-pip fallback
/// with this symbol-aware lookup).
pub(crate) fn default_pip_size(symbol: &str) -> f64 {
    // GROUP C remediation: empty symbol → NaN sentinel so downstream
    // pip math collapses and the fitness guard rejects the strategy.
    if symbol.trim().is_empty() {
        return f64::NAN;
    }
    match symbol_kind(symbol) {
        "metal" => 0.01,
        "crypto" => 1.0,
        "fx" => match split_symbol_parts(symbol) {
            Some((_base, quote)) if quote == "JPY" => 0.01,
            _ => 0.0001,
        },
        _ => 0.0001,
    }
}

fn default_contract_size(symbol: &str) -> f64 {
    // GROUP C remediation: empty symbol → NaN sentinel (see default_pip_size).
    if symbol.trim().is_empty() {
        return f64::NAN;
    }
    match symbol_kind(symbol) {
        "metal" => match split_symbol_parts(symbol) {
            Some((base, _quote)) if base == "XAG" => 5_000.0,
            Some((base, _quote)) if base == "XAU" => 100.0,
            _ => 100.0,
        },
        "crypto" => 1.0,
        "fx" => 100_000.0,
        _ => 1.0,
    }
}

/// Convert one pip on `symbol` into account-currency units per standard lot.
///
/// `quote_to_account_rate` is the live `quote_currency → account_currency` FX
/// rate (e.g. for EURGBP on a USD account this is the GBP→USD rate ≈ 1.27).
/// When `None`, we fall back to a price-hint-only model that is correct for
/// pairs where account currency is base or quote, and approximate (but
/// flagged via `tracing::warn`) for cross pairs. Set the env override
/// `NEOETHOS_BOT_PROP_PIP_VALUE_PER_LOT` to bypass this entirely when the
/// caller already knows the value (e.g. straight from cTrader metadata).
fn estimate_pip_value_per_lot(
    symbol: &str,
    account_currency: &str,
    price_hint: Option<f64>,
    quote_to_account_rate: Option<f64>,
) -> f64 {
    let pip_size = default_pip_size(symbol);
    let contract_size = default_contract_size(symbol);
    let pip_value_quote = pip_size * contract_size;
    let account_currency = account_currency.trim().to_ascii_uppercase();
    let normalized = normalized_symbol(symbol);
    let price = price_hint.filter(|value| value.is_finite() && *value > 0.0);
    let conv_rate = quote_to_account_rate.filter(|v| v.is_finite() && *v > 0.0);

    if let Some((base, quote)) = split_symbol_parts(&normalized) {
        if quote == account_currency {
            // pip is already in account currency
            return pip_value_quote.max(1e-6);
        }
        if base == account_currency {
            // For e.g. USDJPY on a USD account: pip_value_USD = pip_value_JPY / USDJPY
            return price
                .map(|value| pip_value_quote / value.max(1e-9))
                .unwrap_or_else(|| pip_value_quote.max(1e-6));
        }
        // Cross pair (e.g. EURGBP on USD account). Correct formula needs the
        // quote→account FX rate. If supplied, use it.
        if let Some(rate) = conv_rate {
            return (pip_value_quote * rate).max(1e-6);
        }
        // No rate supplied. Strict mode (`NEOETHOS_BOT_REJECT_PIP_FALLBACK=1`)
        // returns NaN so downstream PnL collapses to NaN and the strategy
        // is rejected by the evaluator's existing fitness guard, surfacing
        // the misconfiguration instead of silently shipping wrong sizing.
        // Default mode preserves the previous lenient behaviour but logs
        // at error level (was warn) so the gap is visible in production.
        if reject_cross_pair_fallback() {
            tracing::error!(
                target: "neoethos_search::cost_model",
                symbol,
                account_currency = %account_currency,
                "cross-pair pip_value_per_lot rejected: no quote→account FX rate \
                 and NEOETHOS_BOT_REJECT_PIP_FALLBACK=1; set NEOETHOS_BOT_PROP_PIP_VALUE_PER_LOT \
                 or supply quote_to_account_rate"
            );
            return f64::NAN;
        }
        tracing::error!(
            target: "neoethos_search::cost_model",
            symbol,
            account_currency = %account_currency,
            "cross-pair pip_value_per_lot fallback (silently wrong) — set \
             NEOETHOS_BOT_PROP_PIP_VALUE_PER_LOT or supply quote_to_account_rate; \
             enable NEOETHOS_BOT_REJECT_PIP_FALLBACK=1 to fail fast"
        );
        return pip_value_quote.max(1e-6);
    }

    pip_value_quote.max(1e-6)
}

fn reject_cross_pair_fallback() -> bool {
    // F-CORE3 closure (2026-05-25): previously read `std::env::var`
    // inline on every fallback call. Now resolved through the typed
    // `CostProfileRuntimeOverrides::reject_pip_fallback` boundary so
    // the env is hit at most once per process (in
    // `StrategyEvaluationRuntimeOverrides::from_env`).
    super::runtime_overrides::current_strategy_evaluation_runtime_overrides()
        .cost_profile
        .reject_pip_fallback
}

pub fn infer_market_cost_profile(
    symbol: &str,
    account_currency: &str,
    price_hint: Option<f64>,
    spread_pips_override: Option<f64>,
    commission_override: Option<f64>,
) -> MarketCostProfile {
    // Cost-profile fallbacks resolve through the typed
    // `CostProfileRuntimeOverrides` boundary so the legacy
    // `NEOETHOS_BOT_PROP_*` env vars are read at most once per process.
    let runtime_overrides = current_strategy_evaluation_runtime_overrides();
    let cost = runtime_overrides.cost_profile;

    // GROUP C remediation (operator directive 2026-05-25 "remove
    // hardcoded values that kill performance"): the previous code
    // silently fell back to "EURUSD" + "USD" when given empty
    // inputs. That hid misconfiguration AND prevented the cost
    // model from ever surfacing the bug. We now propagate the
    // emptiness all the way down and let the downstream pip-math
    // path collapse to NaN — the existing fitness guard rejects
    // NaN PnL, so the strategy is rejected loudly rather than
    // backtested against the wrong symbol. Runtime overrides (set
    // explicitly by the operator) still resolve normally; only the
    // last-resort EURUSD/USD literals are gone.
    let symbol = if symbol.trim().is_empty() {
        match cost.symbol.clone() {
            Some(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => {
                tracing::error!(
                    target: "neoethos_search::cost_model",
                    "infer_market_cost_profile called with empty symbol and no \
                     runtime override; cost profile will be NaN-sentinel and \
                     the strategy will be rejected by the fitness guard. \
                     Bind a real cTrader symbol via for_symbol(...) before backtesting."
                );
                String::new()
            }
        }
    } else {
        symbol.trim().to_string()
    };
    let account_currency = if account_currency.trim().is_empty() {
        match cost.account_currency.clone() {
            Some(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => {
                tracing::error!(
                    target: "neoethos_search::cost_model",
                    "infer_market_cost_profile called with empty account_currency \
                     and no runtime override; cost profile will be NaN-sentinel \
                     and the strategy will be rejected by the fitness guard."
                );
                String::new()
            }
        }
    } else {
        account_currency.trim().to_string()
    };

    // Symbol metadata table — disk first (cTrader-populated /
    // operator-edited), then baked-in defaults for the majors.
    // Replaces the old `default_pip_size` / `default_contract_size`
    // heuristic so JPY pairs and metals get correct pip math without
    // env-var hacks.
    let metadata = neoethos_core::symbol_metadata::resolve(&symbol);

    let pip_value = cost
        .pip_value
        .or_else(|| metadata.as_ref().map(|m| m.pip_size))
        .unwrap_or_else(|| default_pip_size(&symbol));
    let pip_value_per_lot = cost.pip_value_per_lot.unwrap_or_else(|| {
        if let Some(meta) = metadata.as_ref() {
            // Use the metadata-table pip math (knows about JPY, XAU,
            // etc. and applies the right base/quote conversion).
            let live_or_typical = price_hint.or(meta.typical_price);
            let v = meta.pip_value_in_account(
                &account_currency,
                cost.quote_to_account_rate,
                live_or_typical,
            );
            if v.is_finite() && v > 0.0 {
                return v;
            }
            // Cross pair with no conv-rate — fall through to the
            // existing estimator which handles the warn/reject path.
        }
        estimate_pip_value_per_lot(
            &symbol,
            &account_currency,
            price_hint,
            cost.quote_to_account_rate,
        )
    });

    // **F-029 fix (2026-05-25)** — resolved via F-126 (SymbolMetadata
    // gained `typical_spread_pips` + `commission_per_lot`). The
    // resolution order is now:
    //   1. Explicit per-call override (`spread_pips_override` /
    //      `commission_override`) — for unit tests + when the caller
    //      already knows the value.
    //   2. Process-wide runtime override (`cost.spread_pips` /
    //      `cost.commission_per_trade`) — operator's `Settings`.
    //   3. **SymbolMetadata** broker-authoritative value — the new
    //      typed boundary populated by the cTrader connector.
    //   4. Asset-class synthetic default — LAST RESORT, kept only so
    //      that pre-F-126 `data/symbol_metadata.json` files (which
    //      have neither field) keep working. Logs a `tracing::warn!`
    //      so operators see the synthetic-fallback was taken.
    //
    // When `data/symbol_metadata.json` is populated from cTrader
    // (`ProtoOASymbol::spread` + commission schedule), step (4) is
    // never reached. Operator's real-data policy 2026-05-24 is
    // honoured: synthetic only when broker silence + no operator
    // override leaves no other choice.
    // F-301 (2026-05-28): kill the synthetic-spread fallback. The
    // previous code returned a per-asset-class default (1.5 / 2.5 /
    // 8.0 / 1.0 pips) with a tracing::warn — which meant every novel
    // symbol with no broker data silently got fake numbers, and the
    // GA produced backtests using spread that didn't match the live
    // market. The operator's directive 2026-05-24: "ολα τα νουμερα
    // απο τον σερβερ", and that includes spread.
    //
    // Resolution chain unchanged at the top; on miss:
    //   - tracing::error! with the symbol context (operators see the
    //     exact (symbol, asset_class) tuple that needs broker data)
    //   - NaN-sentinel return value. Downstream:
    //     - `BacktestSettings.spread_pips` becomes NaN
    //     - eval kernel's `half_spread_cost = NaN * NaN` = NaN
    //     - entry_px = close + NaN = NaN
    //     - no trades open; sanitize() scrubs metrics to 0
    //     - GA candidate has trade_count=0
    //     - run_discovery_cycle's F-304 preflight catches non-finite
    //       evaluation_spread_pips at config-level if it propagated
    //       through DiscoveryConfig
    //   - operator's `--bootstrap-data` or `data/symbol_metadata.json`
    //     edit resolves the issue
    //
    // Live-trading path is unaffected: live spread comes from the
    // cTrader spot tick, not from this offline-backtest helper.
    let spread_pips = spread_pips_override
        .filter(|value| value.is_finite() && *value >= 0.0)
        .or(cost.spread_pips)
        .or_else(|| metadata.as_ref().and_then(|m| m.typical_spread_pips))
        .unwrap_or_else(|| {
            tracing::error!(
                target: "neoethos_search::cost_model",
                symbol = %symbol,
                asset_class = symbol_kind(&symbol),
                "F-301 fail-loud: no broker spread for this symbol. \
                 SymbolMetadata has no `typical_spread_pips`, no \
                 operator override, and no cost.spread_pips. Synthetic \
                 fallback REMOVED — returning NaN so the eval kernel \
                 zero-trades this candidate instead of silently using \
                 1.5 pips. Action: populate data/symbol_metadata.json \
                 from cTrader ProtoOASymbol.spread (run \
                 --rebuild-symbol-metadata) OR set \
                 settings.risk.backtest_spread_pips in config.yaml."
            );
            f64::NAN
        });
    // Phase D.2e (2026-05-28) — step 3.5 in the resolution chain:
    // when the operator override and SymbolMetadata.commission_per_lot
    // are both unset, derive commission from the broker-supplied
    // `commission_type` + `commission_rate_decimal` (D.2d schema
    // additions) via `commission_per_lot_account_ccy`. Returns the
    // commission in **account currency** so it slots directly into
    // MarketCostProfile.commission_per_trade (which the eval kernel
    // subtracts from PnL in account ccy). Bails to None for:
    //   - type 1/2 USD-denominated on non-USD accounts (need USD→
    //     account rate; deferred to D.2f)
    //   - cross-currency type 3/4 with missing quote_to_account_rate
    // In those cases the synthetic $7/lot fallback is taken with a
    // tracing::warn! — surfaces the gap rather than silently lying.
    let commission_per_trade = commission_override
        .filter(|value| value.is_finite() && *value >= 0.0)
        .or(cost.commission_per_trade)
        .or_else(|| metadata.as_ref().and_then(|m| m.commission_per_lot))
        .or_else(|| {
            metadata.as_ref().and_then(|m| {
                m.commission_per_lot_account_ccy(
                    &account_currency,
                    price_hint.or(m.typical_price),
                    cost.quote_to_account_rate,
                )
            })
        })
        .unwrap_or_else(|| {
            tracing::warn!(
                target: "neoethos_search::cost_model",
                symbol = %symbol,
                account_currency = %account_currency,
                fallback_commission = 7.0,
                "Synthetic commission fallback used (F-029 LAST RESORT): \
                 SymbolMetadata has no `commission_per_lot`, no operator \
                 override, AND the broker-derived `commission_per_lot_account_ccy` \
                 returned None (typically: type 1/2 on non-USD account, or \
                 cross-currency type 3/4 without quote_to_account_rate). \
                 Populate `commission_type`+`commission_rate_decimal` via \
                 --rebuild-symbol-metadata and supply quote_to_account_rate \
                 to silence this."
            );
            7.0
        });

    // **Phase C (2026-05-28)**: pull the broker-supplied swap +
    // conversion fee values from SymbolMetadata. Default to 0.0 when
    // unknown — the eval kernel treats 0.0 as "no charge", matching
    // the pre-Phase-C silent behaviour. The audit (F-CY3-2/3) calls
    // this out as a HIGH-severity gap when the broker data exists
    // but the cost model ignores it; populating these fields from
    // `data/symbol_metadata.json` (which the bridge refreshes from
    // cTrader) lifts the gap.
    let swap_long_pips_per_day = metadata
        .as_ref()
        .and_then(|m| m.daily_swap_long_pips)
        .filter(|v| v.is_finite())
        .unwrap_or(0.0);
    let swap_short_pips_per_day = metadata
        .as_ref()
        .and_then(|m| m.daily_swap_short_pips)
        .filter(|v| v.is_finite())
        .unwrap_or(0.0);
    let pnl_conversion_fee_rate = metadata
        .as_ref()
        .and_then(|m| m.pnl_conversion_fee_rate)
        .filter(|v| v.is_finite() && *v >= 0.0 && *v < 1.0)
        .unwrap_or(0.0);

    MarketCostProfile {
        symbol,
        account_currency,
        pip_value,
        pip_value_per_lot,
        spread_pips,
        commission_per_trade,
        swap_long_pips_per_day,
        swap_short_pips_per_day,
        pnl_conversion_fee_rate,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_strict_discovery_cost_profile_from_metadata(
    symbol: &str,
    account_currency: &str,
    metadata: Option<&neoethos_core::symbol_metadata::SymbolMetadata>,
    spread_override: Option<f64>,
    commission_override: Option<f64>,
    quote_to_account_rate: Option<f64>,
    price_hint: Option<f64>,
    slippage_pips: f64,
) -> Result<MarketCostProfile> {
    let symbol = normalized_symbol(symbol);
    let account_currency = account_currency.trim().to_ascii_uppercase();
    if symbol.is_empty() || account_currency.is_empty() {
        bail!("strict Discovery requires symbol and account currency");
    }
    let metadata = metadata
        .ok_or_else(|| anyhow::anyhow!("missing broker SymbolMetadata for {symbol}"))?;
    if normalized_symbol(&metadata.symbol) != symbol {
        bail!("broker SymbolMetadata symbol does not match {symbol}");
    }
    for (name, value) in [
        ("pip_size", metadata.pip_size),
        ("contract_size", metadata.contract_size),
        ("pip_value_quote", metadata.pip_value_quote),
    ] {
        if !value.is_finite() || value <= 0.0 {
            bail!("invalid broker {name} for {symbol}");
        }
    }
    if !slippage_pips.is_finite() || slippage_pips < 0.0 {
        bail!("invalid configured slippage for {symbol}");
    }
    let market_spread = match spread_override {
        Some(value) if value.is_finite() && value >= 0.0 => value,
        Some(_) => bail!("invalid explicit Discovery spread override for {symbol}"),
        None => metadata
            .typical_spread_pips
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| anyhow::anyhow!("missing broker spread for {symbol}"))?,
    };
    let spread_pips = market_spread + slippage_pips;
    if !spread_pips.is_finite() {
        bail!("invalid effective spread for {symbol}");
    }

    let quote = metadata.quote.trim().to_ascii_uppercase();
    let base = metadata.base.trim().to_ascii_uppercase();
    if quote.is_empty() || base.is_empty() {
        bail!("missing broker base/quote currency for {symbol}");
    }
    let quote_to_account_rate = match quote_to_account_rate {
        Some(value) if value.is_finite() && value > 0.0 => Some(value),
        Some(_) => bail!("invalid quote→account conversion rate for {symbol}"),
        None if quote != account_currency && base != account_currency => {
            bail!("missing quote→account conversion rate for {symbol}")
        }
        None => None,
    };
    let price_hint = price_hint.filter(|value| value.is_finite() && *value > 0.0);
    if base == account_currency && price_hint.is_none() {
        bail!("missing valid price hint for base-currency pair {symbol}");
    }
    let effective_quote_to_account_rate = if quote == account_currency {
        Some(1.0)
    } else if base == account_currency {
        price_hint.map(|price| 1.0 / price)
    } else {
        quote_to_account_rate
    };
    let pip_value_per_lot = metadata.pip_value_in_account(
        &account_currency,
        effective_quote_to_account_rate,
        price_hint,
    );
    if !pip_value_per_lot.is_finite() || pip_value_per_lot <= 0.0 {
        bail!("invalid pip value per lot for {symbol}");
    }

    let commission_per_trade = match commission_override {
        Some(value) if value.is_finite() && value >= 0.0 => value,
        Some(_) => bail!("invalid explicit Discovery commission override for {symbol}"),
        None => metadata
            .commission_per_lot
            .filter(|value| value.is_finite() && *value >= 0.0)
            .and_then(|value| {
                effective_quote_to_account_rate.map(|rate| value * rate)
            })
            .or_else(|| {
                let usd_to_account_rate = if account_currency == "USD" {
                    None
                } else if quote == "USD" {
                    effective_quote_to_account_rate
                } else if base == "USD" {
                    price_hint
                        .zip(effective_quote_to_account_rate)
                        .map(|(price, rate)| price * rate)
                } else {
                    None
                };
                metadata.commission_per_lot_account_ccy_v2(
                    &account_currency,
                    price_hint,
                    effective_quote_to_account_rate,
                    usd_to_account_rate,
                )
            })
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| anyhow::anyhow!("missing or invalid commission truth for {symbol}"))?,
    };
    let swap_long_pips_per_day = metadata
        .daily_swap_long_pips
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow::anyhow!("missing swap_long for {symbol}"))?;
    let swap_short_pips_per_day = metadata
        .daily_swap_short_pips
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow::anyhow!("missing swap_short for {symbol}"))?;
    let pnl_conversion_fee_rate = match metadata.pnl_conversion_fee_rate {
        Some(value) if value.is_finite() && (0.0..1.0).contains(&value) => value,
        Some(_) => bail!("invalid PnL conversion fee for {symbol}"),
        None if quote == account_currency => 0.0,
        None => bail!("missing PnL conversion fee for {symbol}"),
    };

    Ok(MarketCostProfile {
        symbol,
        account_currency,
        pip_value: metadata.pip_size,
        pip_value_per_lot,
        spread_pips,
        commission_per_trade,
        swap_long_pips_per_day,
        swap_short_pips_per_day,
        pnl_conversion_fee_rate,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_strict_discovery_cost_profile(
    symbol: &str,
    account_currency: &str,
    spread_override: Option<f64>,
    commission_override: Option<f64>,
    quote_to_account_rate: Option<f64>,
    price_hint: Option<f64>,
    slippage_pips: f64,
) -> Result<MarketCostProfile> {
    let metadata = neoethos_core::symbol_metadata::resolve(symbol);
    resolve_strict_discovery_cost_profile_from_metadata(
        symbol,
        account_currency,
        metadata.as_ref(),
        spread_override,
        commission_override,
        quote_to_account_rate,
        price_hint,
        slippage_pips,
    )
}

impl Gene {
    pub fn is_anomalous(&self) -> bool {
        let trades = self.trades_count as f64;
        let win_rate = self.win_rate;
        let profit_factor = self.profit_factor;
        let profit = self.fitness; // Using fitness as profit proxy
        let dd = self.max_drawdown;
        let ppt = if trades > 0.0 { profit / trades } else { 0.0 };

        // Anomaly thresholds calibrated for the 4-10%/mo target on a 10y window.
        // At 6%/mo compounded on a $10K base, target equity is ~$11M; profit alone
        // therefore cannot identify overfitting — only impossible *ratios* can.
        // We keep the original ratio gates (DD, win_rate, PF) but raise absolute
        // profit thresholds 50× so genuine target-hitting strategies are not
        // discarded as "too good to be true".
        let min_trades = 120.0;
        let max_dd = 0.0025;
        let min_win_rate = 0.92;
        let min_pf = 12.0;
        let min_profit = 10_000_000.0;
        let max_ppt = 100_000.0;

        let suspicious_combo = trades >= min_trades
            && dd <= max_dd
            && win_rate >= min_win_rate
            && profit_factor >= min_pf
            && profit >= min_profit;

        let suspicious_ppt = trades >= 40.0 && dd <= 0.01 && ppt >= max_ppt;

        let suspicious_ultra =
            trades >= 50.0 && dd <= 0.001 && profit >= 7_500_000.0 && ppt >= 50_000.0;

        let suspicious_low_dd = trades >= 80.0 && dd <= 0.001 && profit >= 2_500_000.0;

        suspicious_combo || suspicious_ppt || suspicious_ultra || suspicious_low_dd
    }

    pub fn passes_filter(&self, cfg: &FilteringConfig) -> bool {
        if self.fitness <= cfg.min_profit {
            return false;
        }
        if self.max_drawdown > cfg.max_dd {
            return false;
        }
        if (self.trades_count as f64) < cfg.min_trades {
            return false;
        }
        if self.sharpe_ratio < cfg.min_sharpe {
            return false;
        }
        if self.win_rate < cfg.min_win_rate {
            return false;
        }
        if self.profit_factor < cfg.min_profit_factor {
            return false;
        }
        if cfg.anomaly_guard && self.is_anomalous() {
            return false;
        }
        true
    }

    pub fn requires_quality_screen(cfg: &FilteringConfig) -> bool {
        cfg.min_positive_months > 0
            || cfg.min_trades_per_month > 0.0
            || cfg.min_monthly_return_pct > 0.0
            || cfg.log_trades
            || (cfg.opportunistic_enabled && cfg.use_opportunistic_candidates)
    }

    pub fn normalize(&mut self, n_indicators: usize, min_indicators: usize) {
        let n_indicators = n_indicators.max(1);
        let min_indicators = min_indicators.clamp(1, n_indicators);

        if self.indices.is_empty() {
            self.indices.push(0);
        }
        if self.weights.len() != self.indices.len() {
            self.weights = vec![1.0; self.indices.len()];
        }

        let mut terms: Vec<(usize, f32)> = self
            .indices
            .iter()
            .copied()
            .zip(self.weights.iter().copied())
            .map(|(idx, weight)| {
                let idx = idx % n_indicators;
                let weight = if weight.is_finite() { weight } else { 0.0 };
                (idx, weight)
            })
            .collect();
        terms.sort_by_key(|(idx, _)| *idx);

        let mut merged: Vec<(usize, f32)> = Vec::with_capacity(terms.len());
        for (idx, weight) in terms {
            if let Some((last_idx, last_weight)) = merged.last_mut()
                && *last_idx == idx
            {
                *last_weight += weight;
                continue;
            }
            merged.push((idx, weight));
        }

        if merged.len() > min_indicators {
            merged.retain(|(_, weight)| weight.abs() > 1e-6);
        }
        if merged.is_empty() {
            merged.push((0, 1.0));
        }

        let mut rng = rand::rng();
        let mut seen: HashSet<usize> = merged.iter().map(|(idx, _)| *idx).collect();
        while merged.len() < min_indicators {
            let idx = rng.random_range(0..n_indicators);
            if seen.insert(idx) {
                merged.push((idx, rng.random_range(0.1..1.0)));
            }
        }
        merged.sort_by_key(|(idx, _)| *idx);

        self.indices = merged.iter().map(|(idx, _)| *idx).collect();
        self.weights = merged
            .iter()
            .map(|(_, weight)| {
                if weight.is_finite() && weight.abs() > 1e-6 {
                    weight.clamp(-5.0, 5.0)
                } else {
                    1.0
                }
            })
            .collect();

        if !self.long_threshold.is_finite() {
            self.long_threshold = 0.25;
        }
        if !self.short_threshold.is_finite() {
            self.short_threshold = -0.25;
        }
        if self.long_threshold <= self.short_threshold {
            let mid = (self.long_threshold + self.short_threshold) * 0.5;
            self.long_threshold = mid + 0.05;
            self.short_threshold = mid - 0.05;
        }
        if !self.tp_pips.is_finite() || self.tp_pips <= 0.0 {
            self.tp_pips = 40.0;
        }
        if !self.sl_pips.is_finite() || self.sl_pips <= 0.0 {
            self.sl_pips = 20.0;
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub genes: Vec<Gene>,
    pub metrics: Vec<[f64; 11]>,
}

#[derive(Debug, Clone)]
pub struct EvaluationConfig {
    pub initial_equity_override: Option<f64>,
    pub symbol: String,
    pub account_currency: String,
    pub max_hold_bars: usize,
    pub trailing_enabled: bool,
    pub trailing_atr_multiplier: f64,
    pub trailing_be_trigger_r: f64,
    pub pip_value: f64,
    pub spread_pips: f64,
    pub commission_per_trade: f64,
    pub pip_value_per_lot: f64,
    pub swap_long_pips_per_day: f64,
    pub swap_short_pips_per_day: f64,
    pub pnl_conversion_fee_rate: f64,
    pub smc_gate_threshold: f32,
    pub smc_weight_ob: f32,
    pub smc_weight_fvg: f32,
    pub smc_weight_liq: f32,
    pub smc_weight_mtf: f32,
    pub smc_weight_premium: f32,
    pub smc_weight_inducement: f32,
    pub smc_weight_bos: f32,
    pub smc_weight_choch: f32,
    pub smc_weight_eqh: f32,
    pub smc_weight_eql: f32,
    pub smc_weight_displacement: f32,
    /// scoring_version 5 (2026-07-02): when `true` the GA evolves under the
    /// Kelly log-growth objective (`scoring::ga_fitness_growth`) instead of
    /// the prop-firm consistency formula. Set by the discovery driver for
    /// `DiscoveryMode::Risky` only; `false` everywhere else (PropFirm/Strict,
    /// tests, parity fixtures) keeps the v4 landscape byte-for-byte.
    pub growth_objective: bool,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        // GROUP C remediation (operator directive 2026-05-25): the
        // previous code synthesized EURUSD/USD cost-profile fields via
        // `infer_market_cost_profile("", "", ...)`. We now emit empty
        // strings + NaN sentinels so callers that use Default::default()
        // WITHOUT then binding via `for_symbol(...)` are caught by the
        // downstream NaN-fitness guard rather than silently backtested
        // against EURUSD/USD math. Production callers MUST use
        // `EvaluationConfig::for_symbol(...)`.
        let smc = current_strategy_evaluation_runtime_overrides().smc_weights;

        Self {
            initial_equity_override: None,
            symbol: String::new(),
            account_currency: String::new(),
            max_hold_bars: 0,
            trailing_enabled: false,
            trailing_atr_multiplier: 1.0,
            trailing_be_trigger_r: 1.0,
            pip_value: f64::NAN,
            spread_pips: f64::NAN,
            commission_per_trade: f64::NAN,
            pip_value_per_lot: f64::NAN,
            swap_long_pips_per_day: 0.0,
            swap_short_pips_per_day: 0.0,
            pnl_conversion_fee_rate: 0.0,
            smc_gate_threshold: smc.gate_threshold,
            smc_weight_ob: smc.w_ob,
            smc_weight_fvg: smc.w_fvg,
            smc_weight_liq: smc.w_liq,
            smc_weight_mtf: smc.w_mtf,
            smc_weight_premium: smc.w_premium,
            smc_weight_inducement: smc.w_inducement,
            smc_weight_bos: smc.w_bos,
            smc_weight_choch: smc.w_choch,
            smc_weight_eqh: smc.w_eqh,
            smc_weight_eql: smc.w_eql,
            smc_weight_displacement: smc.w_displacement,
            growth_objective: false,
        }
    }
}

impl EvaluationConfig {
    pub fn for_symbol(
        symbol: &str,
        account_currency: &str,
        price_hint: Option<f64>,
        spread_pips_override: Option<f64>,
        commission_override: Option<f64>,
    ) -> Self {
        let profile = infer_market_cost_profile(
            symbol,
            account_currency,
            price_hint,
            spread_pips_override,
            commission_override,
        );
        Self::from_market_cost_profile(profile)
    }

    pub fn from_market_cost_profile(profile: MarketCostProfile) -> Self {
        Self {
            symbol: profile.symbol,
            account_currency: profile.account_currency,
            pip_value: profile.pip_value,
            pip_value_per_lot: profile.pip_value_per_lot,
            spread_pips: profile.spread_pips,
            commission_per_trade: profile.commission_per_trade,
            swap_long_pips_per_day: profile.swap_long_pips_per_day,
            swap_short_pips_per_day: profile.swap_short_pips_per_day,
            pnl_conversion_fee_rate: profile.pnl_conversion_fee_rate,
            // 2026-06-06 operator mandate: break-even + trailing is ALWAYS ON in
            // discovery (HARDCODED here — the production EvaluationConfig builder).
            // Both the GA eval (search_engine `b_settings`) and the funnel/prop-firm
            // window gate (`discovery_backtest_settings`) read these fields, so a single
            // hardcode makes the risk model consistent everywhere. RATIONALE: with
            // trailing OFF (the old default), a losing trade ran to the full SL, so
            // drawdown was higher than necessary and candidates failed the prop-firm
            // DD/consistency gate. With BE ON, once a trade reaches +1R the stop moves to
            // ~entry (then trails the extreme), so after 1R the trade can no longer become
            // a loss → lower DD → more strategies clear the prop-firm gate. `Default`
            // keeps these at false/1.0/1.0 for test fixtures; production = always on.
            trailing_enabled: true,
            trailing_be_trigger_r: 1.0, // move to break-even once +1R in profit
            trailing_atr_multiplier: 1.0, // then trail 1×SL behind the running extreme
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict_metadata(
        symbol: &str,
        base: &str,
        quote: &str,
    ) -> neoethos_core::symbol_metadata::SymbolMetadata {
        neoethos_core::symbol_metadata::SymbolMetadata {
            symbol: symbol.to_string(),
            base: base.to_string(),
            quote: quote.to_string(),
            pip_size: 0.0001,
            contract_size: 100_000.0,
            pip_value_quote: 10.0,
            digits: 5,
            min_lot: 0.01,
            max_lot: 100.0,
            lot_step: 0.01,
            typical_price: Some(1.1),
            typical_spread_pips: Some(1.2),
            commission_per_lot: Some(6.0),
            daily_swap_long_pips: Some(0.0),
            daily_swap_short_pips: Some(0.0),
            pnl_conversion_fee_rate: None,
            commission_type: None,
            commission_rate_decimal: None,
        }
    }

    fn strict_profile(
        metadata: Option<&neoethos_core::symbol_metadata::SymbolMetadata>,
        account_currency: &str,
        spread: Option<f64>,
        commission: Option<f64>,
        quote_rate: Option<f64>,
        slippage: f64,
    ) -> Result<MarketCostProfile> {
        resolve_strict_discovery_cost_profile_from_metadata(
            "EURUSD",
            account_currency,
            metadata,
            spread,
            commission,
            quote_rate,
            Some(1.1),
            slippage,
        )
    }

    #[test]
    fn normalize_canonicalizes_duplicate_indicator_terms() {
        let mut gene = Gene {
            indices: vec![4, 1, 4, 9],
            weights: vec![0.25, 1.0, 0.75, 0.5],
            long_threshold: 0.4,
            short_threshold: -0.4,
            tp_pips: 40.0,
            sl_pips: 20.0,
            ..Default::default()
        };

        gene.normalize(5, 1);

        assert_eq!(gene.indices, vec![1, 4]);
        assert_eq!(gene.weights, vec![1.0, 1.5]);
    }

    #[test]
    fn normalize_repairs_invalid_numeric_fields() {
        let mut gene = Gene {
            indices: vec![0],
            weights: vec![f32::NAN],
            long_threshold: f32::NAN,
            short_threshold: f32::NAN,
            tp_pips: f64::NAN,
            sl_pips: -1.0,
            ..Default::default()
        };

        gene.normalize(3, 1);

        assert_eq!(gene.weights, vec![1.0]);
        assert!(gene.long_threshold > gene.short_threshold);
        assert_eq!(gene.tp_pips, 40.0);
        assert_eq!(gene.sl_pips, 20.0);
    }

    // ─── Phase C broker-cost plumbing (2026-05-28) ────────────────────
    //
    // `infer_market_cost_profile` must surface the broker-supplied
    // swap & PnL-conversion-fee values from `SymbolMetadata` into
    // the `MarketCostProfile`. The eval kernel reads them from
    // BacktestSettings (populated in turn from MarketCostProfile).
    // These tests guard the data-layer plumbing; the eval-kernel
    // application is exercised in eval.rs (Phase C.2).

    #[test]
    fn infer_market_cost_profile_zeroes_swap_and_fee_when_metadata_absent() {
        // EURUSD baked-in default ships with `daily_swap_*: None` and
        // `pnl_conversion_fee_rate: None`. The cost-model fall-back
        // for those is `0.0` (fail-safe — no charge known means no
        // charge applied). The synthetic-fallback warn is logged for
        // commission + spread only, not for swap/fee.
        let profile = infer_market_cost_profile("EURUSD", "USD", None, None, None);
        assert_eq!(profile.swap_long_pips_per_day, 0.0);
        assert_eq!(profile.swap_short_pips_per_day, 0.0);
        assert_eq!(profile.pnl_conversion_fee_rate, 0.0);
    }

    #[test]
    fn strict_discovery_rejects_missing_metadata_and_spread() {
        assert!(strict_profile(None, "USD", None, None, None, 0.0)
            .expect_err("metadata is mandatory")
            .to_string()
            .contains("SymbolMetadata"));

        let mut metadata = strict_metadata("EURUSD", "EUR", "USD");
        metadata.typical_spread_pips = None;
        assert!(strict_profile(Some(&metadata), "USD", Some(f64::NAN), None, None, 0.0)
            .expect_err("invalid explicit spread must fail")
            .to_string()
            .contains("spread override"));
        assert!(strict_profile(Some(&metadata), "USD", None, None, None, 0.0)
            .expect_err("spread truth is mandatory")
            .to_string()
            .contains("spread"));
        let profile = strict_profile(Some(&metadata), "USD", Some(0.8), None, None, 0.3)
            .expect("explicit spread plus slippage should resolve");
        assert!((profile.spread_pips - 1.1).abs() < 1e-12);
    }

    #[test]
    fn strict_discovery_rejects_missing_commission_without_synthetic_seven() {
        let mut metadata = strict_metadata("EURUSD", "EUR", "USD");
        metadata.commission_per_lot = None;
        assert!(strict_profile(Some(&metadata), "USD", None, Some(f64::NAN), None, 0.0)
            .expect_err("invalid explicit commission must fail")
            .to_string()
            .contains("commission override"));
        let err = strict_profile(Some(&metadata), "USD", None, None, None, 0.0)
            .expect_err("missing commission schedule must fail");
        assert!(err.to_string().contains("commission truth"));

        metadata.commission_per_lot = Some(5.25);
        let profile = strict_profile(Some(&metadata), "USD", None, None, None, 0.0)
            .expect("broker commission should resolve");
        assert_eq!(profile.commission_per_trade, 5.25);
        assert_ne!(profile.commission_per_trade, 7.0);

        metadata.commission_per_lot = Some(f64::NAN);
        metadata.commission_type = Some(2);
        metadata.commission_rate_decimal = Some(4.5);
        let profile = strict_profile(Some(&metadata), "USD", None, None, None, 0.0)
            .expect("valid broker schedule should replace invalid precomputed commission");
        assert_eq!(profile.commission_per_trade, 4.5);
    }

    #[test]
    fn strict_discovery_requires_both_swaps_but_accepts_authoritative_zero() {
        let mut metadata = strict_metadata("EURUSD", "EUR", "USD");
        metadata.daily_swap_long_pips = None;
        assert!(strict_profile(Some(&metadata), "USD", None, None, None, 0.0)
            .expect_err("long swap is mandatory")
            .to_string()
            .contains("swap_long"));

        metadata.daily_swap_long_pips = Some(0.0);
        metadata.daily_swap_short_pips = None;
        assert!(strict_profile(Some(&metadata), "USD", None, None, None, 0.0)
            .expect_err("short swap is mandatory")
            .to_string()
            .contains("swap_short"));

        metadata.daily_swap_short_pips = Some(0.0);
        let profile = strict_profile(Some(&metadata), "USD", None, None, None, 0.0)
            .expect("broker zero swaps are authoritative");
        assert_eq!(profile.swap_long_pips_per_day, 0.0);
        assert_eq!(profile.swap_short_pips_per_day, 0.0);
    }

    #[test]
    fn strict_discovery_conversion_fee_depends_on_quote_currency() {
        let same_quote = strict_metadata("EURUSD", "EUR", "USD");
        assert_eq!(
            strict_profile(Some(&same_quote), "USD", None, None, None, 0.0)
                .expect("same quote requires no fee metadata")
                .pnl_conversion_fee_rate,
            0.0
        );

        let cross = strict_metadata("EURUSD", "EUR", "USD");
        assert!(strict_profile(Some(&cross), "GBP", None, None, Some(0.79), 0.0)
            .expect_err("cross account requires fee metadata")
            .to_string()
            .contains("conversion fee"));
    }

    #[test]
    fn strict_discovery_cross_pair_requires_real_conversion_rate() {
        let mut metadata = strict_metadata("EURUSD", "EUR", "USD");
        metadata.pnl_conversion_fee_rate = Some(0.001);
        assert!(strict_profile(Some(&metadata), "GBP", None, None, None, 0.0)
            .expect_err("cross pair requires quote conversion")
            .to_string()
            .contains("quote→account"));

        let profile = strict_profile(Some(&metadata), "GBP", None, None, Some(0.79), 0.0)
            .expect("real quote conversion should resolve");
        assert!((profile.pip_value_per_lot - 7.9).abs() < 1e-12);
        assert!(profile.pip_value_per_lot.is_finite());
    }

    #[test]
    fn strict_discovery_uses_real_price_and_usd_conversion_when_required() {
        let mut base_account = strict_metadata("EURUSD", "EUR", "USD");
        assert!(
            resolve_strict_discovery_cost_profile_from_metadata(
                "EURUSD",
                "EUR",
                Some(&base_account),
                None,
                None,
                Some(0.91),
                None,
                0.0,
            )
            .expect_err("base-currency account requires a real price")
            .to_string()
            .contains("price hint")
        );

        base_account.pnl_conversion_fee_rate = Some(0.001);
        let profile = resolve_strict_discovery_cost_profile_from_metadata(
            "EURUSD",
            "GBP",
            Some(&base_account),
            None,
            None,
            Some(0.8),
            Some(1.1),
            0.0,
        )
        .expect("precomputed quote-currency commission should convert to account currency");
        assert!((profile.commission_per_trade - 4.8).abs() < 1e-12);

        base_account.commission_per_lot = None;
        base_account.commission_type = Some(2);
        base_account.commission_rate_decimal = Some(5.0);
        let profile = resolve_strict_discovery_cost_profile_from_metadata(
            "EURUSD",
            "GBP",
            Some(&base_account),
            None,
            None,
            Some(0.8),
            Some(1.1),
            0.0,
        )
        .expect("USD quote rate also supplies USD-to-account commission conversion");
        assert!((profile.commission_per_trade - 4.0).abs() < 1e-12);

        let mut usd_base = strict_metadata("USDJPY", "USD", "JPY");
        usd_base.pip_size = 0.01;
        usd_base.pip_value_quote = 1_000.0;
        usd_base.commission_per_lot = None;
        usd_base.commission_type = Some(2);
        usd_base.commission_rate_decimal = Some(5.0);
        usd_base.pnl_conversion_fee_rate = Some(0.001);
        let profile = resolve_strict_discovery_cost_profile_from_metadata(
            "USDJPY",
            "EUR",
            Some(&usd_base),
            None,
            None,
            Some(0.006),
            Some(150.0),
            0.0,
        )
        .expect("base-USD price and quote rate should derive USD-to-account conversion");
        assert!((profile.commission_per_trade - 4.5).abs() < 1e-12);
    }

    #[test]
    fn strict_profile_survives_market_profile_to_evaluation_config() {
        let mut metadata = strict_metadata("EURUSD", "EUR", "USD");
        metadata.daily_swap_long_pips = Some(-0.7);
        metadata.daily_swap_short_pips = Some(0.2);
        metadata.pnl_conversion_fee_rate = Some(0.003);
        let profile = strict_profile(Some(&metadata), "GBP", None, None, Some(0.8), 0.4)
            .expect("complete strict profile");
        let evaluation = EvaluationConfig::from_market_cost_profile(profile.clone());

        assert_eq!(evaluation.swap_long_pips_per_day, profile.swap_long_pips_per_day);
        assert_eq!(evaluation.swap_short_pips_per_day, profile.swap_short_pips_per_day);
        assert_eq!(evaluation.pnl_conversion_fee_rate, profile.pnl_conversion_fee_rate);
        assert!(evaluation.pip_value.is_finite());
        assert!(evaluation.pip_value_per_lot.is_finite());
        assert!(evaluation.spread_pips.is_finite());
        assert!(evaluation.commission_per_trade.is_finite());
    }

    // Note: a follow-up test will verify that broker-supplied swap +
    // fee values flow end-to-end from `SymbolFinancials` →
    // `SymbolMetadata` → `MarketCostProfile`. That requires a
    // `SymbolMetadataTable::override_for_test` helper (not yet
    // wired) so the test can install custom metadata without
    // racing the singleton. Currently the Phase C schema tests in
    // `symbol_metadata.rs::tests::phase_c_fields_*` cover the
    // schema/serde boundary; this file covers the cost-model
    // boundary (the third leg — eval kernel — lands in Phase C.2).
}
