//! Asynchronous FTS5 sidecar builder and repair consumer.
//!
//! The worker never writes canonical SQLite. It reads one canonical snapshot,
//! commits the disposable search generation, then sends an acknowledgement to
//! the daemon's single canonical writer.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rosetta_core::timeline::SearchLane;
use rosetta_store::queries::{self, ProjectionState, SearchDocument};
use rosetta_store::{StoreError, assets};
use rusqlite::{Connection, OpenFlags, params};

use crate::SearchError;
use crate::sidecar::{
    Fts5SidecarSearch, GenerationState, Manifest, SEARCH_DDL, SEARCH_INDEXES,
    SEARCH_SCHEMA_VERSION, generation_state, open_generation, read_manifest,
};

const WORKER_POLL: Duration = Duration::from_millis(200);
const STOP_CHECK_ROWS: u64 = 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchAck {
    pub projection_epoch: String,
    pub revision: u64,
}

fn backend(message: impl Into<String>) -> SearchError {
    SearchError::Backend(message.into())
}

fn canonical_snapshot(path: &Path) -> Result<(Connection, ProjectionState), SearchError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| backend(format!("opening canonical search snapshot: {error}")))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|error| backend(format!("configuring canonical search snapshot: {error}")))?;
    conn.execute_batch("BEGIN")
        .map_err(|error| backend(format!("beginning canonical search snapshot: {error}")))?;
    let state = queries::projection_state(&conn)
        .map_err(|error| backend(format!("reading canonical search snapshot: {error}")))?;
    Ok((conn, state))
}

fn insert_document(
    canonical_path: &Path,
    mut document: SearchDocument,
    metadata: &mut rusqlite::Statement<'_>,
    fts: &mut rusqlite::Statement<'_>,
) -> Result<(), StoreError> {
    let members = std::mem::take(&mut document.members);
    for member in &members {
        remove_preview(search_lane(&mut document, member.lane), &member.preview);
    }
    for member in members {
        let text = assets::read_text_member(canonical_path, &member.asset)?;
        push_search_value(search_lane(&mut document, member.lane), &text);
    }
    metadata.execute(params![
        document.id,
        document.timeline_id,
        document.session_id,
        document.runtime_id,
        document.agent,
        document.project_id,
        document.ts,
        document.kind,
        document.is_error,
    ])?;
    fts.execute(params![
        document.id,
        document.prose,
        document.target,
        document.intent,
        document.change,
        document.tool,
    ])?;
    Ok(())
}

fn search_lane(document: &mut SearchDocument, lane: SearchLane) -> &mut String {
    match lane {
        SearchLane::Prose => &mut document.prose,
        SearchLane::Target => &mut document.target,
        SearchLane::Intent => &mut document.intent,
        SearchLane::Change => &mut document.change,
        SearchLane::Tool => &mut document.tool,
    }
}

fn push_search_value(lane: &mut String, value: &str) {
    if value.is_empty() {
        return;
    }
    if !lane.is_empty() {
        lane.push('\n');
    }
    lane.push_str(value);
}

/// Remove exactly one preview value from the newline-composed lane before
/// adding its full member. Preview text itself may contain newlines, so this
/// checks only the boundaries around the complete byte string rather than
/// splitting it into lines.
fn remove_preview(lane: &mut String, preview: &str) {
    if preview.is_empty() {
        return;
    }
    let mut from = 0;
    while let Some(relative) = lane[from..].find(preview) {
        let start = from + relative;
        let end = start + preview.len();
        let before = start == 0 || lane.as_bytes()[start - 1] == b'\n';
        let after = end == lane.len() || lane.as_bytes()[end] == b'\n';
        if before && after {
            if start == 0 && end < lane.len() {
                lane.replace_range(start..end + 1, "");
            } else if start > 0 {
                lane.replace_range(start - 1..end, "");
            } else {
                lane.clear();
            }
            return;
        }
        from = end;
    }
}

fn finish_private_generation(
    conn: &mut Connection,
    state: &ProjectionState,
    canonical: &Connection,
    canonical_path: &Path,
    stop: &AtomicBool,
) -> Result<u64, SearchError> {
    conn.execute_batch(
        "PRAGMA journal_mode = MEMORY;
         PRAGMA synchronous = OFF;",
    )
    .map_err(|error| backend(format!("configuring private search build: {error}")))?;
    conn.execute_batch(SEARCH_DDL)
        .map_err(|error| backend(format!("creating private search schema: {error}")))?;
    let tx = conn
        .transaction()
        .map_err(|error| backend(format!("starting private search build: {error}")))?;
    let mut metadata = tx
        .prepare(
            "INSERT INTO search_document
               (id, timeline_id, session_id, runtime_id, agent, project_id,
                ts, kind, is_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .map_err(|error| backend(format!("preparing search metadata insert: {error}")))?;
    let mut fts = tx
        .prepare(
            "INSERT INTO message_fts(rowid, prose, target, intent, change, tool)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(|error| backend(format!("preparing search text insert: {error}")))?;
    let mut documents = 0_u64;
    queries::visit_search_documents(canonical, |document| {
        documents += 1;
        if documents.is_multiple_of(STOP_CHECK_ROWS) && stop.load(Ordering::SeqCst) {
            return Err(StoreError::ProjectionInvalid {
                message: "search build interrupted".into(),
            });
        }
        insert_document(canonical_path, document, &mut metadata, &mut fts)
    })
    .map_err(|error| backend(format!("building search documents: {error}")))?;
    drop(fts);
    drop(metadata);
    tx.execute(
        "INSERT INTO search_state
           (singleton, search_schema, projection_epoch, revision)
         VALUES (1, ?1, ?2, ?3)",
        params![
            SEARCH_SCHEMA_VERSION,
            state.projection_epoch,
            i64::try_from(state.search_revision)
                .map_err(|_| backend("canonical search revision exceeds SQLite integer range"))?,
        ],
    )
    .map_err(|error| backend(format!("recording private search state: {error}")))?;
    tx.execute_batch(SEARCH_INDEXES)
        .map_err(|error| backend(format!("building search metadata indexes: {error}")))?;
    tx.execute(
        "INSERT INTO message_fts(message_fts, rank) VALUES('integrity-check', 1)",
        [],
    )
    .map_err(|error| backend(format!("checking private search index: {error}")))?;
    tx.commit()
        .map_err(|error| backend(format!("committing private search build: {error}")))?;
    let quick: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| backend(format!("validating private search build: {error}")))?;
    if quick != "ok" {
        return Err(backend(format!(
            "private search quick_check returned {quick}"
        )));
    }
    conn.execute_batch(
        "PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = FULL;",
    )
    .map_err(|error| backend(format!("finalizing private search journal: {error}")))?;
    Ok(documents)
}

fn generation_name(epoch: &str, revision: u64) -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        "{epoch}-{SEARCH_SCHEMA_VERSION}-{revision}-{}-{nonce}.sqlite",
        std::process::id()
    )
}

fn publish_manifest(search: &Fts5SidecarSearch, manifest: &Manifest) -> Result<(), SearchError> {
    let mut temporary = tempfile::NamedTempFile::new_in(&search.layout.root)
        .map_err(|error| backend(format!("creating search CURRENT temporary: {error}")))?;
    serde_json::to_writer(&mut temporary, manifest)
        .map_err(|error| backend(format!("serializing search CURRENT: {error}")))?;
    temporary
        .write_all(b"\n")
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| backend(format!("syncing search CURRENT temporary: {error}")))?;
    temporary
        .persist(&search.layout.current)
        .map_err(|error| backend(format!("publishing search CURRENT: {}", error.error)))?;
    search.layout.sync_root()
}

fn full_build(
    search: &Fts5SidecarSearch,
    canonical: &Connection,
    state: &ProjectionState,
    stop: &AtomicBool,
) -> Result<SearchAck, SearchError> {
    search.layout.create()?;
    let temporary = tempfile::NamedTempFile::new_in(&search.layout.generations)
        .map_err(|error| backend(format!("creating private search generation: {error}")))?;
    let temporary = temporary.into_temp_path();
    let mut conn = Connection::open(&temporary)
        .map_err(|error| backend(format!("opening private search generation: {error}")))?;
    finish_private_generation(&mut conn, state, canonical, search.canonical_path(), stop)?;
    drop(conn);
    if stop.load(Ordering::SeqCst) {
        return Err(backend("search build interrupted"));
    }
    File::open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| backend(format!("syncing private search generation: {error}")))?;
    let generation = generation_name(&state.projection_epoch, state.search_revision);
    let destination = search.layout.generations.join(&generation);
    temporary
        .persist(&destination)
        .map_err(|error| backend(format!("publishing search generation: {}", error.error)))?;
    search.layout.sync_generations()?;
    publish_manifest(
        search,
        &Manifest {
            format: 1,
            generation,
            projection_epoch: state.projection_epoch.clone(),
            search_schema: SEARCH_SCHEMA_VERSION,
        },
    )?;
    Ok(SearchAck {
        projection_epoch: state.projection_epoch.clone(),
        revision: state.search_revision,
    })
}

fn repair_generation(
    search: &Fts5SidecarSearch,
    canonical: &Connection,
    state: &ProjectionState,
    manifest: &Manifest,
    generation: &GenerationState,
    stop: &AtomicBool,
) -> Result<SearchAck, SearchError> {
    let repairs = queries::search_repairs(canonical, generation.revision, state.search_revision)
        .map_err(|error| backend(format!("reading search repairs: {error}")))?;
    if repairs.is_empty() {
        return full_build(search, canonical, state, stop);
    }
    let mut conn = open_generation(&search.layout, manifest, false)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;",
    )
    .map_err(|error| backend(format!("configuring search repair journal: {error}")))?;
    let tx = conn
        .transaction()
        .map_err(|error| backend(format!("starting search repair: {error}")))?;
    let mut delete_fts = tx
        .prepare(
            "DELETE FROM message_fts
             WHERE rowid IN (
               SELECT id FROM search_document WHERE session_id = ?1
             )",
        )
        .map_err(|error| backend(format!("preparing search text repair: {error}")))?;
    let mut delete_metadata = tx
        .prepare("DELETE FROM search_document WHERE session_id = ?1")
        .map_err(|error| backend(format!("preparing search metadata repair: {error}")))?;
    let mut metadata = tx
        .prepare(
            "INSERT INTO search_document
               (id, timeline_id, session_id, runtime_id, agent, project_id,
                ts, kind, is_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .map_err(|error| backend(format!("preparing repaired search metadata: {error}")))?;
    let mut fts = tx
        .prepare(
            "INSERT INTO message_fts(rowid, prose, target, intent, change, tool)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(|error| backend(format!("preparing repaired search text: {error}")))?;
    for repair in &repairs {
        if stop.load(Ordering::SeqCst) {
            return Err(backend("search repair interrupted"));
        }
        if repair.kind != "session" {
            return Err(backend(format!(
                "unsupported search repair kind {}",
                repair.kind
            )));
        }
        delete_fts
            .execute(params![repair.key])
            .map_err(|error| backend(format!("deleting stale search text: {error}")))?;
        delete_metadata
            .execute(params![repair.key])
            .map_err(|error| backend(format!("deleting stale search metadata: {error}")))?;
        queries::visit_session_search_documents(canonical, &repair.key, |document| {
            insert_document(search.canonical_path(), document, &mut metadata, &mut fts)
        })
        .map_err(|error| backend(format!("reloading repaired search session: {error}")))?;
    }
    drop(fts);
    drop(metadata);
    drop(delete_metadata);
    drop(delete_fts);
    tx.execute(
        "UPDATE search_state SET revision = ?1 WHERE singleton = 1",
        params![
            i64::try_from(state.search_revision)
                .map_err(|_| backend("canonical search revision exceeds SQLite integer range"))?
        ],
    )
    .map_err(|error| backend(format!("advancing repaired search revision: {error}")))?;
    tx.commit()
        .map_err(|error| backend(format!("committing search repair: {error}")))?;
    Ok(SearchAck {
        projection_epoch: state.projection_epoch.clone(),
        revision: state.search_revision,
    })
}

/// Bring the sidecar through one canonical snapshot. The caller applies the
/// returned acknowledgement through the canonical store's single writer.
pub fn synchronize_fts5_once(
    search: &Fts5SidecarSearch,
    stop: &AtomicBool,
) -> Result<Option<SearchAck>, SearchError> {
    let (canonical, state) = canonical_snapshot(search.canonical_path())?;
    if !state.canonical_ready {
        return Ok(None);
    }
    let manifest = read_manifest(&search.layout).ok().flatten();
    let Some(manifest) = manifest else {
        return full_build(search, &canonical, &state, stop).map(Some);
    };
    let generation_conn = match open_generation(&search.layout, &manifest, true) {
        Ok(conn) => conn,
        Err(_) => return full_build(search, &canonical, &state, stop).map(Some),
    };
    let generation = match generation_state(&generation_conn) {
        Ok(generation) => generation,
        Err(_) => return full_build(search, &canonical, &state, stop).map(Some),
    };
    drop(generation_conn);
    if generation.projection_epoch != state.projection_epoch
        || generation.revision > state.search_revision
    {
        return full_build(search, &canonical, &state, stop).map(Some);
    }
    if generation.revision < state.search_revision {
        return repair_generation(search, &canonical, &state, &manifest, &generation, stop)
            .map(Some);
    }
    Ok(Some(SearchAck {
        projection_epoch: state.projection_epoch,
        revision: state.search_revision,
    }))
}

pub struct Fts5SearchWorker {
    stop: Arc<AtomicBool>,
    wake: SyncSender<()>,
    acknowledgements: Receiver<SearchAck>,
    thread: Option<JoinHandle<()>>,
}

impl Fts5SearchWorker {
    pub fn start(search: Fts5SidecarSearch) -> Self {
        Self::start_inner(search, false)
    }

    /// Construct the detached worker without touching the canonical path
    /// until [`Fts5SearchWorker::wake`]. Fresh serve binds before its durable
    /// projection exists; probing that path every 200 ms would turn an
    /// expected cold state into a misleading search failure.
    pub fn start_paused(search: Fts5SidecarSearch) -> Self {
        Self::start_inner(search, true)
    }

    fn start_inner(search: Fts5SidecarSearch, paused: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (wake, commands) = std::sync::mpsc::sync_channel(1);
        let (acknowledgements_tx, acknowledgements) = std::sync::mpsc::sync_channel(8);
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("rosetta-search".into())
            .spawn(move || {
                let mut last_ack = None;
                if paused {
                    let _ = commands.recv();
                }
                while !thread_stop.load(Ordering::SeqCst) {
                    match synchronize_fts5_once(&search, &thread_stop) {
                        Ok(ack) => {
                            search.set_worker_error(None);
                            if ack != last_ack {
                                let delivered = if let Some(candidate) = ack.clone() {
                                    match acknowledgements_tx.try_send(candidate) {
                                        Ok(()) => true,
                                        Err(TrySendError::Full(_)) => false,
                                        Err(TrySendError::Disconnected(_)) => break,
                                    }
                                } else {
                                    true
                                };
                                if delivered {
                                    last_ack = ack;
                                }
                            }
                        }
                        Err(_) if thread_stop.load(Ordering::SeqCst) => break,
                        Err(error) => {
                            search.set_worker_error(Some(error.to_string()));
                            last_ack = None;
                        }
                    }
                    let _ = commands.recv_timeout(WORKER_POLL);
                }
            })
            .expect("starting search worker thread");
        Self {
            stop,
            wake,
            acknowledgements,
            thread: Some(thread),
        }
    }

    pub fn wake(&self) {
        let _ = self.wake.try_send(());
    }

    pub fn drain_acknowledgements(&self) -> impl Iterator<Item = SearchAck> + '_ {
        self.acknowledgements.try_iter()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Fts5SearchWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.try_send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
