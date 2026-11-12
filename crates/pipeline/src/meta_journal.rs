//! Versioned metadata journal. Required source identifiers preserve an
//! incomplete transcript's readiness across native-file deletion and rebuild.
use crate::{PipelineError, SessionFile};
use rosetta_core::IngestOp;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRequirement {
    pub path: String,
    pub session_id: String,
    pub available: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LedgerMetadata {
    pub ops: Vec<IngestOp>,
    pub required_sources: Vec<SourceRequirement>,
}
impl LedgerMetadata {
    pub fn decode(payload: &str) -> Result<Self, PipelineError> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Compatible {
            Legacy(Vec<IngestOp>),
            Current(LedgerMetadata),
        }
        match serde_json::from_str(payload)
            .map_err(|e| PipelineError::Config(format!("undecodable meta batch in ledger: {e}")))?
        {
            Compatible::Legacy(ops) => Ok(Self {
                ops,
                required_sources: vec![],
            }),
            Compatible::Current(meta) => Ok(meta),
        }
    }
    pub(crate) fn new(ops: Vec<IngestOp>, files: &[SessionFile]) -> Self {
        let required_sources = files
            .iter()
            .filter(|f| !f.ledger_only)
            .filter_map(|file| {
                file.input.as_ref().map(|input| SourceRequirement {
                    path: file.relative_path.clone(),
                    session_id: file.session_id.clone(),
                    available: !input.segments.is_empty(),
                })
            })
            .collect();
        Self {
            ops,
            required_sources,
        }
    }
}

pub(crate) fn apply_requirements(
    requirements: &[SourceRequirement],
    present: &BTreeSet<String>,
    health: &mut BTreeMap<String, bool>,
) {
    for required in requirements {
        if !required.available || !present.contains(&required.path) {
            health.insert(required.session_id.clone(), false);
        }
    }
}
