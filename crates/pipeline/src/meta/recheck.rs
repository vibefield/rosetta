//! Recheck only a capture candidate's metadata dependencies. This index lives
//! with the roster, never in the projection or ledger. Withheld sources stay
//! in it: their fresh cwd may still exclude a sibling or a later child.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, StreamDriver};
use rosetta_capture::ScannedFile;
use rosetta_core::rows::RuntimeRow;
use rosetta_store::{Store, queries};

use super::{CwdAdmission, MetaOutput, MetaReport, SessionFile, exclusions, inventory};
use crate::{IngestOptions, Instrument, PipelineError};

#[derive(Debug, Default)]
pub struct AdmissionSources {
    complete: bool,
    sources: BTreeMap<String, BTreeMap<String, ScannedFile>>,
    parents: BTreeMap<String, BTreeSet<String>>,
}

pub struct AdmissionCheck {
    /// The original candidates whose identities still pass folder policy.
    pub files: Vec<SessionFile>,
    /// Includes dependent sessions outside this pass's capture candidates.
    pub excluded_sessions: BTreeSet<String>,
    /// A source changed identity; full admission must settle its new rows
    /// before it can be captured. Ordinary appends never take this path.
    pub requires_admission: bool,
}

impl AdmissionSources {
    pub(super) fn finish_inventory(&mut self) {
        self.complete = true;
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn observe_sources(&mut self, files: &[SessionFile]) {
        for file in files {
            let physical = match file.driver {
                StreamDriver::JsonLines => None,
                StreamDriver::DocumentSnapshot => {
                    crate::document::virtual_document_path(&file.relative_path)
                }
                StreamDriver::SqliteSnapshot => {
                    crate::snapshot::virtual_db_path(&file.relative_path)
                }
            }
            .unwrap_or(&file.relative_path);
            self.sources
                .entry(file.session_id.clone())
                .or_default()
                .insert(
                    physical.to_owned(),
                    ScannedFile {
                        relative_path: physical.to_owned(),
                        size: file.size,
                        modified_ms: file.modified_ms,
                        class: None,
                    },
                );
        }
    }

    pub(super) fn observe_parents(&mut self, admission: &CwdAdmission) {
        for (id, parents) in &admission.parents {
            self.parents
                .entry(id.clone())
                .or_default()
                .extend(parents.iter().cloned());
        }
    }

    /// Read each relevant physical source at most once, following both
    /// retained and newly skimmed ancestry until no dependency remains.
    /// Shared databases/documents are enumerated at their physical path.
    /// No metadata is journaled or applied by this check; retirement changes
    /// only the RAM publication gate for already interrupted sources.
    #[allow(clippy::too_many_arguments)]
    pub fn recheck(
        &mut self,
        adapter: &dyn AgentAdapter,
        runtime: &RuntimeRow,
        root: &Path,
        store: &mut Store,
        files: &[SessionFile],
        opts: &IngestOptions,
        instrument: &Instrument,
    ) -> Result<AdmissionCheck, PipelineError> {
        if opts.exclude_cwds.is_empty() || files.is_empty() {
            return Ok(AdmissionCheck {
                files: files.to_vec(),
                excluded_sessions: BTreeSet::new(),
                requires_admission: false,
            });
        }
        if !self.complete {
            return Ok(AdmissionCheck {
                files: Vec::new(),
                excluded_sessions: BTreeSet::new(),
                requires_admission: true,
            });
        }
        self.observe_sources(files);
        let mut output = MetaOutput {
            admission: CwdAdmission::new(opts.exclude_cwds.clone()),
            ..MetaOutput::default()
        };
        let mut report = MetaReport::default();
        let mut pending: Vec<_> = files.iter().map(|file| file.session_id.clone()).collect();
        let mut visited = BTreeSet::new();
        let mut read = BTreeSet::new();
        loop {
            let mut batch = BTreeMap::new();
            while let Some(id) = pending.pop() {
                if !visited.insert(id.clone()) {
                    continue;
                }
                if let Some(row) = queries::session(store.conn(), &id)? {
                    output.admission.observe_row(&row);
                }
                for parents in [self.parents.get(&id), output.admission.parents.get(&id)]
                    .into_iter()
                    .flatten()
                {
                    pending.extend(
                        parents
                            .iter()
                            .filter(|parent| !visited.contains(*parent))
                            .cloned(),
                    );
                }
                for (path, source) in self.sources.get(&id).into_iter().flatten() {
                    if !read.insert(path.clone()) {
                        continue;
                    }
                    let mut source = source.clone();
                    match std::fs::metadata(root.join(path)) {
                        Ok(stat) => {
                            source.size = stat.len();
                            source.modified_ms = stat
                                .modified()
                                .ok()
                                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                                .map_or(source.modified_ms, |time| time.as_millis() as i64);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(rosetta_capture::CaptureError::Io {
                                operation: "checking source admission",
                                path: root.join(path).display().to_string(),
                                source: error,
                            }
                            .into());
                        }
                    }
                    batch.insert(path.clone(), source);
                }
            }
            if batch.is_empty() {
                break;
            }
            let batch: Vec<_> = batch.into_values().collect();
            inventory::read_inventory(
                adapter,
                runtime,
                root,
                &batch,
                opts,
                instrument,
                1,
                &mut report,
                &mut output,
            )?;
            // A rewritten child can name a parent absent from its retained
            // row. Expand before filtering, including ledger-only sources.
            for id in &visited {
                if let Some(parents) = output.admission.parents.get(id) {
                    pending.extend(
                        parents
                            .iter()
                            .filter(|parent| !visited.contains(*parent))
                            .cloned(),
                    );
                }
            }
        }
        let identities: BTreeMap<_, _> = output
            .files
            .iter()
            .map(|file| (file.relative_path.as_str(), file.session_id.as_str()))
            .collect();
        let requires_admission = files.iter().any(|file| {
            identities
                .get(file.relative_path.as_str())
                .is_some_and(|id| *id != file.session_id)
        });
        self.observe_sources(&output.files);
        let mut admission = std::mem::take(&mut output.admission);
        let mut excluded = admission.filter(&mut output, store)?;
        self.observe_parents(&admission);
        if !excluded.is_empty() {
            // Retire pending descendants too, without reading unrelated
            // sources. Ordinary allowed appends do not walk this graph.
            let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (id, parents) in &self.parents {
                for parent in parents {
                    children.entry(parent).or_default().push(id);
                }
            }
            let mut pending: Vec<_> = excluded.iter().cloned().collect();
            while let Some(id) = pending.pop() {
                for child in children.get(id.as_str()).into_iter().flatten() {
                    if excluded.insert((*child).to_owned()) {
                        pending.push((*child).to_owned());
                    }
                }
            }
            exclusions::retire_interrupted_sources(
                &runtime.id,
                store,
                &opts.exclude_cwds,
                &excluded,
            )?;
        }
        let admitted: BTreeMap<_, _> = output
            .files
            .iter()
            .map(|file| (file.relative_path.as_str(), file.session_id.as_str()))
            .collect();
        let files = files
            .iter()
            .filter(|file| {
                !excluded.contains(&file.session_id)
                    && admitted.get(file.relative_path.as_str()) == Some(&file.session_id.as_str())
            })
            .cloned()
            .collect();
        Ok(AdmissionCheck {
            files,
            excluded_sessions: excluded,
            requires_admission,
        })
    }
}
