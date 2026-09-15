//! Self-describing **live portfolio artifact** — the bridge from discovery to the
//! autonomous trader.
//!
//! THE PARITY PROBLEM (verified 2026-06-04): a discovered `Gene`'s `indices`
//! reference columns in the **prefiltered** (and optionally normalized) feature
//! matrix, not raw `compute_hpc_features`. But no single existing artifact
//! persists BOTH the full genes (with SMC flags — only in the checkpoint /
//! portfolio-selection files) AND the `effective_feature_names` that the indices
//! map to (only in the in-memory `DiscoveryResult`, or per-gene in the
//! `GeneExport`). So a trader that loads one artifact alone cannot reproduce the
//! exact feature columns ⇒ silently wrong signals.
//!
//! [`LivePortfolioArtifact`] fixes that: it pairs the full `Vec<Gene>` with the
//! ordered `effective_feature_names`, the `base_tf` / `higher_tfs` the cube was
//! built from, and the `normalize_features` flag in effect — everything the
//! trader needs to rebuild the EXACT matrix the genes were evolved against.
//!
//! Discovery writes it (`save_live_portfolio_json`, called next to
//! `save_portfolio_json`); the trader reads it (`load_live_ready_portfolio_json`) and
//! projects its freshly-computed features onto `effective_feature_names` with
//! [`project_features_to_effective`] (the same by-name selection discovery's
//! forward-test path uses).

use std::path::Path;

use neoethos_data::{FeatureData, FeatureFrame};
use neoethos_core::contracts::{
    ArtifactEnvelope, LivePromotionGate, LiveValidationEvidence, ValidationEvidenceKind,
    ValidationEvidenceManifest,
};
use neoethos_core::BrokerFinancialTruthReport;
use serde::{Deserialize, Serialize};

use crate::artifact_io::stable_json_hash;
use crate::Gene;
use crate::discovery::{DiscoveryResult, live_validation_evidence_from_discovery};

/// Bumped when the artifact's shape changes incompatibly.
pub const LIVE_PORTFOLIO_SCHEMA_VERSION: u32 = 1;

/// Everything the autonomous trader needs to evaluate a discovered portfolio on
/// fresh data with backtest parity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LivePortfolioArtifact {
    pub schema_version: u32,
    pub symbol: String,
    pub base_tf: String,
    pub higher_tfs: Vec<String>,
    /// Feature names AFTER discovery's prefilter, in the exact column order the
    /// gene `indices` reference.
    pub effective_feature_names: Vec<String>,
    /// Whether discovery's feature pipeline normalized features. If `true`, the
    /// trader must apply the same normalization (and today the per-column stats
    /// are NOT persisted, so the trader must recompute them the same way — see
    /// the design §6.1). Default discovery is `false`.
    pub normalize_features: bool,
    /// Candidate or promoted portfolio — FULL genes, including SMC flags + SL/TP.
    pub genes: Vec<Gene>,
    /// Financial inputs are independent from statistical validation. Discovery
    /// produces mechanical evidence unless validated broker truth is attached.
    #[serde(default)]
    pub broker_financial_truth: BrokerFinancialTruthReport,
    /// Explicit live boundary. Missing on legacy artifacts, therefore `false`.
    #[serde(default)]
    pub promotion_ready: bool,
    #[serde(default = "all_promotion_evidence_kinds")]
    pub promotion_missing_evidence: Vec<ValidationEvidenceKind>,
    #[serde(default)]
    pub promotion_failed_evidence: Vec<ValidationEvidenceKind>,
    /// Complete existing promotion contracts. `None` means candidate-only.
    #[serde(default)]
    pub promotion: Option<LivePortfolioPromotionProof>,
}

fn all_promotion_evidence_kinds() -> Vec<ValidationEvidenceKind> {
    ValidationEvidenceKind::ALL.to_vec()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePortfolioPromotionProof {
    pub gate: LivePromotionGate,
    pub artifact: ArtifactEnvelope<String>,
    pub evidence_manifest: ValidationEvidenceManifest,
    pub validation_evidence: LiveValidationEvidence,
    #[serde(default)]
    pub broker_financial_truth: BrokerFinancialTruthReport,
}

impl LivePortfolioArtifact {
    pub fn from_discovery(
        symbol: &str,
        base_tf: &str,
        higher_tfs: &[String],
        normalize_features: bool,
        result: &DiscoveryResult,
    ) -> Self {
        let mut artifact = Self {
            schema_version: LIVE_PORTFOLIO_SCHEMA_VERSION,
            symbol: symbol.to_string(),
            base_tf: base_tf.to_string(),
            higher_tfs: higher_tfs.to_vec(),
            effective_feature_names: result.effective_feature_names.clone(),
            normalize_features,
            genes: result.portfolio.clone(),
            broker_financial_truth: BrokerFinancialTruthReport::mechanical(
                "discovery_ohlc_cost_model_is_not_synchronized_broker_bid_ask",
            ),
            promotion_ready: false,
            promotion_missing_evidence: Vec::new(),
            promotion_failed_evidence: Vec::new(),
            promotion: None,
        };
        artifact.record_discovery_promotion_status(result);
        artifact
    }

    pub fn with_validated_promotion(
        mut self,
        promotion: LivePortfolioPromotionProof,
    ) -> anyhow::Result<Self> {
        self.validate_promotion(&promotion)?;
        self.broker_financial_truth = promotion.broker_financial_truth.clone();
        self.promotion_ready = true;
        self.promotion_missing_evidence.clear();
        self.promotion_failed_evidence.clear();
        self.promotion = Some(promotion);
        Ok(self)
    }

    /// Independent final guard used by every live/autonomous consumer.
    pub fn require_live_promotion_ready(&self) -> anyhow::Result<()> {
        if !self.promotion_ready {
            anyhow::bail!(
                "live portfolio is not promotion-ready; missing evidence: {}; failed evidence: {}",
                evidence_labels(&self.promotion_missing_evidence),
                evidence_labels(&self.promotion_failed_evidence),
            );
        }
        if !self.promotion_missing_evidence.is_empty()
            || !self.promotion_failed_evidence.is_empty()
        {
            anyhow::bail!("live portfolio has inconsistent promotion evidence status");
        }
        let promotion = self
            .promotion
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("live portfolio promotion proof is missing"))?;
        self.broker_financial_truth
            .require_live()
            .map_err(anyhow::Error::new)?;
        self.validate_promotion(promotion)
    }

    fn subject_hash(&self) -> anyhow::Result<String> {
        stable_json_hash(&(
            &self.symbol,
            &self.base_tf,
            &self.higher_tfs,
            &self.effective_feature_names,
            self.normalize_features,
            &self.genes,
        ))
    }

    fn validate_promotion(&self, promotion: &LivePortfolioPromotionProof) -> anyhow::Result<()> {
        promotion
            .broker_financial_truth
            .require_live()
            .map_err(anyhow::Error::new)?;
        if self.promotion_ready
            && self.broker_financial_truth.truth_hash
                != promotion.broker_financial_truth.truth_hash
        {
            anyhow::bail!("live portfolio broker financial truth proof does not match artifact");
        }
        if promotion.artifact.payload != self.subject_hash()? {
            anyhow::bail!("live portfolio promotion proof does not match portfolio contents");
        }
        if !promotion.gate.require_deterministic {
            anyhow::bail!("live portfolio promotion must require deterministic provenance");
        }
        let contract = &promotion.gate.live_execution_contract;
        if promotion.broker_financial_truth.symbol.as_deref() != Some(self.symbol.as_str()) {
            anyhow::bail!("broker financial truth does not bind this symbol");
        }
        if promotion.broker_financial_truth.strategy_hash.as_deref()
            != Some(promotion.artifact.payload.as_str())
        {
            anyhow::bail!("broker financial truth does not bind this portfolio");
        }
        if promotion.broker_financial_truth.risk_config_hash.as_deref()
            != Some(contract.risk_config_hash.as_str())
        {
            anyhow::bail!("broker financial truth does not bind live risk configuration");
        }
        if !contract.require_walkforward_pass {
            anyhow::bail!("live portfolio promotion must require walkforward evidence");
        }
        if !contract.require_forward_test_pass {
            anyhow::bail!("live portfolio promotion must require forward_test evidence");
        }
        if !contract.require_prop_firm_pass {
            anyhow::bail!("live portfolio promotion must require prop_firm evidence");
        }
        if contract
            .required_live_sim_runtime_model_hash
            .as_deref()
            .is_none_or(str::is_empty)
        {
            anyhow::bail!(
                "live portfolio promotion must require live_execution_simulation evidence"
            );
        }
        promotion
            .gate
            .validate(&promotion.artifact, &promotion.evidence_manifest)
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        contract
            .validate_evidence(&promotion.validation_evidence)
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }

    fn record_discovery_promotion_status(&mut self, result: &DiscoveryResult) {
        if result.canonical_backtest_artifacts.is_empty() {
            self.promotion_missing_evidence
                .push(ValidationEvidenceKind::CanonicalBacktest);
        }
        if result.walkforward_validation_artifacts.is_empty() {
            self.promotion_missing_evidence
                .push(ValidationEvidenceKind::WalkForward);
        } else if !result.validation_gates.walkforward_passed {
            self.promotion_failed_evidence
                .push(ValidationEvidenceKind::WalkForward);
        }

        let evidence = live_validation_evidence_from_discovery(result);
        match evidence.forward_test_passed {
            None => self
                .promotion_missing_evidence
                .push(ValidationEvidenceKind::ForwardTest),
            Some(false) => self
                .promotion_failed_evidence
                .push(ValidationEvidenceKind::ForwardTest),
            Some(true) => {}
        }
        match evidence.prop_firm_passed {
            None => self
                .promotion_missing_evidence
                .push(ValidationEvidenceKind::PropFirmRisk),
            Some(false) => self
                .promotion_failed_evidence
                .push(ValidationEvidenceKind::PropFirmRisk),
            Some(true) => {}
        }
        self.promotion_missing_evidence
            .push(ValidationEvidenceKind::LiveExecutionSimulation);
    }
}

fn evidence_labels(kinds: &[ValidationEvidenceKind]) -> String {
    if kinds.is_empty() {
        return "none".to_string();
    }
    kinds
        .iter()
        .map(|kind| match kind {
            ValidationEvidenceKind::CanonicalBacktest => "canonical_backtest",
            ValidationEvidenceKind::WalkForward => "walkforward",
            ValidationEvidenceKind::ForwardTest => "forward_test",
            ValidationEvidenceKind::LiveExecutionSimulation => "live_execution_simulation",
            ValidationEvidenceKind::PropFirmRisk => "prop_firm",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Write the live portfolio artifact as pretty JSON. Additive — does NOT touch
/// any existing discovery artifact. Reads the in-effect normalize flag from the
/// data-runtime overrides so the trader knows whether discovery normalized.
pub fn save_live_portfolio_json(
    path: impl AsRef<Path>,
    symbol: &str,
    base_tf: &str,
    higher_tfs: &[String],
    result: &DiscoveryResult,
) -> anyhow::Result<()> {
    let normalize_features = neoethos_data::current_data_runtime_overrides().normalize_features;
    let artifact =
        LivePortfolioArtifact::from_discovery(symbol, base_tf, higher_tfs, normalize_features, result);
    let json = serde_json::to_string_pretty(&artifact).map_err(|e| {
        anyhow::anyhow!("failed to serialize live portfolio artifact: {e}")
    })?;
    std::fs::write(&path, json).map_err(|e| {
        anyhow::anyhow!(
            "failed to write live portfolio artifact to {}: {e}",
            path.as_ref().display()
        )
    })?;
    Ok(())
}

/// Load a live portfolio artifact written by [`save_live_portfolio_json`].
pub fn load_live_portfolio_json(path: impl AsRef<Path>) -> anyhow::Result<LivePortfolioArtifact> {
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        anyhow::anyhow!(
            "live portfolio artifact {} not readable: {e}",
            path.as_ref().display()
        )
    })?;
    let artifact: LivePortfolioArtifact = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "live portfolio artifact {} is not valid: {e}",
            path.as_ref().display()
        )
    })?;
    Ok(artifact)
}

/// Load for trading. Candidate and legacy artifacts stay readable through
/// [`load_live_portfolio_json`] but cannot cross this boundary without proof.
pub fn load_live_ready_portfolio_json(
    path: impl AsRef<Path>,
) -> anyhow::Result<LivePortfolioArtifact> {
    let artifact = load_live_portfolio_json(&path)?;
    artifact.require_live_promotion_ready().map_err(|err| {
        anyhow::anyhow!(
            "live portfolio artifact {} rejected: {err}",
            path.as_ref().display()
        )
    })?;
    Ok(artifact)
}

/// Project a freshly-computed raw `FeatureFrame` onto `effective_feature_names`
/// (post-prefilter set), in that exact order, so a gene's `indices` reference
/// the right columns. This is the SAME by-name selection the discovery
/// forward-test path uses (`compute_discovery_forward_test_artifacts`).
///
/// Returns `Err` when any effective name is missing from `raw` — that means the
/// trader's feature pipeline diverged from discovery's, and evaluating a gene on
/// it would be meaningless (fail loud rather than trade on wrong columns).
pub fn project_features_to_effective(
    raw: &FeatureFrame,
    effective_feature_names: &[String],
) -> anyhow::Result<FeatureFrame> {
    if raw.names == effective_feature_names {
        return Ok(raw.clone());
    }
    let mut keep_indices = Vec::with_capacity(effective_feature_names.len());
    for name in effective_feature_names {
        let idx = raw
            .names
            .iter()
            .position(|candidate| candidate == name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "live feature set is missing '{}' from the discovery effective feature set; \
                     the trader must compute features with the SAME pipeline + config as the \
                     discovery run that produced this portfolio",
                    name
                )
            })?;
        keep_indices.push(idx);
    }
    let n_rows = raw.n_samples();
    let mut projected = ndarray::Array2::<f32>::zeros((n_rows, keep_indices.len()));
    for (new_idx, &orig_idx) in keep_indices.iter().enumerate() {
        projected
            .column_mut(new_idx)
            .assign(&raw.feature_column(orig_idx));
    }
    Ok(FeatureFrame {
        timestamps: raw.timestamps.clone(),
        names: effective_feature_names.to_vec(),
        data: FeatureData::InMemory(projected),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use neoethos_core::contracts::{
        ArtifactKind, ArtifactProvenance, BackendKind, DeterminismPolicy, DeviceAssignment,
        LiveExecutionContract, RuntimeMode,
    };

    fn candidate_artifact() -> LivePortfolioArtifact {
        let mut gene = Gene::default();
        gene.indices = vec![0, 2];
        gene.weights = vec![0.5, -0.25];
        gene.long_threshold = 0.1;
        gene.short_threshold = -0.1;
        gene.strategy_id = "test-gene".to_string();

        LivePortfolioArtifact {
            schema_version: LIVE_PORTFOLIO_SCHEMA_VERSION,
            symbol: "EURGBP".to_string(),
            base_tf: "D1".to_string(),
            higher_tfs: vec!["W1".to_string()],
            effective_feature_names: vec![
                "rsi".to_string(),
                "atr".to_string(),
                "W1_rsi".to_string(),
            ],
            normalize_features: false,
            genes: vec![gene],
            broker_financial_truth: BrokerFinancialTruthReport::mechanical("test candidate"),
            promotion_ready: false,
            promotion_missing_evidence: ValidationEvidenceKind::ALL.to_vec(),
            promotion_failed_evidence: Vec::new(),
            promotion: None,
        }
    }

    fn complete_promotion(artifact: &LivePortfolioArtifact) -> LivePortfolioPromotionProof {
        let risk_config_hash = binding_hash("risk");
        let provenance = ArtifactProvenance::new(
            ArtifactKind::LiveReadyStrategy,
            "feature-schema",
            "dataset",
            "symbols",
            "timeframes",
            "timestamps",
            "availability",
            "labels",
            "training",
            "search",
            "runtime",
            &risk_config_hash,
            DeterminismPolicy::Deterministic { seed: 42 },
            "hardware",
            DeviceAssignment::cpu(),
            BackendKind::NativeCpu,
            RuntimeMode::Canonical,
            None,
            "commit",
        )
        .unwrap();
        let contract = LiveExecutionContract::new(
            "feature-schema",
            "timestamps",
            "availability",
            "symbols",
            "runtime",
            &risk_config_hash,
        )
        .with_required_walkforward_pass()
        .with_required_forward_test_pass()
        .with_required_prop_firm_pass()
        .with_required_live_sim_runtime_model_hash("live-model");
        let gate = LivePromotionGate::new(contract).require_deterministic(true);
        let envelope =
            ArtifactEnvelope::new(provenance, artifact.subject_hash().unwrap()).unwrap();
        let evidence_manifest = ValidationEvidenceManifest::new(
            "canonical",
            "walkforward",
            "forward",
            "live-sim",
            "prop-firm",
        )
        .unwrap();
        let validation_evidence = LiveValidationEvidence {
            walkforward_passed: true,
            cpcv_passed: false,
            forward_test_passed: Some(true),
            prop_firm_passed: Some(true),
            live_sim_runtime_model_hash: Some("live-model".to_string()),
        };
        LivePortfolioPromotionProof {
            gate,
            artifact: envelope,
            evidence_manifest,
            validation_evidence,
            broker_financial_truth: live_broker_truth_report(
                artifact.subject_hash().unwrap(),
                risk_config_hash,
            ),
        }
    }

    fn binding_hash(label: &str) -> String {
        format!(
            "fnv64:{:016x}",
            neoethos_core::utils::fnv1a64(label.as_bytes())
        )
    }

    fn live_broker_truth_report(
        strategy_hash: String,
        risk_config_hash: String,
    ) -> BrokerFinancialTruthReport {
        use neoethos_core::{
            BrokerCloseDeal, BrokerPositionFinancialRow, BrokerPositionFinancialSnapshot,
            CommissionTypeExact, ConversionBook, EvidenceProvenance, ExactCommissionSchedule,
            ExactProtoOaSymbolContract, ExactSwapSchedule, ExactTradingInterval,
            ExpectedLocalClose, HistoricalBrokerTruthEvidence, LiveBrokerTruthEvidence,
            MinimumCommissionCurrency, PositionDirection, SwapCalculationExact,
            SynchronizedBidAsk, SynchronizedQuote, reconcile_close_deals,
        };
        let provenance = |name: &str| {
            EvidenceProvenance::new(
                name,
                1_700_000_000_000,
                format!("fnv64:{:016x}", neoethos_core::utils::fnv1a64(name.as_bytes())),
            )
            .unwrap()
        };
        let contract = ExactProtoOaSymbolContract::new(
            7,
            5,
            "GBP",
            1,
            "EURGBP",
            4,
            "EUR",
            5,
            "GBP",
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
                minimum_asset: "GBP".to_string(),
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
        .unwrap();
        let quotes = SynchronizedBidAsk::new(
            1,
            "EURGBP",
            vec![SynchronizedQuote {
                timestamp_ms: 1_000,
                bid: 0.85,
                ask: 0.8502,
            }],
            "fnv64:0000000000000001",
            "fnv64:0000000000000002",
            provenance("quotes"),
        )
        .unwrap();
        let historical = HistoricalBrokerTruthEvidence::new(
            contract,
            quotes,
            ConversionBook::new("GBP", vec![]).unwrap(),
            10_000.0,
            risk_config_hash,
            strategy_hash,
            binding_hash("slippage"),
        )
        .unwrap();
        let positions = BrokerPositionFinancialSnapshot::new(
            7,
            "GBP",
            1_700_000_000_000,
            i64::MAX / 4,
            vec![BrokerPositionFinancialRow {
                position_id: 9,
                symbol_id: 1,
                direction: PositionDirection::Long,
                volume_units: 1_000.0,
                entry_price: 0.85,
                gross_unrealized_pnl_account: 1.0,
                net_unrealized_pnl_account: 0.9,
                swap_account: Some(0.0),
                commission_account: Some(-0.1),
            }],
            provenance("positions"),
        )
        .unwrap();
        let reconciliation = reconcile_close_deals(
            ExpectedLocalClose {
                account_id: 7,
                symbol_id: 1,
                position_id: 10,
                direction: PositionDirection::Long,
                expected_volume_units: 1_000.0,
                local_estimated_pnl_account: Some(0.5),
            },
            vec![BrokerCloseDeal {
                deal_id: 11,
                order_id: 12,
                position_id: 10,
                symbol_id: 1,
                direction: PositionDirection::Short,
                filled_volume_units: 1_000.0,
                execution_timestamp_ms: 2_000,
                gross_profit_account: 1.0,
                commission_account: -0.1,
                swap_account: 0.0,
                conversion_fee_account: 0.0,
                net_profit_account: 0.9,
            }],
            provenance("deals"),
        )
        .unwrap();
        BrokerFinancialTruthReport::live(
            &LiveBrokerTruthEvidence::new(historical, positions, vec![reconciliation]).unwrap(),
        )
        .unwrap()
    }

    fn temp_portfolio_path(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{unique}.live_portfolio.json"))
    }

    #[test]
    fn artifact_round_trips_through_json() {
        let artifact = candidate_artifact();

        let json = serde_json::to_string(&artifact).unwrap();
        let back: LivePortfolioArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(artifact, back, "artifact must survive a JSON round-trip");
    }

    #[test]
    fn legacy_artifact_loads_as_not_live_ready() {
        let mut value = serde_json::to_value(candidate_artifact()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("promotion_ready");
        object.remove("promotion_missing_evidence");
        object.remove("promotion_failed_evidence");
        object.remove("promotion");

        let artifact: LivePortfolioArtifact = serde_json::from_value(value).unwrap();
        assert!(!artifact.promotion_ready);
        assert!(artifact.promotion.is_none());
        assert_eq!(
            artifact.promotion_missing_evidence,
            ValidationEvidenceKind::ALL
        );
        assert!(artifact.require_live_promotion_ready().is_err());
    }

    #[test]
    fn empty_manifest_cannot_promote() {
        let artifact = candidate_artifact();
        let mut proof = complete_promotion(&artifact);
        proof.evidence_manifest = ValidationEvidenceManifest {
            canonical_backtest_validation_hash: String::new(),
            walkforward_validation_hash: String::new(),
            forward_test_validation_hash: String::new(),
            live_execution_simulation_hash: String::new(),
            prop_firm_risk_validation_hash: String::new(),
        };

        assert!(artifact.with_validated_promotion(proof).is_err());
    }

    #[test]
    fn every_missing_manifest_kind_blocks_promotion_with_named_evidence() {
        let artifact = candidate_artifact();
        for kind in ValidationEvidenceKind::ALL {
            let mut proof = complete_promotion(&artifact);
            match kind {
                ValidationEvidenceKind::CanonicalBacktest => {
                    proof.evidence_manifest.canonical_backtest_validation_hash.clear()
                }
                ValidationEvidenceKind::WalkForward => {
                    proof.evidence_manifest.walkforward_validation_hash.clear()
                }
                ValidationEvidenceKind::ForwardTest => {
                    proof.evidence_manifest.forward_test_validation_hash.clear()
                }
                ValidationEvidenceKind::LiveExecutionSimulation => {
                    proof.evidence_manifest.live_execution_simulation_hash.clear()
                }
                ValidationEvidenceKind::PropFirmRisk => {
                    proof.evidence_manifest.prop_firm_risk_validation_hash.clear()
                }
            }
            let error = artifact
                .clone()
                .with_validated_promotion(proof)
                .expect_err("missing manifest kind must reject promotion");
            assert!(error.to_string().contains(kind.field_name()));
        }
    }

    #[test]
    fn failed_required_evidence_outcomes_block_promotion() {
        let artifact = candidate_artifact();
        let mut cases = Vec::new();

        let mut walkforward = complete_promotion(&artifact);
        walkforward.validation_evidence.walkforward_passed = false;
        cases.push((walkforward, "walkforward"));

        let mut forward = complete_promotion(&artifact);
        forward.validation_evidence.forward_test_passed = Some(false);
        cases.push((forward, "forward_test"));

        let mut prop_firm = complete_promotion(&artifact);
        prop_firm.validation_evidence.prop_firm_passed = Some(false);
        cases.push((prop_firm, "prop_firm"));

        let mut live_sim = complete_promotion(&artifact);
        live_sim.validation_evidence.live_sim_runtime_model_hash =
            Some("wrong-model".to_string());
        cases.push((live_sim, "live_sim_runtime_model_hash"));

        for (proof, expected) in cases {
            let error = artifact
                .clone()
                .with_validated_promotion(proof)
                .expect_err("failed required evidence must reject promotion");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn complete_promotion_round_trip_is_live_ready() {
        let candidate = candidate_artifact();
        let promoted = candidate
            .clone()
            .with_validated_promotion(complete_promotion(&candidate))
            .expect("complete canonical proof must promote");
        assert!(promoted.promotion_ready);
        promoted.require_live_promotion_ready().unwrap();

        let json = serde_json::to_string(&promoted).unwrap();
        let round_trip: LivePortfolioArtifact = serde_json::from_str(&json).unwrap();
        assert!(round_trip.promotion_ready);
        round_trip.require_live_promotion_ready().unwrap();

        let path = temp_portfolio_path("complete-promotion");
        std::fs::write(&path, json).unwrap();
        load_live_ready_portfolio_json(&path).expect("strict loader must accept complete proof");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn validation_evidence_cannot_promote_without_live_broker_truth() {
        let artifact = candidate_artifact();
        let mut proof = complete_promotion(&artifact);
        proof.broker_financial_truth =
            BrokerFinancialTruthReport::mechanical("explicit_spread_pips=1.0");
        let error = artifact
            .with_validated_promotion(proof)
            .expect_err("mechanical financial inputs must never pass live promotion");
        assert!(
            error
                .to_string()
                .contains(neoethos_core::BROKER_FINANCIAL_TRUTH_UNAVAILABLE_V1)
        );
    }

    #[test]
    fn broker_truth_for_another_portfolio_cannot_promote() {
        let artifact = candidate_artifact();
        let mut proof = complete_promotion(&artifact);
        proof.broker_financial_truth = live_broker_truth_report(
            binding_hash("different-portfolio"),
            proof.gate.live_execution_contract.risk_config_hash.clone(),
        );
        let error = artifact
            .with_validated_promotion(proof)
            .expect_err("broker truth must bind the exact portfolio");
        assert!(error.to_string().contains("does not bind this portfolio"));
    }

    #[test]
    fn broker_truth_for_another_risk_config_cannot_promote() {
        let artifact = candidate_artifact();
        let mut proof = complete_promotion(&artifact);
        proof.broker_financial_truth = live_broker_truth_report(
            artifact.subject_hash().unwrap(),
            binding_hash("different-risk-config"),
        );
        let error = artifact
            .with_validated_promotion(proof)
            .expect_err("broker truth must bind the live risk configuration");
        assert!(
            error
                .to_string()
                .contains("does not bind live risk configuration")
        );
    }

    #[test]
    fn promoted_artifact_rejects_replaced_financial_truth() {
        let candidate = candidate_artifact();
        let proof = complete_promotion(&candidate);
        let mut promoted = candidate.with_validated_promotion(proof).unwrap();
        promoted.broker_financial_truth =
            BrokerFinancialTruthReport::mechanical("tampered after promotion");
        assert!(promoted.require_live_promotion_ready().is_err());
    }

    #[test]
    fn strict_loader_rejects_candidate_regardless_of_filename() {
        let path = temp_portfolio_path("promoted-and-ready");
        std::fs::write(&path, serde_json::to_vec(&candidate_artifact()).unwrap()).unwrap();

        let loose = load_live_portfolio_json(&path).expect("candidate remains readable");
        assert!(!loose.promotion_ready);
        let error =
            load_live_ready_portfolio_json(&path).expect_err("candidate must not trade");
        assert!(error.to_string().contains("not promotion-ready"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn proof_cannot_be_reused_after_portfolio_tampering() {
        let candidate = candidate_artifact();
        let mut promoted = candidate
            .clone()
            .with_validated_promotion(complete_promotion(&candidate))
            .unwrap();
        promoted.genes[0].long_threshold += 0.01;

        let error = promoted
            .require_live_promotion_ready()
            .expect_err("proof must bind to exact portfolio contents");
        assert!(error.to_string().contains("does not match portfolio contents"));
    }

    #[test]
    fn project_selects_and_reorders_by_name() {
        // raw frame: 3 cols [a, b, c]; effective wants [c, a] (subset + reorder).
        let data = ndarray::array![
            [1.0_f32, 2.0, 3.0],
            [4.0, 5.0, 6.0],
        ];
        let raw = FeatureFrame {
            timestamps: vec![0, 1],
            names: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            data: FeatureData::InMemory(data),
        };
        let effective = vec!["c".to_string(), "a".to_string()];
        let projected = project_features_to_effective(&raw, &effective).unwrap();
        assert_eq!(projected.names, effective);
        assert_eq!(projected.n_features(), 2);
        // column 0 == raw "c" == [3, 6]; column 1 == raw "a" == [1, 4]
        assert_eq!(projected.feature_at(0, 0), 3.0);
        assert_eq!(projected.feature_at(1, 0), 6.0);
        assert_eq!(projected.feature_at(0, 1), 1.0);
        assert_eq!(projected.feature_at(1, 1), 4.0);
    }

    #[test]
    fn project_errors_on_missing_feature() {
        let data = ndarray::array![[1.0_f32, 2.0]];
        let raw = FeatureFrame {
            timestamps: vec![0],
            names: vec!["a".to_string(), "b".to_string()],
            data: FeatureData::InMemory(data),
        };
        let effective = vec!["a".to_string(), "missing".to_string()];
        assert!(project_features_to_effective(&raw, &effective).is_err());
    }
}
