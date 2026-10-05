//! Phase 2 — meta ingest. Bounded head/tail skims only: project and session
//! lists must render before any transcript body is fully read. Output is one
//! batch of Runtime + Project + SessionMeta upserts, plus the file→session
//! map the stream phase keys its cursors by.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use rosetta_adapter_support::{
    AgentAdapter, FileDiscipline, FileFormat, ProjectHint, SkimResult, StreamDriver, StreamSpec,
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
    /// from streaming until movement re-admits them; a finite ingest treats
    /// any such file as an incomplete ingest.
    pub files_unidentified: usize,
    /// Unidentified transcripts whose first record no delimiter ends yet:
    /// their writer is still writing it, so they are not yet sessions.
    /// Withheld like [`MetaReport::files_unidentified`], never counted there.
    pub files_unfinished: usize,
    pub files_vanished: usize,
    pub walk_truncated: bool,
    pub sessions: usize,
    pub projects: usize,
    pub stats: ApplyStats,
}

#[derive(Debug, Default)]
struct MetaOutput {
    projects: Vec<ProjectRow>,
    /// First-merge order. Grows only through [`MetaOutput::merge_session`],
    /// which keeps `session_positions` in step.
    sessions: Vec<SessionRow>,
    session_positions: HashMap<String, usize>,
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
        rosetta_capture::walk_classified(
            root,
            opts.max_files,
            |dir| layout.may_claim_below(dir, |_| true),
            |path| layout.matching_stream_index(path),
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
/// reads still handle files vanishing or changing after that census. Each
/// file's stream comes from the class its discovery walk carried.
///
/// Sources are read on a bounded pool (see `skim`); everything that merges
/// their facts — sessions, projects, counters, databases — runs in inventory
/// order, so the batch and its ledger frame match a serial skim exactly.
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
    let (mut report, files, ops) = plan_inventory(
        adapter,
        runtime,
        root,
        store,
        inventory,
        walk_truncated,
        opts,
        instrument,
        crate::skim::skim_threads(),
    )?;
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
    Ok((report, files))
}

/// Where one inventory file goes, decided from its stream alone.
enum Entry<'a> {
    /// No stream claims the path (an inventory walked without classes).
    Unclaimed,
    InventoryOnly,
    /// A driver the stream's declared layout cannot carry.
    Misconfigured(&'a StreamSpec, StreamDriver),
    /// A SQLite source, snapshotted and admitted by the ordered merge.
    Database(&'a StreamSpec, StreamDriver),
    /// A transcript or document whose bytes are read on the skim pool.
    Read(&'a StreamSpec, StreamDriver),
}

fn classify<'a>(
    adapter: &dyn AgentAdapter,
    layout: &'a rosetta_adapter_support::GroundTruthDoc,
    file: &rosetta_capture::ScannedFile,
) -> Entry<'a> {
    let Some(stream) = crate::discover::inventory_stream(layout, file) else {
        return Entry::Unclaimed;
    };
    let Some(driver) = adapter.stream_driver(&stream.id) else {
        return Entry::InventoryOnly;
    };
    match driver {
        StreamDriver::JsonLines
            if stream.file_format == FileFormat::Jsonl
                && stream.discipline == FileDiscipline::AppendOnly =>
        {
            Entry::Read(stream, driver)
        }
        StreamDriver::DocumentSnapshot
            if matches!(
                stream.file_format,
                FileFormat::Json | FileFormat::Toml | FileFormat::Markdown | FileFormat::Text
            ) && stream.discipline == FileDiscipline::RewriteInPlace =>
        {
            Entry::Read(stream, driver)
        }
        StreamDriver::SqliteSnapshot
            if stream.file_format == FileFormat::Sqlite
                && stream.discipline == FileDiscipline::Database =>
        {
            Entry::Database(stream, driver)
        }
        _ => Entry::Misconfigured(stream, driver),
    }
}

/// The bounded skim of one inventory, up to the batch it journals and
/// applies: the report, the sources the stream phase will tail (newest
/// first), and the ops in batch order. `store` is only read, for progress a
/// re-skim must not regress. `threads` bounds the concurrent source reads.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_inventory(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &Store,
    inventory: &[rosetta_capture::ScannedFile],
    walk_truncated: bool,
    opts: &IngestOptions,
    instrument: &Instrument,
    threads: usize,
) -> Result<(MetaReport, Vec<SessionFile>, Vec<IngestOp>), PipelineError> {
    let layout = adapter.layout();

    let mut report = MetaReport::default();
    let mut output = MetaOutput::default();
    report.walk_truncated = walk_truncated;

    let entries: Vec<Entry<'_>> = inventory
        .iter()
        .map(|file| classify(adapter, layout, file))
        .collect();
    report.files_matched = entries
        .iter()
        .filter(|entry| !matches!(entry, Entry::Unclaimed))
        .count();
    let reads: Vec<(&rosetta_capture::ScannedFile, &StreamSpec, StreamDriver)> = inventory
        .iter()
        .zip(&entries)
        .filter_map(|(file, entry)| match *entry {
            Entry::Read(stream, driver) => Some((file, stream, driver)),
            _ => None,
        })
        .collect();
    crate::skim::in_order(
        &reads,
        threads,
        |&(file, stream, driver)| match driver {
            StreamDriver::DocumentSnapshot => {
                crate::skim::read_document(adapter, root, file, stream, opts, instrument)
            }
            _ => crate::skim::skim_transcript(adapter, root, file, stream, opts, instrument),
        },
        |next| -> Result<(), PipelineError> {
            for (file, entry) in inventory.iter().zip(&entries) {
                let (stream, driver) = match *entry {
                    Entry::Unclaimed => continue,
                    Entry::InventoryOnly => {
                        report.files_inventory_only += 1;
                        continue;
                    }
                    Entry::Misconfigured(stream, driver) => {
                        return Err(PipelineError::Config(format!(
                            "{} opted stream {} into {:?}, but its layout is {:?}/{:?}",
                            adapter.kind(),
                            stream.id,
                            driver,
                            stream.file_format,
                            stream.discipline
                        )));
                    }
                    Entry::Database(stream, driver) => {
                        // Sidecars (-wal/-shm/-journal) travel with their
                        // database; only the database itself is a source.
                        if rosetta_capture::is_db_sidecar(&file.relative_path) {
                            report.files_inventory_only += 1;
                            continue;
                        }
                        match instrument.time(Metric::SkimNanos, || {
                            rosetta_capture::sqlite_file_status(&root.join(&file.relative_path))
                        })? {
                            rosetta_capture::SqliteFileStatus::Missing => {
                                report.files_vanished += 1;
                                instrument.add(Metric::FilesVanished, 1);
                                continue;
                            }
                            rosetta_capture::SqliteFileStatus::NotDatabase => {
                                // Broad database globs also see agent-owned
                                // lock and marker files. Preserve them in the
                                // inventory count, but never manufacture an
                                // empty SQLite source.
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
                    Entry::Read(stream, driver) => (stream, driver),
                };
                match next()? {
                    crate::skim::Skimmed::Vanished => {
                        report.files_vanished += 1;
                        instrument.add(Metric::FilesVanished, 1);
                    }
                    crate::skim::Skimmed::Unfinished => {
                        report.files_skimmed += 1;
                        report.files_unfinished += 1;
                        instrument.add(Metric::FilesUnfinished, 1);
                    }
                    crate::skim::Skimmed::Transcript(skim) => {
                        report.files_skimmed += 1;
                        admit_skim(
                            runtime,
                            &mut report,
                            &mut output,
                            file,
                            stream,
                            driver,
                            *skim,
                            false,
                            instrument,
                        );
                    }
                    // One physical document may enumerate several native
                    // sessions, each an independently replayable virtual
                    // source (`<document-path>#<native-id>`). A member the
                    // document no longer lists keeps its last projected
                    // generation, like a removed SQLite member and a deleted
                    // JSONL transcript (product decision, 2026-09-24).
                    crate::skim::Skimmed::Document(skims) => {
                        report.files_skimmed += 1;
                        for skim in skims {
                            admit_skim(
                                runtime,
                                &mut report,
                                &mut output,
                                file,
                                stream,
                                driver,
                                skim,
                                true,
                                instrument,
                            );
                        }
                    }
                }
            }
            Ok(())
        },
    )?;

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
    let sessions = order_session_dependencies(sessions)?;
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

    // Newest sessions stream first.
    files.sort_by_key(|file| std::cmp::Reverse(file.modified_ms));
    Ok((report, files, ops))
}

/// Sidecars can refine a previously merged child to name a parent first seen
/// later in the inventory. Main-before-subagent is insufficient for nested
/// trees: both session foreign keys must precede their dependents. Keep the
/// original order among ready rows so metadata and ledger output stay stable.
fn order_session_dependencies(sessions: Vec<SessionRow>) -> Result<Vec<SessionRow>, PipelineError> {
    let positions: HashMap<&str, usize> = sessions
        .iter()
        .enumerate()
        .map(|(index, row)| (row.id.as_str(), index))
        .collect();
    let mut pending = vec![0_usize; sessions.len()];
    let mut children = vec![Vec::new(); sessions.len()];
    for (index, row) in sessions.iter().enumerate() {
        for dependency in [
            row.parent_session_id.as_deref(),
            Some(row.root_session_id.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(&parent) = positions.get(dependency)
                && parent != index
            {
                pending[index] += 1;
                children[parent].push(index);
            }
        }
    }
    let mut ready: BTreeSet<usize> = pending
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect();
    let mut rows: Vec<_> = sessions.into_iter().map(Some).collect();
    let mut ordered = Vec::with_capacity(rows.len());
    while let Some(index) = ready.pop_first() {
        ordered.push(rows[index].take().expect("each session is ordered once"));
        for &child in &children[index] {
            pending[child] -= 1;
            if pending[child] == 0 {
                ready.insert(child);
            }
        }
    }
    if ordered.len() != rows.len() {
        return Err(PipelineError::Config(
            "cyclic session metadata dependencies".into(),
        ));
    }
    Ok(ordered)
}

impl MetaOutput {
    /// Fold one bounded fact about a session into this pass's row for it. A
    /// session can span several files, or reappear across runs; an index by
    /// id keeps each fold constant-time while rows keep first-merge order.
    fn merge_session(&mut self, row: SessionRow) {
        let Some(&position) = self.session_positions.get(&row.id) else {
            self.session_positions
                .insert(row.id.clone(), self.sessions.len());
            self.sessions.push(row);
            return;
        };
        let existing = &mut self.sessions[position];
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
    let mut resolver = crate::project::ProjectResolver::default();
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
            Some(ProjectHint::Root { root, git_repo }) => Some(resolver.project_from_hint(
                session.cwd.as_deref().unwrap_or(root),
                root,
                git_repo.as_deref(),
            )),
            Some(ProjectHint::SameAsSession { .. }) => None,
            None => session.cwd.as_deref().map(|cwd| resolver.project(cwd)),
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
            extend_project_roots(
                &mut resolver,
                &mut project,
                output.sessions[index].cwd.as_deref(),
            );
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
            .or_else(|| session.cwd.as_deref().map(|cwd| resolver.project(cwd)));
        session.project_id = project.as_ref().map(|project| project.id.clone());
        if let Some(project) = project {
            merge_project(&mut projects, project);
        }
    }
    output.projects = projects.into_values().collect();
}

fn extend_project_roots(
    resolver: &mut crate::project::ProjectResolver,
    project: &mut ProjectRow,
    cwd: Option<&str>,
) {
    let Some(cwd) = cwd else {
        return;
    };
    let observed = resolver.project(cwd);
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
        output.merge_session(SessionRow {
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
        });
    }

    // A retained subagent transcript can outlive its parent's transcript.
    // Materialize an honest parent placeholder so the relationship and FK
    // survive GC; a later main skim refines the same deterministic row.
    if let (Some(parent_native), Some(parent_id)) = (
        skim.parent_session_id.as_deref(),
        parent_session_id.as_ref(),
    ) {
        let nested_parent = parent_id != &root_session_id;
        output.merge_session(SessionRow {
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
        });
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
    output.merge_session(row);

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

/// Stage one skimmed source: its meta ops, then its identified session and
/// source row. A document member's source is virtual
/// (`<document-path>#<native-id>`); an unidentified skim is only counted.
#[allow(clippy::too_many_arguments)]
fn admit_skim(
    runtime: &RuntimeRow,
    report: &mut MetaReport,
    output: &mut MetaOutput,
    file: &rosetta_capture::ScannedFile,
    stream: &StreamSpec,
    driver: StreamDriver,
    mut skim: SkimResult,
    member: bool,
    instrument: &Instrument,
) {
    if !skim.ledger_only {
        output.ops.extend(std::mem::take(&mut skim.ops));
    }
    let Some(native_session_id) = skim.session_id.clone() else {
        report.files_unidentified += 1;
        instrument.add(Metric::FilesUnidentified, 1);
        return;
    };
    let relative_path = if member {
        format!("{}#{native_session_id}", file.relative_path)
    } else {
        file.relative_path.clone()
    };
    admit_identified(
        runtime,
        output,
        AdmitSource {
            relative_path,
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
            instrument.add(Metric::FilesVanished, 1);
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
            instrument.add(Metric::FilesUnidentified, 1);
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
#[path = "meta_tests.rs"]
mod tests;
