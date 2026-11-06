//! The global HTML review library and its source-owned publication history.

use std::collections::HashSet;

use crate::StoreError;
use rosetta_core::{
    html_artifacts::{
        HTML_MEDIA_TYPE, HtmlArtifactAvailability, HtmlArtifactContent, HtmlArtifactPublication,
    },
    rows::StreamState,
    timeline::AssetRef,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ArtifactListRequest {
    pub q: Option<String>,
    pub agent: Option<String>,
    pub project: Option<String>,
    /// Omitted: latest presentation of each artifact. Set: its full history.
    pub artifact_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ArtifactEntry {
    pub publication: HtmlArtifactPublication,
    pub availability: HtmlArtifactAvailability,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub captured_at: Option<i64>,
    pub recovered_from: Option<String>,
    pub asset: Option<AssetRef>,
    pub agent: String,
    pub session_title: Option<String>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub presentations: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ArtifactPage {
    pub entries: Vec<ArtifactEntry>,
    pub next_cursor: Option<String>,
    /// False while the library may still grow during initial ingestion.
    pub canonical_ready: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Cursor {
    epoch: String,
    /// The library revision (`projection_state.artifacts_revision`, schema
    /// 30). It replaced `search_revision`, which moved with every new message
    /// and so expired a walk while agents worked (RO-35).
    revision: i64,
    /// Whether the page was read behind the cold admission barrier. Lifting
    /// the barrier reveals rows without a revision step, so it expires too.
    #[serde(default)]
    hidden: bool,
    query: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ),
    at: i64,
    id: String,
}

#[derive(Debug)]
pub enum ListError {
    Invalid,
    Expired,
    Store(StoreError),
}
impl From<StoreError> for ListError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}
impl From<rusqlite::Error> for ListError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.into())
    }
}

/// Upsert one publication. Returns the rows it changed: 0 when a replay
/// restates the facts already held, so a live transaction steps the library
/// revision only when the library moved.
pub(crate) fn apply_publication(
    tx: &Transaction,
    row: &HtmlArtifactPublication,
    src: Option<i64>,
) -> Result<u64, StoreError> {
    row.validate()
        .map_err(|message| StoreError::ProjectionInvalid {
            message: message.into(),
        })?;
    let changed = tx.prepare_cached("INSERT INTO html_artifact
        (id,artifact_id,session_id,timeline_id,title,filename,path,url,native_id,version,published_at,sort_at,source_offset,src_cursor,description,dropped_files)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
        ON CONFLICT(id) DO UPDATE SET artifact_id=excluded.artifact_id, title=excluded.title,
        url=excluded.url,native_id=excluded.native_id,version=excluded.version,
        published_at=excluded.published_at,sort_at=excluded.sort_at,
        availability=CASE WHEN source_offset!=excluded.source_offset OR src_cursor IS NOT excluded.src_cursor THEN 'not_captured' ELSE availability END,
        captured_at=CASE WHEN source_offset!=excluded.source_offset OR src_cursor IS NOT excluded.src_cursor THEN NULL ELSE captured_at END,
        recovered_from=CASE WHEN source_offset!=excluded.source_offset OR src_cursor IS NOT excluded.src_cursor THEN NULL ELSE recovered_from END,
        asset=CASE WHEN source_offset!=excluded.source_offset OR src_cursor IS NOT excluded.src_cursor THEN NULL ELSE asset END,
        source_offset=excluded.source_offset,src_cursor=excluded.src_cursor,
        description=excluded.description,dropped_files=excluded.dropped_files
        WHERE artifact_id IS NOT excluded.artifact_id OR title IS NOT excluded.title
        OR url IS NOT excluded.url OR native_id IS NOT excluded.native_id
        OR version IS NOT excluded.version OR published_at IS NOT excluded.published_at
        OR source_offset IS NOT excluded.source_offset OR src_cursor IS NOT excluded.src_cursor
        OR description IS NOT excluded.description OR dropped_files IS NOT excluded.dropped_files")?
        .execute(params![row.id,row.artifact_id,row.session_id,row.timeline_id,row.title,row.filename,row.path,row.url,row.native_id,row.version,row.published_at,row.published_at.unwrap_or(0),row.source_offset,src,row.description,row.dropped_files])?;
    Ok(changed as u64)
}

/// Enrich the publications at one coordinate with an observation. Returns
/// the rows whose capture changed: a replay of the same observation is 0.
pub(crate) fn apply_content(
    tx: &Transaction,
    row: &HtmlArtifactContent,
    src: Option<i64>,
) -> Result<u64, StoreError> {
    if row.recovered_from.as_ref().is_some_and(|from| {
        from.is_empty()
            || from.len() > 32768
            || from.contains('\0')
            || row.availability != HtmlArtifactAvailability::Captured
    }) {
        return Err(StoreError::AssetInvalid {
            message: "invalid HTML recovery provenance".into(),
        });
    }
    if let Some(asset) = &row.asset {
        asset.validate().map_err(|e| StoreError::AssetInvalid {
            message: e.to_string(),
        })?;
        if asset.media_type != HTML_MEDIA_TYPE
            || row.availability != HtmlArtifactAvailability::Captured
        {
            return Err(StoreError::AssetInvalid {
                message: "invalid HTML artifact capture".into(),
            });
        }
    } else if row.availability == HtmlArtifactAvailability::Captured {
        return Err(StoreError::AssetInvalid {
            message: "HTML capture is missing its content member".into(),
        });
    }
    let status = serde_json::to_value(row.availability)?
        .as_str()
        .expect("status enum")
        .to_owned();
    let asset = row.asset.as_ref().map(serde_json::to_string).transpose()?;
    // Coordinate + source ownership prevents one result/file from enriching a
    // different publication or a replacement generation's rows. A row that
    // already holds this observation is left alone, so it counts no change.
    let mut statement = tx.prepare_cached(
        "UPDATE html_artifact SET availability=?1,captured_at=?2,asset=?3,recovered_from=?8
        WHERE session_id=?4 AND source_offset=?5 AND path IS ?6 AND src_cursor IS ?7
        AND (availability IS NOT ?1 OR captured_at IS NOT ?2 OR asset IS NOT ?3
            OR recovered_from IS NOT ?8)",
    )?;
    let changed = statement.execute(params![
        status,
        row.captured_at,
        asset,
        row.session_id,
        row.source_offset,
        row.path,
        src,
        row.recovered_from
    ])?;
    Ok(changed as u64)
}

/// One step of the library revision (RO-35). The store calls it once per
/// writer transaction that changed what the library shows, in every mode: a
/// publication or capture written or swept, or a readiness flip across
/// Pending of a session that holds one ([`flip_moves`]). Two builds of one
/// ledger may hold different values (the count follows their transactions);
/// only a change of value means anything.
pub(crate) fn step_revision(tx: &Transaction) -> Result<u64, StoreError> {
    let revision: i64 = tx.query_row(
        "UPDATE projection_state
         SET artifacts_revision = artifacts_revision + 1
         WHERE singleton = 1
         RETURNING artifacts_revision",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::ProjectionInvalid {
        message: format!("negative artifact library revision {revision}"),
    })
}

/// The library revision: the list cursor's binding, `/api/status`'s
/// `artifacts_revision`, and what `/ws/meta`'s `artifacts_changed` announces.
pub fn revision(conn: &Connection) -> Result<u64, StoreError> {
    let revision: i64 = conn.query_row(
        "SELECT artifacts_revision FROM projection_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u64::try_from(revision).map_err(|_| StoreError::ProjectionInvalid {
        message: format!("negative artifact library revision {revision}"),
    })
}

/// The sessions that hold a publication: the only ones whose readiness flip
/// can move the library. One pass over the session index, so a batch flip
/// checks each of its sessions in RAM and reads only a publisher's state.
pub(crate) fn publishing_sessions(conn: &Connection) -> Result<HashSet<String>, StoreError> {
    let mut statement = conn.prepare_cached("SELECT DISTINCT session_id FROM html_artifact")?;
    let sessions = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(sessions)
}

/// Whether moving `session_id` to `next` crosses Pending while the session
/// holds a publication: the readiness flip that changes what the library
/// shows. The admission barrier hides a Pending session's publications, and
/// every list leaves them out of version counts and links. Read before the
/// flip is written.
pub(crate) fn flip_moves(
    conn: &Connection,
    session_id: &str,
    next: StreamState,
) -> Result<bool, StoreError> {
    Ok(conn
        .prepare_cached(
            "SELECT (s.stream_state = 'pending') != (?2 = 'pending')
                AND EXISTS (SELECT 1 FROM html_artifact a WHERE a.session_id = s.id)
             FROM session s WHERE s.id = ?1",
        )?
        .query_row(params![session_id, next.as_str()], |row| row.get(0))
        .optional()?
        .unwrap_or(false))
}

/// Whether a Pending session holds a publication: what lifting the admission
/// barrier shows without a revision step, so the mirror announces it then.
pub fn held_by_pending(conn: &Connection) -> Result<bool, StoreError> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM html_artifact a JOIN session s ON s.id = a.session_id
             WHERE s.stream_state = 'pending')",
        )?
        .query_row([], |row| row.get(0))?)
}

const COLUMNS: &str = "a.id,a.artifact_id,a.session_id,a.timeline_id,a.title,a.filename,a.path,
    COALESCE(a.url,(SELECT u.url FROM html_artifact u JOIN session us ON us.id=u.session_id
        WHERE u.artifact_id=a.artifact_id AND u.url IS NOT NULL AND us.stream_state!='pending'
        ORDER BY u.sort_at DESC,u.id DESC LIMIT 1)),a.native_id,a.version,a.published_at,a.source_offset,a.availability,a.captured_at,a.asset,r.kind,s.title,p.id,p.name,
    (SELECT count(*) FROM html_artifact versions JOIN session vs ON vs.id=versions.session_id
     WHERE versions.artifact_id=a.artifact_id AND vs.stream_state!='pending'),a.recovered_from,
    a.description,a.dropped_files";
const JOINS: &str = "FROM html_artifact a JOIN session s ON s.id=a.session_id
    JOIN agent_runtime r ON r.id=s.runtime_id LEFT JOIN session root ON root.id=s.root_session_id
    LEFT JOIN project p ON p.id=root.project_id";

fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactEntry> {
    let json_error = |index, error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    let status: String = row.get(12)?;
    let asset: Option<String> = row.get(14)?;
    Ok(ArtifactEntry {
        publication: HtmlArtifactPublication {
            id: row.get(0)?,
            artifact_id: row.get(1)?,
            session_id: row.get(2)?,
            timeline_id: row.get(3)?,
            title: row.get(4)?,
            filename: row.get(5)?,
            path: row.get(6)?,
            url: row.get(7)?,
            native_id: row.get(8)?,
            version: row.get(9)?,
            published_at: row.get(10)?,
            source_offset: row.get(11)?,
            description: row.get(21)?,
            dropped_files: row.get(22)?,
        },
        availability: serde_json::from_value(serde_json::Value::String(status))
            .map_err(|e| json_error(12, e))?,
        captured_at: row.get(13)?,
        recovered_from: row.get(20)?,
        asset: asset
            .map(|a| serde_json::from_str(&a))
            .transpose()
            .map_err(|e| json_error(14, e))?,
        agent: row.get(15)?,
        session_title: row.get(16)?,
        project_id: row.get(17)?,
        project_name: row.get(18)?,
        presentations: row.get(19)?,
    })
}

pub fn list(
    conn: &Connection,
    request: &ArtifactListRequest,
    hide_pending: bool,
) -> Result<ArtifactPage, ListError> {
    if request.q.as_ref().is_some_and(|q| q.len() > 512)
        || [&request.agent, &request.project, &request.artifact_id]
            .iter()
            .any(|v| v.as_ref().is_some_and(|v| v.len() > 256))
        || request.cursor.as_ref().is_some_and(|c| c.len() > 4096)
    {
        return Err(ListError::Invalid);
    }
    // HTTP callers already hold a read snapshot. Direct readers get the same
    // watermark/rows guarantee without attempting a nested transaction.
    let snapshot = conn
        .is_autocommit()
        .then(|| conn.unchecked_transaction())
        .transpose()?;
    let conn: &Connection = snapshot.as_deref().unwrap_or(conn);
    // The library's own revision, never the search watermark: a message that
    // touches no publication must not expire a walk (RO-35, schema 30).
    let (epoch, revision, ready): (String,i64,bool) = conn.query_row("SELECT projection_epoch,artifacts_revision,canonical_ready FROM projection_state WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let query = (
        request.q.clone(),
        request.agent.clone(),
        request.project.clone(),
        request.artifact_id.clone(),
    );
    let cursor = request
        .cursor
        .as_ref()
        .map(|c| serde_json::from_str::<Cursor>(c))
        .transpose()
        .map_err(|_| ListError::Invalid)?;
    if let Some(cursor) = &cursor {
        if cursor.query != query {
            return Err(ListError::Invalid);
        }
        if cursor.epoch != epoch || cursor.revision != revision || cursor.hidden != hide_pending {
            return Err(ListError::Expired);
        }
    }
    let limit = request.limit.unwrap_or(50).clamp(1, 100);
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} {JOINS}
        WHERE (?1 IS NULL OR instr(lower(a.title || ' ' || a.filename || ' ' || coalesce(s.title,'')),lower(?1))>0)
        AND (?2 IS NULL OR r.kind=?2) AND (?3 IS NULL OR p.id=?3)
        AND (?4 IS NULL OR a.artifact_id=?4)
        AND (?4 IS NOT NULL OR NOT EXISTS (SELECT 1 FROM html_artifact newer JOIN session ns ON ns.id=newer.session_id
            WHERE newer.artifact_id=a.artifact_id AND (?5=0 OR ns.stream_state!='pending')
            AND (newer.sort_at,newer.id)>(a.sort_at,a.id)))
        AND (?5=0 OR s.stream_state!='pending')
        AND (?6 IS NULL OR (a.sort_at,a.id)<(?6,?7))
        ORDER BY a.sort_at DESC,a.id DESC LIMIT ?8"))?;
    let mut entries = stmt
        .query_map(
            params![
                request.q,
                request.agent,
                request.project,
                request.artifact_id,
                hide_pending,
                cursor.as_ref().map(|c| c.at),
                cursor.as_ref().map(|c| c.id.as_str()),
                limit + 1
            ],
            decode,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let more = entries.len() > limit as usize;
    entries.truncate(limit as usize);
    let next_cursor = if more {
        entries.last().map(|entry| {
            serde_json::to_string(&Cursor {
                epoch,
                revision,
                hidden: hide_pending,
                query,
                at: entry.publication.published_at.unwrap_or(0),
                id: entry.publication.id.clone(),
            })
            .expect("cursor serialization")
        })
    } else {
        None
    };
    Ok(ArtifactPage {
        entries,
        next_cursor,
        canonical_ready: ready,
    })
}

pub fn get(
    conn: &Connection,
    session_id: &str,
    publication_id: &str,
) -> Result<Option<ArtifactEntry>, StoreError> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {COLUMNS} {JOINS} WHERE a.session_id=?1 AND a.id=?2"
        ))?
        .query_row(params![session_id, publication_id], decode)
        .optional()?)
}

pub(crate) fn for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<ArtifactEntry>, StoreError> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {COLUMNS} {JOINS} WHERE a.session_id=?1
        AND NOT EXISTS (SELECT 1 FROM html_artifact newer WHERE newer.session_id=a.session_id
        AND newer.artifact_id=a.artifact_id AND (newer.sort_at,newer.id)>(a.sort_at,a.id))
        ORDER BY a.sort_at DESC,a.id DESC"
        ))?
        .query_map(params![session_id], decode)?
        .collect::<Result<Vec<_>, _>>()?)
}
