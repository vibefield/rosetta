//! Phase 2 — meta ingest. Bounded head/tail skims only: project and session
//! lists must render before any transcript body is fully read. Output is one
//! batch of Runtime + Project + SessionMeta upserts, plus the file→session
//! map the stream phase keys its cursors by.

use std::path::Path;

use rosetta_adapter_support::{
    AgentAdapter, FileDiscipline, FileFormat, FileStat, SkimResult, StreamDriver,
};
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
    pub file_format: FileFormat,
    pub discipline: FileDiscipline,
    pub driver: StreamDriver,
    pub session_id: String,
    pub native_session_id: String,
    pub size: u64,
    pub modified_ms: i64,
    pub agent_version: Option<String>,
}

#[derive(Debug, Default)]
pub struct MetaReport {
    pub files_matched: usize,
    /// Layout-owned files that have no normalization driver yet. They remain
    /// inventory, never accidental transcript input.
    pub files_inventory_only: usize,
    pub files_skimmed: usize,
    /// Matched streamable files whose session could not be identified even
    /// after the bounded first-record retry. They are reported and withheld
    /// from streaming; the CLI treats any such file as an incomplete ingest.
    pub files_unidentified: usize,
    pub files_vanished: usize,
    pub walk_truncated: bool,
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
    meta_ingest_into(adapter, runtime, root, store, opts, instrument, None)
}

/// [`meta_ingest`] with a raw ledger attached: the meta ops batch is
/// journaled (and durably flushed) before it is applied, so a rebuild after
/// the agent GC'd its files still has the runtime, project, and session
/// facts that only bounded skims ever saw.
#[allow(clippy::too_many_arguments)]
pub fn meta_ingest_into(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    opts: &IngestOptions,
    instrument: &Instrument,
    ledger: Option<&mut rosetta_capture::Ledger>,
) -> Result<(MetaReport, Vec<SessionFile>), PipelineError> {
    let layout = adapter.layout();
    let walk = instrument.time(Metric::WalkNanos, || {
        rosetta_capture::walk_filtered(root, opts.max_files, |path| {
            layout.matching_stream(path).is_some()
        })
    })?;

    let mut report = MetaReport::default();
    let mut files = Vec::new();
    let mut projects: Vec<ProjectRow> = Vec::new();
    let mut sessions: Vec<SessionRow> = Vec::new();
    let mut skim_ops: Vec<IngestOp> = Vec::new();
    report.walk_truncated = walk.truncated;

    for file in &walk.files {
        let Some(stream) = layout.matching_stream(&file.relative_path) else {
            continue;
        };
        report.files_matched += 1;

        let Some(driver) = adapter.stream_driver(&stream.id) else {
            report.files_inventory_only += 1;
            continue;
        };
        match driver {
            StreamDriver::JsonLines
                if stream.file_format == FileFormat::Jsonl
                    && stream.discipline == FileDiscipline::AppendOnly => {}
            StreamDriver::SqliteSnapshot
                if stream.file_format == FileFormat::Sqlite
                    && stream.discipline == FileDiscipline::Database =>
            {
                // Sidecars (-wal/-shm/-journal) travel with their database;
                // only the database itself is a source.
                if rosetta_capture::is_db_sidecar(&file.relative_path) {
                    report.files_inventory_only += 1;
                    continue;
                }
                admit_database(
                    adapter,
                    runtime,
                    root,
                    store,
                    stream,
                    driver,
                    &file.relative_path,
                    file.size,
                    file.modified_ms,
                    &mut report,
                    &mut projects,
                    &mut sessions,
                    &mut files,
                    instrument,
                )?;
                continue;
            }
            _ => {
                return Err(PipelineError::Config(format!(
                    "{} opted stream {} into {:?}, but its layout is {:?}/{:?}",
                    adapter.kind(),
                    stream.id,
                    driver,
                    stream.file_format,
                    stream.discipline
                )));
            }
        }

        let Some(windows) = instrument.time(Metric::SkimNanos, || {
            rosetta_capture::head_tail(
                &root.join(&file.relative_path),
                opts.skim_head_bytes,
                opts.skim_tail_bytes,
            )
        })?
        else {
            report.files_vanished += 1;
            continue;
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
        let mut skim = adapter.skim(&stream.id, &stat, &windows.head, &windows.tail);
        // Some opening records legitimately exceed the UI skim window (Codex
        // session_meta can embed AGENTS.md). Read exactly one bounded record,
        // never the transcript body, before declaring the file unidentified.
        if skim.session_id.is_none()
            && let Some(mut first) = instrument.time(Metric::SkimNanos, || {
                rosetta_capture::first_record(
                    &root.join(&file.relative_path),
                    opts.tail.delimiter,
                    opts.tail.max_record_bytes,
                    opts.tail.normalize_crlf,
                )
            })?
        {
            first.push(opts.tail.delimiter);
            instrument.add(Metric::BytesRead, first.len() as u64);
            skim = adapter.skim(&stream.id, &stat, &first, &windows.tail);
        }
        skim_ops.extend(std::mem::take(&mut skim.ops));
        let Some(native_session_id) = skim.session_id.clone() else {
            report.files_unidentified += 1;
            continue;
        };
        admit_identified(
            runtime,
            &mut projects,
            &mut sessions,
            &mut files,
            AdmitSource {
                relative_path: file.relative_path.clone(),
                stream_id: stream.id.clone(),
                file_format: stream.file_format,
                discipline: stream.discipline,
                driver,
                size: file.size,
                modified_ms: file.modified_ms,
            },
            skim,
            native_session_id,
        );
    }

    // A re-skim must never regress ingest progress: a session the stream
    // phase already indexed stays indexed.
    for session in &mut sessions {
        if let Some(existing) = rosetta_store::queries::session(store.conn(), &session.id)? {
            session.stream_state = existing.stream_state;
        }
    }
    sessions.sort_by_key(|session| !matches!(session.role, SessionRole::Main));

    report.sessions = sessions.len();
    report.projects = projects.len();
    instrument.add(Metric::FilesMatched, report.files_matched as u64);
    instrument.add(Metric::SessionsDiscovered, report.sessions as u64);

    // FK order: runtime → projects → sessions, one transaction.
    let mut ops: Vec<IngestOp> = Vec::with_capacity(1 + projects.len() + sessions.len());
    ops.push(IngestOp::Runtime(runtime.clone()));
    ops.extend(projects.into_iter().map(IngestOp::Project));
    ops.extend(sessions.into_iter().map(IngestOp::SessionMeta));
    ops.extend(skim_ops);
    if let Some(ledger) = ledger {
        instrument.ledger_write(ledger, |ledger| -> Result<(), PipelineError> {
            let json = serde_json::to_string(&ops).map_err(|error| {
                PipelineError::Config(format!("serializing meta ops for the ledger: {error}"))
            })?;
            ledger.append_meta(crate::LEDGER_META_SCHEMA_VERSION, &json)?;
            ledger.flush()?;
            Ok(())
        })?;
    }
    report.stats = instrument.apply(
        store,
        &Batch {
            ops,
            checkpoints: vec![],
            source: None,
        },
    )?;
    instrument.add(Metric::BatchesApplied, 1);

    // Newest sessions stream first.
    files.sort_by_key(|file| std::cmp::Reverse(file.modified_ms));
    Ok((report, files))
}

fn merge_session(sessions: &mut Vec<SessionRow>, row: SessionRow) {
    match sessions.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => {
            let started_at = existing.started_at.min(row.started_at);
            if row.updated_at >= existing.updated_at
                || (existing.role == SessionRole::Main && row.role != SessionRole::Main)
            {
                *existing = row;
            } else {
                existing.project_id = existing.project_id.clone().or(row.project_id);
                existing.parent_session_id =
                    existing.parent_session_id.clone().or(row.parent_session_id);
            }
            existing.started_at = started_at;
        }
        None => sessions.push(row),
    }
}

/// Everything the admission of one identified session needs to know about
/// its source, shared by the skim path and the database path.
struct AdmitSource {
    relative_path: String,
    stream_id: String,
    file_format: FileFormat,
    discipline: FileDiscipline,
    driver: StreamDriver,
    size: u64,
    modified_ms: i64,
}

/// Mint canonical ids from native facts and stage the session, project, and
/// source-file rows. Identical semantics for a skimmed transcript and a
/// database-enumerated session.
fn admit_identified(
    runtime: &RuntimeRow,
    projects: &mut Vec<ProjectRow>,
    sessions: &mut Vec<SessionRow>,
    files: &mut Vec<SessionFile>,
    source: AdmitSource,
    skim: SkimResult,
    native_session_id: String,
) {
    let session_id = ids::session_id(&runtime.id, &native_session_id);

    let project_id = skim.cwd.as_deref().map(|cwd| {
        let project = crate::project::project(cwd);
        let id = project.id.clone();
        if !projects.iter().any(|existing| existing.id == id) {
            projects.push(project);
        }
        id
    });

    let parent_session_id = skim
        .parent_session_id
        .as_deref()
        .map(|native| ids::session_id(&runtime.id, native));
    let root_session_id = skim
        .root_session_id
        .as_deref()
        .map(|native| ids::session_id(&runtime.id, native))
        .or_else(|| parent_session_id.clone())
        .unwrap_or_else(|| session_id.clone());
    let role = skim.role.unwrap_or(SessionRole::Main);

    // A retained subagent transcript can outlive its parent's transcript.
    // Materialize an honest parent placeholder so the relationship and FK
    // survive GC; a later main skim refines the same deterministic row.
    if let (Some(parent_native), Some(parent_id)) = (
        skim.parent_session_id.as_deref(),
        parent_session_id.as_ref(),
    ) {
        merge_session(
            sessions,
            SessionRow {
                id: parent_id.clone(),
                native_session_id: parent_native.to_owned(),
                runtime_id: runtime.id.clone(),
                project_id: project_id.clone(),
                parent_session_id: None,
                root_session_id: root_session_id.clone(),
                role: SessionRole::Main,
                title: None,
                summary: None,
                started_at: skim.started_at.unwrap_or(source.modified_ms),
                updated_at: source.modified_ms,
                stream_state: StreamState::Pending,
            },
        );
    }

    // One session can span several files (or reappear across runs) —
    // keep the newest skim's view of the mutable fields.
    let row = SessionRow {
        id: session_id.clone(),
        native_session_id: native_session_id.clone(),
        runtime_id: runtime.id.clone(),
        project_id,
        parent_session_id,
        root_session_id,
        role,
        title: skim.title,
        summary: None,
        started_at: skim.started_at.unwrap_or(source.modified_ms),
        updated_at: source.modified_ms,
        stream_state: StreamState::Pending,
    };
    merge_session(sessions, row);

    files.push(SessionFile {
        relative_path: source.relative_path,
        stream_id: source.stream_id,
        file_format: source.file_format,
        discipline: source.discipline,
        driver: source.driver,
        session_id,
        native_session_id,
        size: source.size,
        modified_ms: source.modified_ms,
        agent_version: skim.agent_version,
    });
}

/// Meta admission for a `SqliteSnapshot` stream: enumerate the database's
/// sessions via the adapter's declared plan and admit each as a VIRTUAL
/// source (`<db-path>#<native-id>`) — its own cursor, its own generation
/// swaps, its own ledger entries, exactly like a file.
#[allow(clippy::too_many_arguments)]
fn admit_database(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &Store,
    stream: &rosetta_adapter_support::StreamSpec,
    driver: StreamDriver,
    db_relative: &str,
    size: u64,
    modified_ms: i64,
    report: &mut MetaReport,
    projects: &mut Vec<ProjectRow>,
    sessions: &mut Vec<SessionRow>,
    files: &mut Vec<SessionFile>,
    instrument: &Instrument,
) -> Result<(), PipelineError> {
    use rosetta_core::rows::SessionRole;

    let Some(plan) = adapter.snapshot_plan(&stream.id) else {
        return Err(PipelineError::Config(format!(
            "{} drives stream {} as SqliteSnapshot but declares no snapshot plan",
            adapter.kind(),
            stream.id
        )));
    };
    let outcome = instrument.time(Metric::SkimNanos, || {
        rosetta_capture::snapshot_db(&root.join(db_relative))
    })?;
    let snapshot = match outcome {
        rosetta_capture::SnapshotOutcome::Ready(snapshot) => snapshot,
        // A racing writer or vanished db is the stream phase's problem.
        rosetta_capture::SnapshotOutcome::Missing | rosetta_capture::SnapshotOutcome::Unstable => {
            report.files_vanished += 1;
            return Ok(());
        }
    };
    report.files_skimmed += 1;
    instrument.add(Metric::FilesSkimmed, 1);

    for row in snapshot.query_rows(&plan.sessions_sql, None)? {
        let value: serde_json::Value = serde_json::from_str(&row).map_err(|error| {
            PipelineError::Config(format!("sessions_sql row is not an object: {error}"))
        })?;
        let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        let Some(native) = text("native_session_id") else {
            report.files_unidentified += 1;
            continue;
        };
        let skim = SkimResult {
            ops: Vec::new(),
            session_id: Some(native.clone()),
            parent_session_id: text("parent_native_session_id"),
            root_session_id: text("root_native_session_id"),
            role: match text("role").as_deref() {
                Some("subagent") => Some(SessionRole::Subagent),
                Some("team") => Some(SessionRole::Team),
                _ => None,
            },
            cwd: text("cwd"),
            agent_version: text("agent_version"),
            title: text("title"),
            started_at: value.get("started_at").and_then(|v| v.as_i64()),
        };
        let updated = value
            .get("updated_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(modified_ms);
        admit_identified(
            runtime,
            projects,
            sessions,
            files,
            AdmitSource {
                relative_path: format!("{db_relative}#{native}"),
                stream_id: stream.id.clone(),
                file_format: stream.file_format,
                discipline: stream.discipline,
                driver,
                size,
                modified_ms: updated,
            },
            skim,
            native,
        );
    }

    // The database is an authoritative snapshot. Cursors for members no
    // longer returned by sessions_sql remain as virtual tombstones for this
    // pass; their empty record query activates a sweep of old contribution.
    let prefix = format!("{db_relative}#");
    for (relative_path, cursor_stream, session_id) in
        rosetta_store::queries::cursor_sources_with_prefix(store.conn(), &runtime.id, &prefix)?
    {
        if files.iter().any(|file| file.relative_path == relative_path) {
            continue;
        }
        let Some(native_session_id) = relative_path.strip_prefix(&prefix).map(str::to_owned) else {
            continue;
        };
        files.push(SessionFile {
            relative_path,
            stream_id: cursor_stream,
            file_format: stream.file_format,
            discipline: stream.discipline,
            driver,
            session_id,
            native_session_id,
            size,
            modified_ms,
            agent_version: None,
        });
    }
    Ok(())
}
