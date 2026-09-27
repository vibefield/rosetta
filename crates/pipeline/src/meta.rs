//! Phase 2 — meta ingest. Bounded head/tail skims only: project and session
//! lists must render before any transcript body is fully read. Output is one
//! batch of Runtime + Project + SessionMeta upserts, plus the file→session
//! map the stream phase keys its cursors by.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rosetta_adapter_support::{
    AgentAdapter, FileDiscipline, FileFormat, FileStat, ProjectHint, SkimResult, StreamDriver,
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
    /// Capture and checkpoint this source without admitting a public session.
    pub ledger_only: bool,
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

impl SessionFile {
    pub(crate) fn projected_session_id(&self) -> Option<String> {
        (!self.ledger_only).then(|| self.session_id.clone())
    }
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
    project_hints: BTreeMap<String, BTreeSet<ProjectHint>>,
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
        rosetta_capture::walk_pruned(
            root,
            opts.max_files,
            |dir| layout.may_claim_below(dir, |_| true),
            |path| layout.matching_stream(path).is_some(),
        )
    })?;

    meta_ingest_inventory_into(
        adapter,
        runtime,
        root,
        store,
        &walk.files,
        walk.truncated,
        opts,
        instrument,
        ledger,
    )
}

/// Metadata admission over the exact roster already returned by discovery.
/// The roster is immutable evidence from the same finite command; individual
/// reads still handle files vanishing or changing after that census.
#[allow(clippy::too_many_arguments)]
pub fn meta_ingest_inventory_into(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &mut Store,
    inventory: &[rosetta_capture::ScannedFile],
    walk_truncated: bool,
    opts: &IngestOptions,
    instrument: &Instrument,
    ledger: Option<&mut rosetta_capture::Ledger>,
) -> Result<(MetaReport, Vec<SessionFile>), PipelineError> {
    let layout = adapter.layout();

    let mut report = MetaReport::default();
    let mut output = MetaOutput::default();
    report.walk_truncated = walk_truncated;

    for file in inventory {
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
        if !skim.ledger_only {
            output.ops.extend(std::mem::take(&mut skim.ops));
        }
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
            instrument,
        );
    }

    // A snapshot source that disappears whole keeps every member's last
    // projected generation, like a deleted JSONL transcript keeps its rows;
    // its virtual cursors simply see no further activity (product decision,
    // 2026-09-24: an agent pruning its own store must not erase history).

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

    // FK order: runtime → projects → sessions, one transaction.
    let MetaOutput {
        projects,
        sessions,
        mut files,
        ops: meta_ops,
        ..
    } = output;
    // Admission is part of a captured source's durable identity. New policy
    // can exclude a previously projected source from metadata admission, but
    // a re-skim must not turn its existing cursor into a different source.
    // Preserve that legacy binding while continuing to capture its raw tail;
    // newly discovered internal sources still have no projected session.
    for file in files.iter_mut().filter(|file| file.ledger_only) {
        if rosetta_store::queries::cursor_state(store.conn(), &runtime.id, &file.relative_path)?
            .is_some_and(|cursor| cursor.session_id.as_deref() == Some(file.session_id.as_str()))
        {
            file.ledger_only = false;
        }
    }
    let mut ops: Vec<IngestOp> =
        Vec::with_capacity(1 + projects.len() + sessions.len() + meta_ops.len());
    ops.push(IngestOp::Runtime(runtime.clone()));
    ops.extend(projects.into_iter().map(IngestOp::Project));
    ops.extend(sessions.into_iter().map(IngestOp::SessionMeta));
    ops.extend(meta_ops);
    if let Some(ledger) = ledger {
        let mut semantic_ops = ops.clone();
        for op in &mut semantic_ops {
            if let IngestOp::Runtime(runtime) = op {
                // Discovery wall time is intentionally insert-only in the
                // projection and must not turn an otherwise identical bounded
                // skim into a new durable metadata generation every boot.
                runtime.discovered_at = 0;
            }
        }
        let semantic = serde_json::to_vec(&semantic_ops).map_err(|error| {
            PipelineError::Config(format!("serializing semantic meta key: {error}"))
        })?;
        let semantic_key = blake3::hash(&semantic).to_hex().to_string();
        instrument.ledger_write(ledger, |ledger| -> Result<(), PipelineError> {
            let json = serde_json::to_string(&ops).map_err(|error| {
                PipelineError::Config(format!("serializing meta ops for the ledger: {error}"))
            })?;
            ledger.append_meta_checkpoint_if_changed(
                crate::LEDGER_META_SCHEMA_VERSION,
                &json,
                &semantic_key,
            )?;
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
            let incoming_has_delegation = row.delegation.is_some();
            let existing_has_delegation = existing.delegation.is_some();
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
            existing.parent_session_id = if incoming_has_delegation {
                row.parent_session_id
                    .or_else(|| existing.parent_session_id.clone())
            } else if existing_has_delegation {
                existing.parent_session_id.clone()
            } else {
                existing.parent_session_id.clone().or(row.parent_session_id)
            };
            existing.delegation = merge_delegation(existing.delegation.take(), row.delegation);
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

fn merge_delegation(
    existing: Option<rosetta_core::SessionDelegation>,
    incoming: Option<rosetta_core::SessionDelegation>,
) -> Option<rosetta_core::SessionDelegation> {
    match (existing, incoming) {
        (Some(existing), Some(incoming)) => Some(rosetta_core::SessionDelegation {
            parent_tool_native_id: incoming
                .parent_tool_native_id
                .or(existing.parent_tool_native_id),
            workflow_run_id: incoming.workflow_run_id.or(existing.workflow_run_id),
            team: incoming.team.or(existing.team),
            color: incoming.color.or(existing.color),
            configured_model: incoming.configured_model.or(existing.configured_model),
        }),
        (existing, incoming) => incoming.or(existing),
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
/// metadata. Explicit adapter ownership can also join a deleted worktree to a
/// native owner session or checkout root. No source-body record is consulted.
fn settle_projects(output: &mut MetaOutput) {
    let positions: BTreeMap<String, usize> = output
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

    // Conflicting native ownership facts fail closed to the ordinary cwd
    // resolver. A unique `SameAsSession` may form a chain, so resolve roots
    // first and propagate only already-settled rows. Missing owners and cycles
    // likewise fall back to cwd instead of manufacturing a relationship.
    let hints: Vec<Option<ProjectHint>> = output
        .sessions
        .iter()
        .map(|session| {
            output
                .project_hints
                .get(&session.id)
                .and_then(|hints| (hints.len() == 1).then(|| hints.first().cloned()).flatten())
        })
        .collect();
    let mut resolved: Vec<Option<ProjectRow>> = output
        .sessions
        .iter()
        .zip(&hints)
        .map(|(session, hint)| match hint {
            Some(ProjectHint::Root { root, git_repo }) => Some(crate::project::project_from_hint(
                session.cwd.as_deref().unwrap_or(root),
                root,
                git_repo.as_deref(),
            )),
            Some(ProjectHint::SameAsSession { .. }) => None,
            None => session.cwd.as_deref().map(crate::project::project),
        })
        .collect();

    for _ in 0..output.sessions.len() {
        let mut changed = false;
        for index in 0..output.sessions.len() {
            if resolved[index].is_some() {
                continue;
            }
            let Some(ProjectHint::SameAsSession { native_session_id }) = &hints[index] else {
                continue;
            };
            let Some(owner) = positions.get(native_session_id) else {
                continue;
            };
            let Some(mut project) = resolved[*owner].clone() else {
                continue;
            };
            extend_project_roots(&mut project, output.sessions[index].cwd.as_deref());
            resolved[index] = Some(project);
            changed = true;
        }
        if !changed {
            break;
        }
    }

    let mut projects = BTreeMap::new();
    for (index, session) in output.sessions.iter_mut().enumerate() {
        let project = resolved[index]
            .take()
            .or_else(|| session.cwd.as_deref().map(crate::project::project));
        session.project_id = project.as_ref().map(|project| project.id.clone());
        if let Some(project) = project {
            merge_project(&mut projects, project);
        }
    }
    output.projects = projects.into_values().collect();
}

fn extend_project_roots(project: &mut ProjectRow, cwd: Option<&str>) {
    let Some(cwd) = cwd else {
        return;
    };
    let observed = crate::project::project(cwd);
    project.root_paths.extend(observed.root_paths);
    project.root_paths.sort();
    project.root_paths.dedup();
    if project.git_repo.is_none() {
        project.git_repo = observed.git_repo;
    }
}

fn merge_project(projects: &mut BTreeMap<String, ProjectRow>, mut row: ProjectRow) {
    let Some(existing) = projects.get_mut(&row.id) else {
        projects.insert(row.id.clone(), row);
        return;
    };
    existing.root_paths.append(&mut row.root_paths);
    existing.root_paths.sort();
    existing.root_paths.dedup();
    existing.name = existing.name.clone().min(row.name);
    existing.git_repo = match (existing.git_repo.take(), row.git_repo) {
        (None, value) | (value, None) => value,
        (Some(left), Some(right)) => Some(left.min(right)),
    };
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
    instrument: &Instrument,
) {
    let sessions_before = output.sessions.len();
    let session_id = ids::session_id(&runtime.id, &native_session_id);

    if skim.ledger_only {
        output.files.push(SessionFile {
            ledger_only: true,
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
        return;
    }

    if let Some(project) = skim
        .project
        .take()
        .and_then(|hint| canonical_project_hint(&runtime.id, hint))
    {
        output
            .project_hints
            .entry(session_id.clone())
            .or_default()
            .insert(project);
    }

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

    // A depth-2 sidecar can name an immediate parent whose own transcript has
    // expired. Materialize the proven root first so both foreign keys remain
    // honest even when only the descendant sidecar survives.
    if let Some(root_native) = skim.root_session_id.as_deref()
        && root_session_id != session_id
        && parent_session_id.as_deref() != Some(root_session_id.as_str())
    {
        merge_session(
            &mut output.sessions,
            SessionRow {
                id: root_session_id.clone(),
                native_session_id: root_native.to_owned(),
                runtime_id: runtime.id.clone(),
                project_id: None,
                cwd: None,
                parent_session_id: None,
                delegation: None,
                root_session_id: root_session_id.clone(),
                role: SessionRole::Main,
                title: None,
                summary: None,
                started_at: skim.started_at.unwrap_or(source.modified_ms),
                updated_at: source.modified_ms,
                activity: None,
                stream_state: StreamState::Pending,
            },
        );
    }

    // A retained subagent transcript can outlive its parent's transcript.
    // Materialize an honest parent placeholder so the relationship and FK
    // survive GC; a later main skim refines the same deterministic row.
    if let (Some(parent_native), Some(parent_id)) = (
        skim.parent_session_id.as_deref(),
        parent_session_id.as_ref(),
    ) {
        let nested_parent = parent_id != &root_session_id;
        merge_session(
            &mut output.sessions,
            SessionRow {
                id: parent_id.clone(),
                native_session_id: parent_native.to_owned(),
                runtime_id: runtime.id.clone(),
                project_id: None,
                cwd: None,
                parent_session_id: nested_parent.then(|| root_session_id.clone()),
                delegation: None,
                root_session_id: root_session_id.clone(),
                role: if nested_parent {
                    SessionRole::Subagent
                } else {
                    SessionRole::Main
                },
                title: None,
                summary: None,
                started_at: skim.started_at.unwrap_or(source.modified_ms),
                updated_at: source.modified_ms,
                activity: None,
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
        delegation: skim.delegation,
        root_session_id,
        role,
        // Ranked title facts are applied after all session rows exist.
        title: None,
        summary: skim.summary,
        started_at: skim.started_at.unwrap_or(source.modified_ms),
        updated_at: skim.updated_at.unwrap_or(source.modified_ms),
        activity: None,
        stream_state: StreamState::Pending,
    };
    merge_session(&mut output.sessions, row);

    output.files.push(SessionFile {
        ledger_only: false,
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
    instrument.add(
        Metric::SessionsDiscovered,
        (output.sessions.len() - sessions_before) as u64,
    );
}

fn canonical_project_hint(runtime_id: &str, hint: ProjectHint) -> Option<ProjectHint> {
    match hint {
        ProjectHint::Root { root, git_repo } => {
            let root = ids::normalize_root(&root);
            (!root.is_empty()).then_some(ProjectHint::Root { root, git_repo })
        }
        ProjectHint::SameAsSession { native_session_id } => {
            (!native_session_id.is_empty()).then(|| ProjectHint::SameAsSession {
                native_session_id: ids::session_id(runtime_id, &native_session_id),
            })
        }
    }
}

/// Meta admission for a rewrite-in-place document. One physical file may
/// enumerate several native sessions, each represented as an independently
/// replayable virtual source (`<document-path>#<native-id>`).
#[allow(clippy::too_many_arguments)]
fn admit_document(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
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
        if !skim.ledger_only {
            output.ops.extend(std::mem::take(&mut skim.ops));
        }
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
            instrument,
        );
    }

    // A member the document no longer lists keeps its last projected
    // generation, like a removed SQLite member and a deleted JSONL transcript
    // (product decision, 2026-09-24).
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
            instrument,
        );
    }

    // A member no longer returned by sessions_sql keeps its last projected
    // generation, exactly as a deleted JSONL transcript keeps its rows: an
    // agent pruning its own store (Hermes 0.21.4 retains 90 days by default)
    // must not erase Rosetta's history (product decision, 2026-09-24). The
    // cursor simply sees no further activity.
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
            delegation: None,
            root_session_id: root.to_owned(),
            role,
            title: None,
            summary: None,
            started_at,
            updated_at: started_at,
            activity: None,
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
    fn exact_sidecar_parent_outranks_directory_fallback_in_any_order() {
        let fallback = session(
            "child",
            None,
            Some("root"),
            "root",
            SessionRole::Subagent,
            10,
        );
        let mut exact = fallback.clone();
        exact.parent_session_id = Some("worker-parent".into());
        exact.delegation = Some(rosetta_core::SessionDelegation {
            parent_tool_native_id: Some("toolu-child".into()),
            workflow_run_id: None,
            team: None,
            color: None,
            configured_model: Some("opus".into()),
        });
        let mut forward = Vec::new();
        merge_session(&mut forward, fallback.clone());
        merge_session(&mut forward, exact.clone());
        let mut reverse = Vec::new();
        merge_session(&mut reverse, exact);
        merge_session(&mut reverse, fallback);
        assert_eq!(forward, reverse);
        assert_eq!(
            forward[0].parent_session_id.as_deref(),
            Some("worker-parent")
        );
        assert_eq!(
            forward[0]
                .delegation
                .as_ref()
                .and_then(|delegation| delegation.parent_tool_native_id.as_deref()),
            Some("toolu-child")
        );
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

    #[test]
    fn native_owner_links_a_deleted_worktree_to_the_parent_project() {
        let parent_cwd = "/work/original";
        let child_cwd = "/gone/worktrees/child";
        let mut output = MetaOutput {
            sessions: vec![
                session(
                    "parent",
                    Some(parent_cwd),
                    None,
                    "parent",
                    SessionRole::Main,
                    10,
                ),
                session(
                    "child",
                    Some(child_cwd),
                    Some("parent"),
                    "parent",
                    SessionRole::Subagent,
                    20,
                ),
            ],
            project_hints: BTreeMap::from([(
                "child".to_owned(),
                BTreeSet::from([ProjectHint::SameAsSession {
                    native_session_id: "parent".to_owned(),
                }]),
            )]),
            ..MetaOutput::default()
        };

        settle_projects(&mut output);

        assert_eq!(output.sessions[1].project_id, output.sessions[0].project_id);
        assert_eq!(output.projects.len(), 1);
        assert_eq!(output.projects[0].name, "original");
        assert_eq!(
            output.projects[0].root_paths,
            vec![child_cwd.to_owned(), parent_cwd.to_owned()]
        );
    }

    #[test]
    fn native_checkout_root_links_a_deleted_worktree_without_live_git_metadata() {
        let canonical_root = "/work/original";
        let child_cwd = "/gone/worktrees/child";
        let mut output = MetaOutput {
            sessions: vec![session(
                "child",
                Some(child_cwd),
                None,
                "child",
                SessionRole::Main,
                20,
            )],
            project_hints: BTreeMap::from([(
                "child".to_owned(),
                BTreeSet::from([ProjectHint::Root {
                    root: canonical_root.to_owned(),
                    git_repo: Some("git@example.com:acme/repo.git".to_owned()),
                }]),
            )]),
            ..MetaOutput::default()
        };

        settle_projects(&mut output);

        assert_eq!(output.sessions[0].cwd.as_deref(), Some(child_cwd));
        assert_eq!(output.projects.len(), 1);
        assert_eq!(
            output.sessions[0].project_id.as_deref(),
            Some(ids::project_id(canonical_root, None).as_str())
        );
        assert_eq!(output.projects[0].name, "original");
        assert_eq!(
            output.projects[0].root_paths,
            vec![child_cwd.to_owned(), canonical_root.to_owned()]
        );
        assert_eq!(
            output.projects[0].git_repo.as_deref(),
            Some("example.com:acme/repo")
        );
    }
}
