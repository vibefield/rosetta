//! The writer's setup for one streamed runtime, and the unit it admits in.

use std::collections::BTreeSet;
use std::sync::Arc;

use rosetta_adapter_support::ColdAdmission;
use rosetta_capture::SourceDef;
use rosetta_core::IngestOp;
use rosetta_store::Store;

use super::source_def;
use super::trees::{SessionLink, TreeMap};
use crate::PipelineError;
use crate::meta::SessionFile;

/// What the canonical writer needs before a streamed runtime's first source:
/// every roster source (its cursor row and, for a session no metadata batch
/// named, its session stub), in source-path order, and the unit its sessions
/// are admitted in.
#[derive(Debug)]
pub struct StreamedSetup {
    pub(crate) sources: Vec<SourceDef>,
    pub(crate) metadata_sessions: BTreeSet<String>,
    /// The admission the runtime asked for: its adapter's declaration, or
    /// [`ColdAdmission::Runtime`] under the kill switch.
    pub(crate) admission: ColdAdmission,
    /// The runtime's root trees: `Some` exactly when it is admitted per tree,
    /// that is, it asked for [`ColdAdmission::RootTree`] and its discovery and
    /// metadata walks were complete.
    pub(crate) trees: Option<Arc<TreeMap>>,
}

impl StreamedSetup {
    pub fn sources(&self) -> usize {
        self.sources.len()
    }

    /// The admission the runtime asked for. Whether it got it is
    /// [`StreamedSetup::trees`]: `None` after a fallback.
    pub fn admission(&self) -> ColdAdmission {
        self.admission
    }

    /// The root trees of a runtime admitted per tree; `None` for runtime
    /// admission, declared or fallen back to.
    pub fn trees(&self) -> Option<&Arc<TreeMap>> {
        self.trees.as_ref()
    }

    /// A runtime that asked for root trees but is admitted whole, because a
    /// truncated walk leaves its trees unproven.
    pub fn fell_back(&self) -> bool {
        self.admission == ColdAdmission::RootTree && self.trees.is_none()
    }
}

/// Close the planner's metadata phase: take the metadata batches it retained
/// (the caller has already published identical rows to the canonical store)
/// and describe the runtime's sources for the writer's setup. The runtime is
/// admitted whole, in path order: [`streamed_setup_for`] with
/// [`ColdAdmission::Runtime`].
pub fn streamed_setup(
    files: &[SessionFile],
    planner: &mut Store,
) -> Result<StreamedSetup, PipelineError> {
    streamed_setup_for(files, planner, ColdAdmission::Runtime, false)
}

/// [`streamed_setup`] for a runtime that asks for `admission`. A root-tree
/// runtime also gets its [`TreeMap`], built from the roster and the
/// `SessionMeta` links its metadata batches carry, unless `truncated` (its
/// discovery or metadata walk stopped early): an unseen file could be a
/// missing link, so such a runtime falls back to runtime admission before
/// its producer starts.
pub fn streamed_setup_for(
    files: &[SessionFile],
    planner: &mut Store,
    admission: ColdAdmission,
    truncated: bool,
) -> Result<StreamedSetup, PipelineError> {
    let per_tree = admission == ColdAdmission::RootTree && !truncated;
    let mut metadata_sessions = BTreeSet::new();
    let mut links: Vec<SessionLink> = Vec::new();
    for batch in planner.drain_planned_batches()? {
        if batch.source.is_some() {
            return Err(PipelineError::Config(
                "cold planner captured a source before its streamed setup".to_string(),
            ));
        }
        for op in &batch.ops {
            if let IngestOp::SessionMeta(row) = op {
                metadata_sessions.insert(row.id.clone());
                if per_tree {
                    links.push((
                        row.id.clone(),
                        row.parent_session_id.clone(),
                        Some(row.root_session_id.clone()),
                    ));
                }
            }
        }
    }
    let mut sources = files.iter().map(source_def).collect::<Vec<_>>();
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if let Some(pair) = sources
        .windows(2)
        .find(|pair| pair[0].relative_path == pair[1].relative_path)
    {
        return Err(PipelineError::Config(format!(
            "cold roster names source {} more than once",
            pair[0].relative_path
        )));
    }
    Ok(StreamedSetup {
        sources,
        metadata_sessions,
        admission,
        trees: per_tree.then(|| Arc::new(TreeMap::build(files, &links))),
    })
}
