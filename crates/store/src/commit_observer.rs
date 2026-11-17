//! A test harness hook: a callback the writer runs right after each of its
//! transactions commits (R1 brief §10, the per-commit visibility audit).
//!
//! The callback learns only which kind of transaction committed. It runs on
//! the writer's own thread with no transaction open, so it may read the
//! shared RAM generation through its own connection — the projection exactly
//! as a reader sees it between two writer commits — or park the writer until
//! a reader thread has read. Production never installs one.

use crate::Store;

/// Which writer transaction just committed.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitKind {
    /// [`Store::apply`]: metadata, setup and live batches.
    Apply,
    /// The terminal transaction of closed sources: one coalesced item, or a
    /// chunked source's last transaction.
    Sources,
    /// One Stage slice of a chunked source.
    StageSlice,
    /// One resolved-row slice of a chunked source.
    ResolvedSlice,
    /// [`Store::admit_cold_units`]: a flip batch, or a runtime's remainder.
    Admission,
    /// A runtime's admission fold, or the buffered bulk fold.
    Fold,
    /// A standalone renumber.
    Renumber,
    /// A standalone readiness change.
    StreamStates,
    /// Finalization's refold (and, off the shared generation, its indexes).
    Finalize,
    /// Finalization's DDL transaction on the shared generation (W2).
    FinalizeIndexes,
}

/// The callback [`Store::set_commit_observer`] installs.
#[doc(hidden)]
pub type CommitObserver = Box<dyn FnMut(CommitKind) + Send>;

impl Store {
    /// Install (or with `None` remove) a callback run after every committed
    /// writer transaction. Tests only.
    #[doc(hidden)]
    pub fn set_commit_observer(&mut self, observer: Option<CommitObserver>) {
        self.commit_observer = observer;
    }

    /// A writer transaction of `kind` committed.
    pub(crate) fn committed(&mut self, kind: CommitKind) {
        if let Some(observer) = self.commit_observer.as_mut() {
            observer(kind);
        }
    }
}
