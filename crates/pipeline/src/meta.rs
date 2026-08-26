//! Phase 2 — meta ingest. Bounded head/tail skims only: project and session
//! lists must render before any transcript body is fully read. Output is one
//! batch of Runtime + Project + SessionMeta upserts, plus the file→session
//! map the stream phase keys its cursors by.

use std::path::Path;

use rosetta_adapter_support::{AgentAdapter, FileStat};
use rosetta_core::rows::{ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState};
use rosetta_core::{IngestOp, ids};
use rosetta_store::{ApplyStats, Batch, Store};

use crate::instrument::{Instrument, Metric};
use crate::{IngestOptions, PipelineError};

/// One source file the stream phase will tail, with the ids minted here.
#[derive(Debug, Clone)]
pub struct SessionFile {
    pub relative_path: String,
    pub stream_id: String,
    pub session_id: String,
    pub native_session_id: String,
    pub size: u64,
    pub modified_ms: i64,
    pub agent_version: Option<String>,
}

#[derive(Debug, Default)]
pub struct MetaReport {
    pub files_matched: usize,
    pub files_skimmed: usize,
    /// Matched files whose session could not be identified from the bounded
    /// windows — the stream phase will still visit them later; they are
    /// reported, never silently dropped.
    pub files_unidentified: usize,
    pub sessions: usize,
    pub projects: usize,
    pub stats: ApplyStats,
}

pub fn meta_ingest(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<(MetaReport, Vec<SessionFile>), PipelineError> {
    let layout = adapter.layout();
    let walk = instrument.time(Metric::WalkNanos, || {
        rosetta_capture::walk(root, opts.max_files)
    })?;

    let mut report = MetaReport::default();
    let mut files = Vec::new();
    let mut projects: Vec<ProjectRow> = Vec::new();
    let mut sessions: Vec<SessionRow> = Vec::new();

    for file in &walk.files {
        let Some(stream) = layout.matching_stream(&file.relative_path) else {
            continue;
        };
        report.files_matched += 1;

        let Some(windows) = instrument.time(Metric::SkimNanos, || {
            rosetta_capture::head_tail(
                &root.join(&file.relative_path),
                opts.skim_head_bytes,
                opts.skim_tail_bytes,
            )
        })?
        else {
            continue; // vanished between walk and skim — the stream phase's problem
        };
        report.files_skimmed += 1;
        instrument.add(Metric::FilesSkimmed, 1);
        instrument.add(
            Metric::BytesRead,
            (windows.head.len() + windows.tail.len()) as u64,
        );

        let stat = FileStat {
            relative_path: file.relative_path.clone(),
            size: file.size,
            modified_ms: file.modified_ms,
        };
        let skim = adapter.skim(&stat, &windows.head, &windows.tail);
        let Some(native_session_id) = skim.session_id else {
            report.files_unidentified += 1;
            continue;
        };

        let session_id = ids::session_id(&runtime.id, &native_session_id);
        let project_id = skim.cwd.as_deref().map(|cwd| {
            let id = ids::project_id(cwd, None);
            if !projects.iter().any(|project| project.id == id) {
                projects.push(ProjectRow {
                    id: id.clone(),
                    name: project_name(cwd),
                    root_paths: vec![cwd.to_string()],
                    git_repo: None,
                });
            }
            id
        });

        // One session can span several files (or reappear across runs) —
        // keep the newest skim's view of the mutable fields.
        let row = SessionRow {
            id: session_id.clone(),
            native_session_id: native_session_id.clone(),
            runtime_id: runtime.id.clone(),
            project_id,
            parent_session_id: None,
            root_session_id: session_id.clone(),
            role: SessionRole::Main,
            title: skim.title,
            summary: None,
            started_at: skim.started_at.unwrap_or(file.modified_ms),
            updated_at: file.modified_ms,
            stream_state: StreamState::Pending,
        };
        match sessions.iter_mut().find(|existing| existing.id == row.id) {
            Some(existing) => {
                if row.updated_at >= existing.updated_at {
                    let started_at = existing.started_at.min(row.started_at);
                    *existing = row;
                    existing.started_at = started_at;
                }
            }
            None => sessions.push(row),
        }

        files.push(SessionFile {
            relative_path: file.relative_path.clone(),
            stream_id: stream.id.clone(),
            session_id,
            native_session_id,
            size: file.size,
            modified_ms: file.modified_ms,
            agent_version: skim.agent_version,
        });
    }

    // A re-skim must never regress ingest progress: a session the stream
    // phase already indexed stays indexed.
    for session in &mut sessions {
        if let Some(existing) = rosetta_store::queries::session(store.conn(), &session.id)? {
            session.stream_state = existing.stream_state;
        }
    }

    report.sessions = sessions.len();
    report.projects = projects.len();
    instrument.add(Metric::FilesMatched, report.files_matched as u64);
    instrument.add(Metric::SessionsDiscovered, report.sessions as u64);

    // FK order: runtime → projects → sessions, one transaction.
    let mut ops: Vec<IngestOp> = Vec::with_capacity(1 + projects.len() + sessions.len());
    ops.push(IngestOp::Runtime(runtime.clone()));
    ops.extend(projects.into_iter().map(IngestOp::Project));
    ops.extend(sessions.into_iter().map(IngestOp::SessionMeta));
    report.stats = instrument.time(Metric::ApplyNanos, || {
        store.apply(&Batch {
            ops,
            checkpoints: vec![],
        })
    })?;
    instrument.add(Metric::BatchesApplied, 1);

    // Newest sessions stream first.
    files.sort_by_key(|file| std::cmp::Reverse(file.modified_ms));
    Ok((report, files))
}

fn project_name(cwd: &str) -> String {
    cwd.rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(cwd)
        .to_string()
}
