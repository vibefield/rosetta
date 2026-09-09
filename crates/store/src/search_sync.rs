//! Canonical/search synchronization state.
//!
//! Canonical transactions record only coalesced session repair keys. The
//! detached search worker reads them through `queries`, commits its own index,
//! then asks the canonical single writer to acknowledge a watermark. Search
//! never writes this database directly.

use std::collections::{BTreeMap, BTreeSet};

use rosetta_core::IngestOp;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::{Batch, SourceMode, StoreError};

type SessionSearchMetadata = (String, Option<String>);

/// Search-relevant values captured before the canonical apply. Many live
/// ticks replay identical Runtime/SessionMeta rows; comparing the indexed
/// subset prevents those no-ops from manufacturing revisions or whole-runtime
/// repair storms.
pub(crate) struct BeforeBatch {
    runtime_kinds: BTreeMap<String, Option<String>>,
    session_metadata: BTreeMap<String, Option<SessionSearchMetadata>>,
}

fn runtime_kind(tx: &Transaction<'_>, runtime_id: &str) -> Result<Option<String>, StoreError> {
    Ok(tx
        .prepare_cached("SELECT kind FROM agent_runtime WHERE id = ?1")?
        .query_row(params![runtime_id], |row| row.get(0))
        .optional()?)
}

fn session_metadata(
    tx: &Transaction<'_>,
    session_id: &str,
) -> Result<Option<SessionSearchMetadata>, StoreError> {
    Ok(tx
        .prepare_cached("SELECT runtime_id, project_id FROM session WHERE id = ?1")?
        .query_row(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?)
}

pub(crate) fn before_batch(tx: &Transaction<'_>, batch: &Batch) -> Result<BeforeBatch, StoreError> {
    let mut runtime_ids = BTreeSet::new();
    let mut session_ids = BTreeSet::new();
    for op in &batch.ops {
        match op {
            IngestOp::Runtime(row) => {
                runtime_ids.insert(row.id.clone());
            }
            IngestOp::SessionMeta(row) => {
                session_ids.insert(row.id.clone());
            }
            _ => {}
        }
    }
    let runtime_kinds = runtime_ids
        .into_iter()
        .map(|id| Ok((id.clone(), runtime_kind(tx, &id)?)))
        .collect::<Result<_, StoreError>>()?;
    let session_metadata = session_ids
        .into_iter()
        .map(|id| Ok((id.clone(), session_metadata(tx, &id)?)))
        .collect::<Result<_, StoreError>>()?;
    Ok(BeforeBatch {
        runtime_kinds,
        session_metadata,
    })
}

fn affected_sessions(
    tx: &Transaction<'_>,
    batch: &Batch,
    before: BeforeBatch,
) -> Result<BTreeSet<String>, StoreError> {
    let mut sessions = BTreeSet::new();

    // Source stages and abandons do not change canonical search documents.
    // Appends and activations can add, replace, or remove any document owned
    // by that source, so their canonical session is the repair grain.
    let source_applied = batch
        .source
        .as_ref()
        .is_some_and(|source| matches!(source.mode, SourceMode::Append | SourceMode::Activate));
    if source_applied
        && let Some(source) = &batch.source
        && session_metadata(tx, &source.session_id)?.is_some()
    {
        sessions.insert(source.session_id.clone());
    }

    // Direct message/result batches are search text mutations. Source-bound
    // equivalents are already covered at session grain above; inspecting
    // their ops would incorrectly dirty staged replacement chunks.
    if batch.source.is_none() {
        for op in &batch.ops {
            match op {
                IngestOp::Message(row) => {
                    sessions.insert(row.session_id.clone());
                }
                IngestOp::ToolUpdate { session_id, .. } => {
                    sessions.insert(session_id.clone());
                }
                _ => {}
            }
        }
    }

    // Only runtime kind is materialized in the search sidecar. A repeated
    // runtime heartbeat/config update with the same kind must be a search
    // no-op; a real kind change repairs every session owned by the runtime.
    let mut statement = tx.prepare_cached("SELECT id FROM session WHERE runtime_id = ?1")?;
    for (runtime_id, old_kind) in before.runtime_kinds {
        if runtime_kind(tx, &runtime_id)? == old_kind {
            continue;
        }
        let rows = statement.query_map(params![runtime_id], |row| row.get(0))?;
        for session in rows {
            sessions.insert(session?);
        }
    }

    // Session runtime/project are the only session fields copied into search.
    // Titles, stream-state flips, and timestamp touches stay canonical-only.
    for (session_id, old_metadata) in before.session_metadata {
        if session_metadata(tx, &session_id)? != old_metadata {
            sessions.insert(session_id);
        }
    }
    Ok(sessions)
}

/// Increment once for a search-relevant canonical transaction and coalesce
/// every affected session at that revision.
pub(crate) fn record_batch(
    tx: &Transaction<'_>,
    batch: &Batch,
    before: BeforeBatch,
) -> Result<(), StoreError> {
    let sessions = affected_sessions(tx, batch, before)?;
    if sessions.is_empty() {
        return Ok(());
    }
    let revision: i64 = tx.query_row(
        "UPDATE projection_state
         SET search_revision = search_revision + 1
         WHERE singleton = 1
         RETURNING search_revision",
        [],
        |row| row.get(0),
    )?;
    let mut statement = tx.prepare_cached(
        "INSERT INTO search_dirty (kind, key, revision)
         VALUES ('session', ?1, ?2)
         ON CONFLICT(kind, key) DO UPDATE SET revision = excluded.revision",
    )?;
    for session in sessions {
        statement.execute(params![session, revision])?;
    }
    Ok(())
}

pub(crate) fn mark_canonical_ready(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let changed = tx.execute(
        "UPDATE projection_state SET canonical_ready = 1 WHERE singleton = 1",
        [],
    )?;
    if changed != 1 {
        return Err(StoreError::ProjectionInvalid {
            message: "projection_state singleton is missing".into(),
        });
    }
    Ok(())
}

/// Acknowledge only a worker for the current projection epoch. Replaying an
/// acknowledgement is harmless, while a stale worker cannot erase a newer
/// projection's repairs.
pub(crate) fn acknowledge(
    tx: &Transaction<'_>,
    projection_epoch: &str,
    through_revision: u64,
) -> Result<bool, StoreError> {
    let revision = i64::try_from(through_revision).map_err(|_| StoreError::ProjectionInvalid {
        message: format!("search revision {through_revision} exceeds SQLite's integer range"),
    })?;
    let current: Option<String> = tx
        .query_row(
            "SELECT projection_epoch FROM projection_state
             WHERE singleton = 1 AND projection_epoch = ?1",
            params![projection_epoch],
            |row| row.get(0),
        )
        .optional()?;
    if current.is_none() {
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM search_dirty WHERE revision <= ?1",
        params![revision],
    )?;
    Ok(true)
}
