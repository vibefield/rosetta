//! Replay's join state for one source: the presented-media facts its raw
//! records normalized to, waiting for the frame that captured each file.
//! RAM only; a rebuilt replay notes them again from the same records.

use std::collections::BTreeMap;

use rosetta_core::IngestOp;
use rosetta_core::presented_media::PresentedMedia;

/// Facts one source may hold at once.
const SLOTS_MAX: usize = 65_536;

/// Facts keyed `(source_offset, path)`: the coordinate a media frame names.
#[derive(Debug)]
pub(crate) struct Slots {
    facts: BTreeMap<(u64, String), PresentedMedia>,
    cap: usize,
}

impl Default for Slots {
    fn default() -> Self {
        Self::with_cap(SLOTS_MAX)
    }
}

impl Slots {
    pub(crate) fn with_cap(cap: usize) -> Self {
        Self {
            facts: BTreeMap::new(),
            cap,
        }
    }

    /// Keep every fact among a record's ops. The first fact at a coordinate
    /// stays, as capture observes a coordinate once. Past the cap the lowest
    /// coordinate goes: its frame, if any, has been met or will be ignored.
    pub(crate) fn note(&mut self, ops: &[IngestOp]) {
        for op in ops {
            let IngestOp::PresentedMedia(fact) = op else {
                continue;
            };
            self.facts
                .entry((fact.source_offset, fact.path.clone()))
                .or_insert_with(|| fact.clone());
            while self.facts.len() > self.cap {
                self.facts.pop_first();
            }
        }
    }

    pub(crate) fn get(&self, offset: u64, path: &str) -> Option<&PresentedMedia> {
        // A borrowed lookup would need a second key type; a media frame is
        // rare enough that one path copy is not worth it.
        self.facts.get(&(offset, path.to_owned()))
    }

    pub(crate) fn remove(&mut self, offset: u64, path: &str) {
        self.facts.remove(&(offset, path.to_owned()));
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.facts.len()
    }
}
