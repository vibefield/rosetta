use serde::{Deserialize, Serialize};

use crate::model::{DatabaseFingerprint, LatencySummary, RunConfig};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexFingerprint {
    pub files: u64,
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TantivyBuildReport {
    pub version: String,
    pub threads: usize,
    pub memory_bytes: u64,
    pub documents: u64,
    pub source_text_bytes: u64,
    pub wall_nanos: u64,
    pub segments: u64,
    pub index: IndexFingerprint,
    pub sqlite_fts_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TantivyCaseReport {
    pub label: String,
    pub class: String,
    pub query_blake3: String,
    pub filter_blake3: String,
    pub weight: u32,
    pub limit: i64,
    pub fts_hits: u64,
    pub tantivy_hits: u64,
    pub fts_result_blake3: String,
    pub tantivy_result_blake3: String,
    pub overlap: u64,
    pub overlap_percent_of_fts: f64,
    pub top_10_overlap: u64,
    pub top_1_same: bool,
    pub filter_violations: u64,
    pub latency: LatencySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TantivyClassReport {
    pub class: String,
    pub latency: LatencySummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TantivyLaneReport {
    pub lane: String,
    pub latency: LatencySummary,
    pub classes: Vec<TantivyClassReport>,
    pub cases: Vec<TantivyCaseReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TantivyRunReport {
    pub format: u32,
    pub campaign_id: String,
    pub implementation: String,
    pub baseline_implementation: String,
    pub machine: String,
    pub os: String,
    pub arch: String,
    pub suite_name: String,
    pub suite_basis: String,
    pub database: DatabaseFingerprint,
    pub build: TantivyBuildReport,
    pub config: RunConfig,
    pub lane: TantivyLaneReport,
}
