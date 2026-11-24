//! Early cold restores could reuse the number of a rejected generation.
//! A failed seal, a new opening record at offset zero and a subsequent clean
//! seal identify a complete replacement. A retry without that final evidence
//! must not displace the archive's existing generation.

use std::collections::BTreeMap;

use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{LedgerEntry, SourceDef};

use super::admission::AdmissionScan;

struct FailedSeal {
    generation: u64,
    /// Ledger position and admission of the latest retry from byte zero.
    opening: Option<(u64, bool)>,
}

#[derive(Default)]
pub(super) struct RetryScan {
    failed: BTreeMap<String, FailedSeal>,
    pub(super) replacements: Boundaries,
}

impl RetryScan {
    pub(super) fn observe_opening(
        &mut self,
        admission: &mut AdmissionScan,
        adapter: &dyn AgentAdapter,
        source: &SourceDef,
        generation: u64,
        payload: &[u8],
        position: u64,
    ) {
        if let Some(failed) = self.failed.get_mut(&source.relative_path)
            && failed.generation == generation
        {
            failed.opening = Some((
                position,
                AdmissionScan::opening_excludes(adapter, source, payload),
            ));
        } else {
            admission.observe(adapter, source, generation, 0, payload);
        }
    }

    pub(super) fn observe_seal(
        &mut self,
        admission: &mut AdmissionScan,
        source: &SourceDef,
        generation: u64,
        clean: bool,
    ) {
        if !clean {
            self.failed.insert(
                source.relative_path.clone(),
                FailedSeal {
                    generation,
                    opening: None,
                },
            );
        } else if let Some(failed) = self.failed.remove(&source.relative_path)
            && failed.generation == generation
            && let Some((start, excluded)) = failed.opening
        {
            self.replacements
                .0
                .insert(source.relative_path.clone(), (generation, start));
            admission.replace_opening(source, generation, excluded);
        }
    }
}

/// First entry in a complete replacement whose number had been reused.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Boundaries(BTreeMap<String, (u64, u64)>);

impl Boundaries {
    pub(super) fn skips(&self, entry: &LedgerEntry, position: u64) -> bool {
        if self.0.is_empty() {
            return false;
        }
        let (source, generation) = match entry {
            LedgerEntry::Raw {
                source, generation, ..
            }
            | LedgerEntry::Document {
                source, generation, ..
            }
            | LedgerEntry::GenerationComplete {
                source, generation, ..
            } => (source, generation),
            LedgerEntry::Meta { .. } => return false,
        };
        self.0
            .get(&source.relative_path)
            .is_some_and(|(reused, start)| reused == generation && position < *start)
    }
}
