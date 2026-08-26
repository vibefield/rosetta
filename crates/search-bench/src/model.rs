use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{CAMPAIGN_FORMAT, SUITE_FORMAT};

fn default_weight() -> u32 {
    1
}

fn default_limit() -> i64 {
    100
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct QueryFilterSpec {
    pub session: Option<String>,
    pub runtime: Option<String>,
    pub project: Option<String>,
    pub kind: Option<String>,
    pub after_ts: Option<i64>,
    pub before_ts: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryCase {
    /// Safe report label. Never put query text or user data here.
    pub label: String,
    /// Declared selectivity/feature class (`broad`, `selective`, `phrase`, …).
    pub class: String,
    /// Private workload input. Reports contain only its digest.
    pub query: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub filter: QueryFilterSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySuite {
    pub format: u32,
    pub name: String,
    /// `diagnostic` is a declared mix; `observed` must come from a
    /// privacy-preserving product histogram/workload capture.
    pub basis: String,
    pub cases: Vec<QueryCase>,
}

impl QuerySuite {
    pub fn validate(&self) -> Result<(), String> {
        if self.format != SUITE_FORMAT {
            return Err(format!(
                "query suite format {} is not supported (expected {SUITE_FORMAT})",
                self.format
            ));
        }
        if self.name.trim().is_empty() {
            return Err("query suite name must not be empty".into());
        }
        if !matches!(self.basis.as_str(), "diagnostic" | "observed") {
            return Err("query suite basis must be `diagnostic` or `observed`".into());
        }
        if self.cases.is_empty() {
            return Err("query suite needs at least one case".into());
        }
        let mut labels = BTreeSet::new();
        for case in &self.cases {
            if case.label.trim().is_empty() || case.class.trim().is_empty() {
                return Err("query labels and classes must not be empty".into());
            }
            if !labels.insert(&case.label) {
                return Err(format!("duplicate query label `{}`", case.label));
            }
            if case.query.trim().is_empty() {
                return Err(format!("case `{}` has an empty query", case.label));
            }
            if case.weight == 0 {
                return Err(format!("case `{}` has zero weight", case.label));
            }
            if !(1..=1_000).contains(&case.limit) {
                return Err(format!(
                    "case `{}` limit {} is outside 1..=1000",
                    case.label, case.limit
                ));
            }
            if let (Some(after), Some(before)) = (case.filter.after_ts, case.filter.before_ts)
                && after > before
            {
                return Err(format!(
                    "case `{}` has after_ts greater than before_ts",
                    case.label
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileFingerprint {
    pub relative_path: String,
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerFingerprint {
    pub files: Vec<FileFingerprint>,
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFingerprint {
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignManifest {
    pub format: u32,
    pub campaign_id: String,
    pub ledger: LedgerFingerprint,
    pub query_suite: ArtifactFingerprint,
    pub suite_name: String,
    pub suite_basis: String,
}

impl CampaignManifest {
    pub(crate) fn validate_format(&self) -> Result<(), String> {
        if self.format != CAMPAIGN_FORMAT {
            return Err(format!(
                "campaign format {} is not supported (expected {CAMPAIGN_FORMAT})",
                self.format
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionCounts {
    pub messages: u64,
    pub fts_rows: u64,
    pub indexed_sessions: u64,
    pub source_ops: u64,
    pub pending_merges: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseFingerprint {
    pub bytes: u64,
    pub blake3: String,
    pub schema_version: i64,
    pub sqlite_version: String,
    pub counts: ProjectionCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencySummary {
    pub samples: u64,
    pub min_nanos: u64,
    pub mean_nanos: u64,
    pub p50_nanos: u64,
    pub p95_nanos: u64,
    pub p99_nanos: u64,
    pub max_nanos: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseReport {
    pub label: String,
    pub class: String,
    pub query_blake3: String,
    pub filter_blake3: String,
    pub weight: u32,
    pub limit: i64,
    pub hits: u64,
    pub result_blake3: String,
    pub latency: LatencySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassReport {
    pub class: String,
    pub latency: LatencySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneReport {
    pub lane: String,
    pub latency: LatencySummary,
    pub classes: Vec<ClassReport>,
    pub cases: Vec<CaseReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    pub warmup_cycles: u32,
    pub measured_cycles: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReport {
    pub format: u32,
    pub campaign_id: String,
    pub implementation: String,
    pub machine: String,
    pub os: String,
    pub arch: String,
    pub suite_name: String,
    pub suite_basis: String,
    pub ledger: LedgerFingerprint,
    pub query_suite: ArtifactFingerprint,
    pub database: DatabaseFingerprint,
    pub config: RunConfig,
    pub lanes: Vec<LaneReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassComparison {
    pub class: String,
    pub before: LatencySummary,
    pub after: LatencySummary,
    pub p50_change_percent: f64,
    pub p95_change_percent: f64,
    pub p99_change_percent: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneComparison {
    pub lane: String,
    pub before: LatencySummary,
    pub after: LatencySummary,
    pub p50_change_percent: f64,
    pub p95_change_percent: f64,
    pub p99_change_percent: f64,
    pub classes: Vec<ClassComparison>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonReport {
    pub campaign_id: String,
    pub before_implementation: String,
    pub after_implementation: String,
    pub machine: String,
    pub lanes: Vec<LaneComparison>,
}

#[cfg(test)]
mod tests {
    use super::{QueryCase, QueryFilterSpec, QuerySuite};

    fn suite() -> QuerySuite {
        QuerySuite {
            format: 1,
            name: "safe".into(),
            basis: "diagnostic".into(),
            cases: vec![QueryCase {
                label: "broad-one".into(),
                class: "broad".into(),
                query: "one".into(),
                weight: 1,
                limit: 100,
                filter: QueryFilterSpec::default(),
            }],
        }
    }

    #[test]
    fn suite_validation_rejects_ambiguous_inputs() {
        assert!(suite().validate().is_ok());
        let mut invalid = suite();
        invalid.cases.push(invalid.cases[0].clone());
        assert!(invalid.validate().is_err());
        let mut invalid = suite();
        invalid.basis = "pretend-production".into();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn committed_diagnostic_suite_is_valid() {
        let suite: QuerySuite =
            serde_json::from_str(include_str!("../workloads/fts5-diagnostic-v1.json")).unwrap();
        suite.validate().unwrap();
        assert_eq!(suite.basis, "diagnostic");
    }
}
