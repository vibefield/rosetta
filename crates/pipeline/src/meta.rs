//! Phase 2 — meta ingest. Bounded head/tail skims only: project and session
//! lists must render before any transcript body is fully read. Output is one
//! batch of Runtime + Project + SessionMeta upserts, plus the file→session
//! map the stream phase keys its cursors by.

use std::path::Path;

use rosetta_adapter_support::{
    AgentAdapter, FileDiscipline, FileFormat, FileStat, SkimResult, StreamDriver,
};
use rosetta_core::rows::{
    ProjectRow, RuntimeRow, SessionRole, SessionRow, SessionTitleRow, StreamState, TitleKind,
};
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
    /// Meta confirmed that the physical snapshot source no longer exists.
    /// Only these virtual tombstones may turn a Missing read into an empty
    /// authoritative generation; a racing ordinary read remains incomplete.
    pub source_missing: bool,
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

#[derive(Debug, Default)]
struct MetaOutput {
    projects: Vec<ProjectRow>,
    sessions: Vec<SessionRow>,
    files: Vec<SessionFile>,
    ops: Vec<IngestOp>,
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
    let mut output = MetaOutput::default();
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
            StreamDriver::DocumentSnapshot
                if matches!(
                    stream.file_format,
                    FileFormat::Json | FileFormat::Toml | FileFormat::Markdown | FileFormat::Text
                ) && stream.discipline == FileDiscipline::RewriteInPlace =>
            {
                admit_document(
                    adapter,
                    runtime,
                    root,
                    store,
                    stream,
                    driver,
                    &file.relative_path,
                    file.size,
                    file.modified_ms,
                    opts,
                    &mut report,
                    &mut output,
                    instrument,
                )?;
                continue;
            }
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
                match instrument.time(Metric::SkimNanos, || {
                    rosetta_capture::sqlite_file_status(&root.join(&file.relative_path))
                })? {
                    rosetta_capture::SqliteFileStatus::Missing => {
                        report.files_vanished += 1;
                        continue;
                    }
                    rosetta_capture::SqliteFileStatus::NotDatabase => {
                        // Broad database globs also see agent-owned lock and
                        // marker files. Preserve them in the inventory count,
                        // but never manufacture an empty SQLite source.
                        report.files_inventory_only += 1;
                        continue;
                    }
                    rosetta_capture::SqliteFileStatus::Database => {}
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
                    &mut output,
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
        if opts.skim_fallback_bytes > 0
            && opts.skim_fallback_records > 0
            && adapter.wants_skim_fallback(&stream.id, &skim)
        {
            // Rewind to the beginning of the record clipped by the head so
            // every byte handed to the adapter is a complete JSONL record.
            let start = windows
                .head
                .iter()
                .rposition(|byte| *byte == opts.tail.delimiter)
                .map_or(0, |at| at + 1) as u64;
            if let Some(scan) = instrument.time(Metric::SkimFallbackNanos, || {
                rosetta_capture::scan_records(
                    &root.join(&file.relative_path),
                    start,
                    opts.skim_fallback_bytes,
                    opts.skim_fallback_records,
                    opts.tail.delimiter,
                    opts.tail.max_record_bytes,
                    opts.tail.normalize_crlf,
                )
            })? {
                instrument.add(Metric::SkimFallbackBytes, scan.bytes_read);
                instrument.add(Metric::BytesRead, scan.bytes_read);
                for record in scan.records {
                    if adapter.absorb_skim_fallback_record(&stream.id, &record, &mut skim) {
                        break;
                    }
                }
            }
        }
        output.ops.extend(std::mem::take(&mut skim.ops));
        let Some(native_session_id) = skim.session_id.clone() else {
            report.files_unidentified += 1;
            continue;
        };
        admit_identified(
            runtime,
            &mut output,
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

    // Snapshot membership is authoritative even when the whole physical
    // source disappears. The walk cannot call `admit_document/database` for a
    // missing path, so retain its existing virtual cursors as explicit empty
    // tombstones for one generation. JSONL files keep their ordinary missing
    // semantics and are never swept here.
    let present: std::collections::BTreeSet<&str> = walk
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    for (virtual_path, cursor_stream, session_id) in
        rosetta_store::queries::cursor_sources_with_prefix(store.conn(), &runtime.id, "")?
    {
        if output
            .files
            .iter()
            .any(|file| file.relative_path == virtual_path)
        {
            continue;
        }
        let Some((physical_path, native_session_id)) = virtual_path.split_once('#') else {
            continue;
        };
        let native_session_id = native_session_id.to_owned();
        if present.contains(physical_path) {
            continue;
        }
        let Some(stream) = layout.stream(&cursor_stream) else {
            continue;
        };
        let Some(driver) = adapter.stream_driver(&cursor_stream) else {
            continue;
        };
        if !matches!(
            driver,
            StreamDriver::DocumentSnapshot | StreamDriver::SqliteSnapshot
        ) {
            continue;
        }
        output.files.push(SessionFile {
            relative_path: virtual_path,
            stream_id: cursor_stream,
            file_format: stream.file_format,
            discipline: stream.discipline,
            driver,
            session_id,
            native_session_id,
            size: 0,
            modified_ms: 0,
            agent_version: None,
            source_missing: true,
        });
    }

    // Session ownership is a metadata decision. Resolve child inheritance and
    // local repository identity exactly once before any transcript is streamed;
    // the resulting SessionRow + ProjectRow facts are then journaled together.
    settle_projects(&mut output);

    // A re-skim must never regress ingest progress: a session the stream
    // phase already indexed stays indexed.
    for session in &mut output.sessions {
        if let Some(existing) = rosetta_store::queries::session(store.conn(), &session.id)? {
            session.stream_state = existing.stream_state;
        }
    }
    output
        .sessions
        .sort_by_key(|session| !matches!(session.role, SessionRole::Main));

    report.sessions = output.sessions.len();
    report.projects = output.projects.len();
    instrument.add(Metric::FilesMatched, report.files_matched as u64);
    instrument.add(Metric::SessionsDiscovered, report.sessions as u64);

    // FK order: runtime → projects → sessions, one transaction.
    let MetaOutput {
        projects,
        sessions,
        mut files,
        ops: meta_ops,
    } = output;
    let mut ops: Vec<IngestOp> =
        Vec::with_capacity(1 + projects.len() + sessions.len() + meta_ops.len());
    ops.push(IngestOp::Runtime(runtime.clone()));
    ops.extend(projects.into_iter().map(IngestOp::Project));
    ops.extend(sessions.into_iter().map(IngestOp::SessionMeta));
    ops.extend(meta_ops);
    if let Some(ledger) = ledger {
        instrument.ledger_write(ledger, |ledger| -> Result<(), PipelineError> {
            let json = serde_json::to_string(&ops).map_err(|error| {
                PipelineError::Config(format!("serializing meta ops for the ledger: {error}"))
            })?;
            ledger.append_meta(crate::LEDGER_META_SCHEMA_VERSION, &json)?;
            ledger.checkpoint()?;
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
            let existing_started_at = existing.started_at;
            let newer = row.updated_at >= existing.updated_at;
            existing.started_at = existing.started_at.min(row.started_at);
            existing.updated_at = existing.updated_at.max(row.updated_at);
            if newer {
                existing.title = row.title.or_else(|| existing.title.clone());
                existing.summary = row.summary.or_else(|| existing.summary.clone());
            } else {
                existing.title = existing.title.clone().or(row.title);
                existing.summary = existing.summary.clone().or(row.summary);
            }
            existing.cwd = select_primary_cwd(
                existing.cwd.take(),
                existing_started_at,
                row.cwd,
                row.started_at,
            );
            // Project assignment is derived after every metadata source has
            // contributed its bounded fact.
            existing.project_id = None;
            existing.parent_session_id =
                existing.parent_session_id.clone().or(row.parent_session_id);
            if existing.root_session_id == existing.id && row.root_session_id != row.id {
                existing.root_session_id = row.root_session_id;
            }
            if existing.role == SessionRole::Main && row.role != SessionRole::Main {
                existing.role = row.role;
            }
            existing.stream_state = stronger_stream_state(existing.stream_state, row.stream_state);
        }
        None => sessions.push(row),
    }
}

fn select_primary_cwd(
    left: Option<String>,
    left_started_at: i64,
    right: Option<String>,
    right_started_at: i64,
) -> Option<String> {
    match (left, right) {
        (None, value) | (value, None) => value,
        (Some(left), Some(right)) if left == right => Some(left),
        (Some(left), Some(right)) => {
            if (right_started_at, right.as_str()) < (left_started_at, left.as_str()) {
                Some(right)
            } else {
                Some(left)
            }
        }
    }
}

/// Close the adapter metadata contract into one cwd/project assignment per
/// session. A child with no native cwd may inherit its parent's already-set
/// metadata; no parent is inferred from a child, and no source-body record is
/// consulted here.
fn settle_projects(output: &mut MetaOutput) {
    let positions: std::collections::BTreeMap<String, usize> = output
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (session.id.clone(), index))
        .collect();

    for _ in 0..output.sessions.len() {
        let inherited: Vec<Option<String>> = output
            .sessions
            .iter()
            .map(|session| {
                if session.cwd.is_some() {
                    return None;
                }
                session
                    .parent_session_id
                    .as_ref()
                    .and_then(|id| positions.get(id))
                    .and_then(|index| output.sessions[*index].cwd.clone())
                    .or_else(|| {
                        (session.root_session_id != session.id)
                            .then_some(&session.root_session_id)
                            .and_then(|id| positions.get(id))
                            .and_then(|index| output.sessions[*index].cwd.clone())
                    })
            })
            .collect();
        let mut changed = false;
        for (session, cwd) in output.sessions.iter_mut().zip(inherited) {
            if session.cwd.is_none() && cwd.is_some() {
                session.cwd = cwd;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut projects = std::collections::BTreeMap::new();
    for session in &mut output.sessions {
        session.project_id = session.cwd.as_deref().map(|cwd| {
            let project = crate::project::project(cwd);
            let id = project.id.clone();
            projects.entry(id.clone()).or_insert(project);
            id
        });
    }
    output.projects = projects.into_values().collect();
}

fn stronger_stream_state(left: StreamState, right: StreamState) -> StreamState {
    let rank = |state| match state {
        StreamState::Pending => 0,
        StreamState::Streaming => 1,
        StreamState::Indexed => 2,
    };
    if rank(right) > rank(left) {
        right
    } else {
        left
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
    output: &mut MetaOutput,
    source: AdmitSource,
    mut skim: SkimResult,
    native_session_id: String,
) {
    let session_id = ids::session_id(&runtime.id, &native_session_id);

    if let Some(title) = skim.title.take() {
        output.ops.push(IngestOp::SessionTitle(SessionTitleRow {
            session_id: session_id.clone(),
            title,
            kind: skim.title_kind.take().unwrap_or(TitleKind::Explicit),
            at: skim
                .updated_at
                .or(skim.started_at)
                .unwrap_or(source.modified_ms),
        }));
    }

    let cwd = skim.cwd.take().map(|cwd| ids::normalize_root(&cwd));

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
            &mut output.sessions,
            SessionRow {
                id: parent_id.clone(),
                native_session_id: parent_native.to_owned(),
                runtime_id: runtime.id.clone(),
                project_id: None,
                cwd: None,
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
        project_id: None,
        cwd,
        parent_session_id,
        root_session_id,
        role,
        // Ranked title facts are applied after all session rows exist.
        title: None,
        summary: skim.summary,
        started_at: skim.started_at.unwrap_or(source.modified_ms),
        updated_at: skim.updated_at.unwrap_or(source.modified_ms),
        stream_state: StreamState::Pending,
    };
    merge_session(&mut output.sessions, row);

    output.files.push(SessionFile {
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
        source_missing: false,
    });
}

/// Meta admission for a rewrite-in-place document. One physical file may
/// enumerate several native sessions, each represented as an independently
/// replayable virtual source (`<document-path>#<native-id>`).
#[allow(clippy::too_many_arguments)]
fn admit_document(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &Store,
    stream: &rosetta_adapter_support::StreamSpec,
    driver: StreamDriver,
    relative_path: &str,
    size: u64,
    modified_ms: i64,
    opts: &IngestOptions,
    report: &mut MetaReport,
    output: &mut MetaOutput,
    instrument: &Instrument,
) -> Result<(), PipelineError> {
    let path = root.join(relative_path);
    let document_limit = adapter.document_size_limit(&stream.id, opts.tail.max_record_bytes);
    let mut retries = 0_u32;
    let snapshot = loop {
        match instrument.time(Metric::SkimNanos, || {
            rosetta_capture::snapshot_document(&path, document_limit)
        })? {
            rosetta_capture::DocumentOutcome::Ready(snapshot) => break snapshot,
            rosetta_capture::DocumentOutcome::Missing => {
                report.files_vanished += 1;
                return Ok(());
            }
            rosetta_capture::DocumentOutcome::Unstable => {
                retries += 1;
                if retries > opts.max_retries {
                    report.files_vanished += 1;
                    return Ok(());
                }
            }
            rosetta_capture::DocumentOutcome::Oversized { size, limit } => {
                return Err(PipelineError::Config(format!(
                    "document snapshot {relative_path} is {size} bytes, above the {limit}-byte limit"
                )));
            }
        }
    };

    report.files_skimmed += 1;
    instrument.add(Metric::FilesSkimmed, 1);
    instrument.add(Metric::BytesRead, snapshot.bytes().len() as u64);
    let stat = FileStat {
        relative_path: relative_path.to_owned(),
        size,
        modified_ms,
    };
    for mut skim in adapter.document_sessions(&stream.id, &stat, snapshot.bytes()) {
        output.ops.extend(std::mem::take(&mut skim.ops));
        let Some(native) = skim.session_id.clone() else {
            report.files_unidentified += 1;
            continue;
        };
        admit_identified(
            runtime,
            output,
            AdmitSource {
                relative_path: format!("{relative_path}#{native}"),
                stream_id: stream.id.clone(),
                file_format: stream.file_format,
                discipline: stream.discipline,
                driver,
                size,
                modified_ms,
            },
            skim,
            native,
        );
    }

    // Removed members are retained for one empty generation so their old
    // projection contribution is swept just like removed SQLite members.
    let prefix = format!("{relative_path}#");
    for (virtual_path, cursor_stream, session_id) in
        rosetta_store::queries::cursor_sources_with_prefix(store.conn(), &runtime.id, &prefix)?
    {
        if output
            .files
            .iter()
            .any(|file| file.relative_path == virtual_path)
        {
            continue;
        }
        let Some(native_session_id) = virtual_path.strip_prefix(&prefix).map(str::to_owned) else {
            continue;
        };
        output.files.push(SessionFile {
            relative_path: virtual_path,
            stream_id: cursor_stream,
            file_format: stream.file_format,
            discipline: stream.discipline,
            driver,
            session_id,
            native_session_id,
            size,
            modified_ms,
            agent_version: None,
            source_missing: false,
        });
    }
    Ok(())
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
    output: &mut MetaOutput,
    instrument: &Instrument,
) -> Result<(), PipelineError> {
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
        let skim = adapter.snapshot_session(&stream.id, &value);
        let Some(native) = skim.session_id.clone() else {
            report.files_unidentified += 1;
            continue;
        };
        let updated = skim.updated_at.unwrap_or(modified_ms);
        admit_identified(
            runtime,
            output,
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
        if output
            .files
            .iter()
            .any(|file| file.relative_path == relative_path)
        {
            continue;
        }
        let Some(native_session_id) = relative_path.strip_prefix(&prefix).map(str::to_owned) else {
            continue;
        };
        output.files.push(SessionFile {
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
            source_missing: false,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        id: &str,
        cwd: Option<&str>,
        parent: Option<&str>,
        root: &str,
        role: SessionRole,
        started_at: i64,
    ) -> SessionRow {
        SessionRow {
            id: id.to_owned(),
            native_session_id: id.to_owned(),
            runtime_id: "runtime".to_owned(),
            project_id: None,
            cwd: cwd.map(str::to_owned),
            parent_session_id: parent.map(str::to_owned),
            root_session_id: root.to_owned(),
            role,
            title: None,
            summary: None,
            started_at,
            updated_at: started_at,
            stream_state: StreamState::Pending,
        }
    }

    #[test]
    fn metadata_cwd_selection_is_order_independent() {
        let early = session("s", Some("/work/early"), None, "s", SessionRole::Main, 10);
        let late = session("s", Some("/work/late"), None, "s", SessionRole::Main, 20);
        let mut left = Vec::new();
        merge_session(&mut left, early.clone());
        merge_session(&mut left, late.clone());
        let mut right = Vec::new();
        merge_session(&mut right, late);
        merge_session(&mut right, early);
        assert_eq!(left[0].cwd.as_deref(), Some("/work/early"));
        assert_eq!(left[0].cwd, right[0].cwd);
    }

    #[test]
    fn child_without_cwd_inherits_only_from_metadata_parent() {
        let mut output = MetaOutput {
            sessions: vec![
                session(
                    "parent",
                    Some("/work/project"),
                    None,
                    "parent",
                    SessionRole::Main,
                    10,
                ),
                session(
                    "child",
                    None,
                    Some("parent"),
                    "parent",
                    SessionRole::Subagent,
                    20,
                ),
            ],
            ..MetaOutput::default()
        };
        settle_projects(&mut output);
        assert_eq!(output.sessions[1].cwd.as_deref(), Some("/work/project"));
        assert_eq!(output.sessions[1].project_id, output.sessions[0].project_id);
        assert_eq!(output.projects.len(), 1);
    }

    #[test]
    fn missing_metadata_stays_unassigned() {
        let mut output = MetaOutput {
            sessions: vec![session("s", None, None, "s", SessionRole::Main, 10)],
            ..MetaOutput::default()
        };
        settle_projects(&mut output);
        assert_eq!(output.sessions[0].cwd, None);
        assert_eq!(output.sessions[0].project_id, None);
        assert!(output.projects.is_empty());
    }
}
