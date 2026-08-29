//! Disposable SQLite FTS5 search projection.
//!
//! Canonical SQLite keeps transactions, reconciliation, and repair keys. This
//! sidecar contains only reloadable search documents and may be deleted at any
//! time. Its three indexed columns and tokenizer intentionally match the old
//! in-file external-content FTS table, preserving BM25 scores and snippets.

use std::ffi::OsString;
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use rosetta_store::queries::{self, ProjectionState, SearchFilter};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::{SearchBackend, SearchError, SearchOutput, SearchQuery, SearchReadiness, SearchState};

pub(crate) const SEARCH_SCHEMA_VERSION: i64 = 1;

pub(crate) const SEARCH_DDL: &str = r#"
CREATE TABLE search_state (
  singleton         INTEGER PRIMARY KEY CHECK (singleton = 1),
  search_schema     INTEGER NOT NULL,
  projection_epoch  TEXT NOT NULL,
  revision          INTEGER NOT NULL CHECK (revision >= 0)
) WITHOUT ROWID;

CREATE TABLE search_document (
  id            INTEGER PRIMARY KEY,
  timeline_id   TEXT NOT NULL UNIQUE,
  session_id    TEXT NOT NULL,
  runtime_id    TEXT NOT NULL,
  agent         TEXT NOT NULL,
  project_id    TEXT,
  ts            INTEGER,
  kind          TEXT NOT NULL,
  is_error      INTEGER
);

CREATE VIRTUAL TABLE message_fts USING fts5(
  content, tool_input, tool_result,
  tokenize='unicode61 remove_diacritics 2'
);
"#;

pub(crate) const SEARCH_INDEXES: &str = r#"
CREATE INDEX search_document_session ON search_document(session_id);
CREATE INDEX search_document_runtime ON search_document(runtime_id);
CREATE INDEX search_document_agent ON search_document(agent);
CREATE INDEX search_document_project ON search_document(project_id);
CREATE INDEX search_document_ts ON search_document(ts);
CREATE INDEX search_document_kind ON search_document(kind);
CREATE INDEX search_document_error ON search_document(is_error);
ANALYZE;
"#;

#[derive(Debug, Clone)]
pub(crate) struct SidecarLayout {
    pub root: PathBuf,
    pub generations: PathBuf,
    pub current: PathBuf,
}

impl SidecarLayout {
    pub fn new(canonical: &Path) -> Self {
        let mut root = OsString::from(canonical.as_os_str());
        root.push(".search");
        let root = PathBuf::from(root);
        Self {
            generations: root.join("generations"),
            current: root.join("CURRENT"),
            root,
        }
    }

    pub fn create(&self) -> Result<(), SearchError> {
        std::fs::create_dir_all(&self.generations).map_err(|error| {
            SearchError::Backend(format!(
                "creating search generations {}: {error}",
                self.generations.display()
            ))
        })
    }

    pub fn sync_root(&self) -> Result<(), SearchError> {
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                SearchError::Backend(format!(
                    "syncing search directory {}: {error}",
                    self.root.display()
                ))
            })
    }

    pub fn sync_generations(&self) -> Result<(), SearchError> {
        File::open(&self.generations)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                SearchError::Backend(format!(
                    "syncing search generations {}: {error}",
                    self.generations.display()
                ))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: u32,
    pub generation: String,
    pub projection_epoch: String,
    pub search_schema: i64,
}

impl Manifest {
    pub(crate) fn generation_path(&self, layout: &SidecarLayout) -> Result<PathBuf, SearchError> {
        let path = Path::new(&self.generation);
        let mut components = path.components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(SearchError::Backend(
                "search CURRENT contains an unsafe generation path".into(),
            ));
        }
        Ok(layout.generations.join(path))
    }
}

pub(crate) fn read_manifest(layout: &SidecarLayout) -> Result<Option<Manifest>, SearchError> {
    let bytes = match std::fs::read(&layout.current) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SearchError::Backend(format!(
                "reading search CURRENT {}: {error}",
                layout.current.display()
            )));
        }
    };
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|error| SearchError::Backend(format!("parsing search CURRENT: {error}")))?;
    if manifest.format != 1 || manifest.search_schema != SEARCH_SCHEMA_VERSION {
        return Err(SearchError::Backend(format!(
            "unsupported search manifest format/schema {}/{}",
            manifest.format, manifest.search_schema
        )));
    }
    Ok(Some(manifest))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationState {
    pub projection_epoch: String,
    pub revision: u64,
}

pub(crate) fn generation_state(conn: &Connection) -> Result<GenerationState, SearchError> {
    let (schema, projection_epoch, revision): (i64, String, i64) = conn
        .query_row(
            "SELECT search_schema, projection_epoch, revision
             FROM search_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| {
            SearchError::Backend(format!("reading search generation state: {error}"))
        })?;
    if schema != SEARCH_SCHEMA_VERSION {
        return Err(SearchError::Backend(format!(
            "search generation schema {schema} does not match {SEARCH_SCHEMA_VERSION}"
        )));
    }
    let revision = u64::try_from(revision)
        .map_err(|_| SearchError::Backend("search generation has a negative revision".into()))?;
    Ok(GenerationState {
        projection_epoch,
        revision,
    })
}

pub(crate) fn open_generation(
    layout: &SidecarLayout,
    manifest: &Manifest,
    read_only: bool,
) -> Result<Connection, SearchError> {
    let path = manifest.generation_path(layout)?;
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = Connection::open_with_flags(&path, flags).map_err(|error| {
        SearchError::Backend(format!(
            "opening search generation {}: {error}",
            path.display()
        ))
    })?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| SearchError::Backend(format!("configuring search generation: {error}")))?;
    Ok(conn)
}

fn canonical_state(canonical: &Path) -> Result<ProjectionState, SearchError> {
    let conn = Connection::open_with_flags(canonical, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| SearchError::Backend(format!("opening canonical projection: {error}")))?;
    queries::projection_state(&conn).map_err(|error| {
        SearchError::Backend(format!("reading canonical projection state: {error}"))
    })
}

fn compare_state(canonical: &ProjectionState, generation: Option<&GenerationState>) -> SearchState {
    if !canonical.canonical_ready {
        return SearchState {
            readiness: SearchReadiness::Building,
            projection_epoch: Some(canonical.projection_epoch.clone()),
            revision: generation.map(|state| state.revision),
        };
    }
    let Some(generation) = generation else {
        return SearchState {
            readiness: SearchReadiness::Building,
            projection_epoch: Some(canonical.projection_epoch.clone()),
            revision: None,
        };
    };
    let readiness = if generation.projection_epoch != canonical.projection_epoch
        || generation.revision > canonical.search_revision
    {
        SearchReadiness::Stale
    } else if generation.revision < canonical.search_revision {
        SearchReadiness::CatchingUp
    } else {
        SearchReadiness::Ready
    };
    SearchState {
        readiness,
        projection_epoch: Some(generation.projection_epoch.clone()),
        revision: Some(generation.revision),
    }
}

/// Production FTS5 backend over the detached generation selected by CURRENT.
#[derive(Debug, Clone)]
pub struct Fts5SidecarSearch {
    canonical: PathBuf,
    pub(crate) layout: SidecarLayout,
    worker_error: Arc<Mutex<Option<String>>>,
}

impl Fts5SidecarSearch {
    pub fn new(canonical: impl Into<PathBuf>) -> Self {
        let canonical = canonical.into();
        Self {
            layout: SidecarLayout::new(&canonical),
            canonical,
            worker_error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }

    /// Resolve the immutable generation currently selected by `CURRENT`.
    /// Benchmark and diagnostic consumers use this to fingerprint the exact
    /// disposable artifact that answered their queries.
    pub fn current_generation_path(&self) -> Result<Option<PathBuf>, SearchError> {
        read_manifest(&self.layout)?
            .map(|manifest| manifest.generation_path(&self.layout))
            .transpose()
    }

    pub(crate) fn set_worker_error(&self, error: Option<String>) {
        *self
            .worker_error
            .lock()
            .expect("search worker state poisoned") = error;
    }

    fn observed_state(&self) -> Result<(SearchState, Option<Manifest>), SearchError> {
        let canonical = canonical_state(&self.canonical)?;
        let manifest = read_manifest(&self.layout)?;
        let generation = match &manifest {
            Some(manifest) => {
                let conn = open_generation(&self.layout, manifest, true)?;
                Some(generation_state(&conn)?)
            }
            None => None,
        };
        let mut state = compare_state(&canonical, generation.as_ref());
        if state.readiness != SearchReadiness::Ready
            && self
                .worker_error
                .lock()
                .expect("search worker state poisoned")
                .is_some()
        {
            state.readiness = SearchReadiness::Failed;
        }
        Ok((state, manifest))
    }
}

fn query_generation(
    conn: &Connection,
    query: &SearchQuery,
) -> Result<Vec<queries::SearchHit>, SearchError> {
    let filter = SearchFilter {
        session: query.session.clone(),
        runtime: query.runtime.clone(),
        agent: query.agent.clone(),
        project: query.project.clone(),
        kind: query.kind.clone(),
        is_error: query.is_error,
        after_ts: query.after_ts,
        before_ts: query.before_ts,
    };
    let mut sql = String::from(
        "SELECT d.timeline_id, d.session_id,
                snippet(message_fts, -1, '[', ']', '…', 12), rank
         FROM message_fts
         JOIN search_document d ON d.id = message_fts.rowid
         WHERE message_fts MATCH ?1",
    );
    let mut binds: Vec<Value> = vec![Value::from(query.text.fts5_expression())];
    let clause = |sql: &mut String, predicate: &str, value: Value, binds: &mut Vec<Value>| {
        binds.push(value);
        sql.push_str(&format!(" AND {predicate} ?{}", binds.len()));
    };
    if let Some(session) = &filter.session {
        clause(
            &mut sql,
            "d.session_id =",
            Value::from(session.clone()),
            &mut binds,
        );
    }
    if let Some(runtime) = &filter.runtime {
        clause(
            &mut sql,
            "d.runtime_id =",
            Value::from(runtime.clone()),
            &mut binds,
        );
    }
    if let Some(agent) = &filter.agent {
        clause(
            &mut sql,
            "d.agent =",
            Value::from(agent.clone()),
            &mut binds,
        );
    }
    if let Some(project) = &filter.project {
        clause(
            &mut sql,
            "d.project_id =",
            Value::from(project.clone()),
            &mut binds,
        );
    }
    if let Some(kind) = &filter.kind {
        clause(&mut sql, "d.kind =", Value::from(kind.clone()), &mut binds);
    }
    if let Some(is_error) = filter.is_error {
        clause(
            &mut sql,
            "d.is_error =",
            Value::from(i64::from(is_error)),
            &mut binds,
        );
    }
    if let Some(after) = filter.after_ts {
        clause(&mut sql, "d.ts >=", Value::from(after), &mut binds);
    }
    if let Some(before) = filter.before_ts {
        clause(&mut sql, "d.ts <=", Value::from(before), &mut binds);
    }
    binds.push(Value::from(query.limit));
    sql.push_str(&format!(
        " ORDER BY rank, d.timeline_id COLLATE BINARY LIMIT ?{}",
        binds.len()
    ));
    let mut statement = conn
        .prepare(&sql)
        .map_err(|error| SearchError::Backend(format!("preparing sidecar search: {error}")))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(binds), |row| {
            Ok(queries::SearchHit {
                timeline_id: row.get(0)?,
                session_id: row.get(1)?,
                snippet: row.get(2)?,
                score: row.get(3)?,
            })
        })
        .map_err(|error| SearchError::Backend(format!("querying sidecar search: {error}")))?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(
            row.map_err(|error| SearchError::Backend(format!("reading sidecar hit: {error}")))?,
        );
    }
    Ok(hits)
}

impl SearchBackend for Fts5SidecarSearch {
    fn state(&self) -> SearchState {
        self.observed_state()
            .map(|(state, _)| state)
            .unwrap_or_else(|_| SearchState::failed())
    }

    fn search(&self, query: SearchQuery) -> Result<SearchOutput, SearchError> {
        query.validate()?;
        let before = canonical_state(&self.canonical)?;
        let Some(manifest) = read_manifest(&self.layout)? else {
            return Ok(SearchOutput::unavailable(compare_state(&before, None)));
        };
        let conn = open_generation(&self.layout, &manifest, true)?;
        conn.execute_batch("BEGIN")
            .map_err(|error| SearchError::Backend(format!("opening search snapshot: {error}")))?;
        let generation = generation_state(&conn)?;
        let state = compare_state(&before, Some(&generation));
        if state.readiness != SearchReadiness::Ready {
            return Ok(SearchOutput::unavailable(state));
        }
        let hits = query_generation(&conn, &query)?;
        let after = canonical_state(&self.canonical)?;
        if after.projection_epoch != before.projection_epoch
            || after.search_revision != before.search_revision
            || !after.canonical_ready
        {
            return Ok(SearchOutput::unavailable(compare_state(
                &after,
                Some(&generation),
            )));
        }
        Ok(SearchOutput { state, hits })
    }
}
