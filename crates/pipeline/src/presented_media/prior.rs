//! Earlier captures of one source, read back from the ledger so a pass that
//! re-reads a file can carry a captured media frame forward instead of
//! observing today's filesystem.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rosetta_capture::{
    CaptureError, CapturedDocument, DocumentCaptureStatus, LedgerEntry, MediaClaim,
    segment_filter_through, visit_ledger_segments,
};

/// Earlier bytes one pass may hold while it carries captures forward.
pub(crate) const PRIOR_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// One captured media frame of an earlier pass, at its ledger coordinate.
#[derive(Debug, Clone)]
pub(crate) struct Prior {
    pub generation: u64,
    pub offset: u64,
    pub document: CapturedDocument,
}

/// `(offset, path, media type, expected length)`: one claim at one record.
type Key = (u64, String, String, u64);

/// The captured media frames of one source, newest generation per claim,
/// within a byte budget that older generations give way in.
#[derive(Debug)]
pub(crate) struct Loaded {
    entries: BTreeMap<Key, Prior>,
    bytes: u64,
    budget: u64,
}

impl Default for Loaded {
    fn default() -> Self {
        Self::with_budget(PRIOR_MAX_BYTES)
    }
}

impl Loaded {
    pub(crate) fn with_budget(bytes: u64) -> Self {
        Self {
            entries: BTreeMap::new(),
            bytes: 0,
            budget: bytes,
        }
    }

    /// Keep a captured media frame. Anything else is ignored: an HTML
    /// document, an observation that captured nothing. A higher generation
    /// replaces the frame held for the same claim at the same offset; an
    /// equal or lower one is ignored, so the first frame inserted within a
    /// generation wins.
    ///
    /// The retained bytes never exceed the budget. A frame that would take
    /// them past it makes room among the frames of strictly lower
    /// generations, lowest generation first: frames arrive in ledger order,
    /// so without this the copies each rewrite carried forward would spend
    /// the budget before a newer generation's only capture of a file
    /// arrived. A frame is refused only when that room is not enough, and
    /// then nothing is given up for it.
    pub(crate) fn insert(&mut self, prior: Prior) {
        if prior.document.status != DocumentCaptureStatus::Captured {
            return;
        }
        let Some(claim) = &prior.document.media else {
            return;
        };
        let key = (
            prior.offset,
            prior.document.path.clone(),
            claim.media_type.clone(),
            claim.expected_len,
        );
        let incoming = prior.document.bytes.len() as u64;
        let held = match self.entries.get(&key) {
            Some(current) if current.generation >= prior.generation => return,
            Some(current) => current.document.bytes.len() as u64,
            None => 0,
        };
        let others = self.bytes - held;
        let over = others.saturating_add(incoming).saturating_sub(self.budget);
        let Some(evicted) = self.room_for(over, prior.generation, &key) else {
            return;
        };
        for key in evicted {
            if let Some(gone) = self.entries.remove(&key) {
                self.bytes -= gone.document.bytes.len() as u64;
            }
        }
        self.bytes = self.bytes - held + incoming;
        self.entries.insert(key, prior);
    }

    /// The held frames to give up so `over` more bytes fit: frames of a
    /// generation below `generation`, lowest generation first (then lowest
    /// offset), never the frame at `replaced`, whose bytes the caller has
    /// already counted as freed. `None` when they do not free enough.
    fn room_for(&self, mut over: u64, generation: u64, replaced: &Key) -> Option<Vec<Key>> {
        if over == 0 {
            return Some(Vec::new());
        }
        let mut older: Vec<(u64, &Key, u64)> = self
            .entries
            .iter()
            .filter(|(key, held)| {
                held.generation < generation && *key != replaced && !held.document.bytes.is_empty()
            })
            .map(|(key, held)| (held.generation, key, held.document.bytes.len() as u64))
            .collect();
        older.sort();
        let mut evicted = Vec::new();
        for (_, key, len) in older {
            if over == 0 {
                break;
            }
            over = over.saturating_sub(len);
            evicted.push(key.clone());
        }
        (over == 0).then_some(evicted)
    }

    /// The capture to carry for a claim met at `offset`: the frame at that
    /// exact coordinate if one is held; otherwise, among the frames of the
    /// same path and claim, the newest generation, then the lowest offset.
    pub(crate) fn find(&self, offset: u64, path: &str, claim: &MediaClaim) -> Option<&Prior> {
        let exact = (
            offset,
            path.to_owned(),
            claim.media_type.clone(),
            claim.expected_len,
        );
        if let Some(prior) = self.entries.get(&exact) {
            return Some(prior);
        }
        self.entries
            .values()
            .filter(|prior| {
                prior.document.path == path && prior.document.media.as_ref() == Some(claim)
            })
            .max_by(|a, b| {
                a.generation
                    .cmp(&b.generation)
                    .then_with(|| b.offset.cmp(&a.offset))
            })
    }
}

/// Reads one source's earlier captures from a runtime's ledger directory.
/// Read-only: it opens segments and the segment index for reading and writes
/// neither, so the runtime's own writer may hold the open segment meanwhile.
pub(crate) struct PriorCaptures {
    dir: PathBuf,
}

impl PriorCaptures {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// The captured media frames of one source in generations up to and
    /// including `through`, within [`PRIOR_MAX_BYTES`].
    ///
    /// Never fails the pass. An unreadable index or segment gives an empty
    /// answer, which only means the filesystem is observed; a partial one
    /// could prefer an older frame over a newer one it did not reach.
    pub(crate) fn load(&self, session_id: &str, relative_path: &str, through: u64) -> Loaded {
        self.load_within(session_id, relative_path, through, PRIOR_MAX_BYTES)
    }

    /// [`PriorCaptures::load`] under an explicit byte budget.
    ///
    /// One scoped walk in ledger order. The index only narrows it: a segment
    /// the index does not list (the open one, one closed by a crash, every
    /// one when there is no index) is walked. A torn tail ends its own
    /// segment's walk and hides nothing in the others, which is how the open
    /// segment reads while its writer has frames still buffered.
    fn load_within(
        &self,
        session_id: &str,
        relative_path: &str,
        through: u64,
        budget: u64,
    ) -> Loaded {
        let Ok(filter) = segment_filter_through(&self.dir, session_id, relative_path, through)
        else {
            return Loaded::with_budget(budget);
        };
        let mut loaded = Loaded::with_budget(budget);
        let walked = visit_ledger_segments::<CaptureError, _>(
            &self.dir,
            |segment| filter.admits(segment),
            |entry| {
                if let LedgerEntry::Document {
                    source,
                    generation,
                    start_offset,
                    document,
                } = entry
                    && generation <= through
                    && source.session_id == session_id
                    && source.relative_path == relative_path
                    && document.media.is_some()
                    && document.status == DocumentCaptureStatus::Captured
                {
                    loaded.insert(Prior {
                        generation,
                        offset: start_offset,
                        document,
                    });
                }
                Ok(())
            },
        );
        match walked {
            Ok(_) => loaded,
            Err(_) => Loaded::with_budget(budget),
        }
    }
}

#[cfg(test)]
mod tests;
