//! Bounded, process-local observation of an unpublished search generation.

use serde::{Deserialize, Serialize};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum SearchBuildStage {
    Indexing,
    Optimizing,
    Validating,
    Publishing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SearchBuildProgress {
    pub stage: SearchBuildStage,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub documents_done: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub documents_total: u64,
}

#[derive(Debug, Default)]
pub(crate) struct BuildProgress {
    // The epoch prevents an old worker's progress from describing a new archive.
    state: Mutex<Option<(String, SearchBuildStage, u64)>>,
    documents: AtomicU64,
}

impl BuildProgress {
    pub fn start(&self, epoch: &str, total: u64) {
        let mut state = self.state.lock().expect("search progress poisoned");
        self.documents.store(0, Ordering::Relaxed);
        *state = Some((epoch.to_owned(), SearchBuildStage::Indexing, total));
    }

    pub fn documents(&self, done: u64) {
        self.documents.store(done, Ordering::Relaxed);
    }

    pub fn stage(&self, stage: SearchBuildStage) {
        if let Some((_, current, _)) = self
            .state
            .lock()
            .expect("search progress poisoned")
            .as_mut()
        {
            *current = stage;
        }
    }

    pub fn clear(&self) {
        *self.state.lock().expect("search progress poisoned") = None;
    }

    pub fn snapshot(&self, epoch: Option<&str>) -> Option<SearchBuildProgress> {
        let state = self.state.lock().expect("search progress poisoned");
        let (owner, stage, total) = state.as_ref()?;
        if epoch != Some(owner.as_str()) {
            return None;
        }
        Some(SearchBuildProgress {
            stage: *stage,
            documents_done: self.documents.load(Ordering::Relaxed),
            documents_total: *total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_scoped_to_its_generation_and_resets_on_retry() {
        let progress = BuildProgress::default();
        assert_eq!(progress.snapshot(Some("a")), None);
        progress.start("a", 1_000);
        progress.documents(512);
        assert_eq!(progress.snapshot(Some("a")).unwrap().documents_done, 512);
        assert_eq!(progress.snapshot(Some("b")), None);
        progress.stage(SearchBuildStage::Validating);
        assert_eq!(
            progress.snapshot(Some("a")).unwrap().stage,
            SearchBuildStage::Validating
        );
        progress.start("a", 2_000);
        assert_eq!(progress.snapshot(Some("a")).unwrap().documents_done, 0);
        progress.clear();
        assert_eq!(progress.snapshot(Some("a")), None);
    }
}
