//! Backend-neutral search boundary.
//!
//! Canonical rows and reconciliation remain in SQLite. This crate owns the
//! query/readiness contract and the synchronous FTS5 reference backend so the
//! server does not depend on an engine-specific query path. A disposable
//! sidecar can implement [`SearchBackend`] without changing the L3 protocol.

use std::fmt;
use std::path::{Path, PathBuf};

use rosetta_core::rows::Ms;
use rosetta_store::queries::{self, SearchFilter};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

pub use rosetta_store::queries::SearchHit;

/// Search readiness is separate from canonical projection readiness. A
/// non-ready state must be returned explicitly; it must never masquerade as a
/// successful empty search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum SearchReadiness {
    Ready,
    Building,
    CatchingUp,
    Stale,
    Failed,
}

/// Identity and readiness of the search projection that answered a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SearchState {
    pub readiness: SearchReadiness,
    /// Owning canonical projection identifier for a detached search index.
    /// Synchronous FTS5 has no independent projection identity.
    pub projection_epoch: Option<String>,
    /// Canonical search revision committed by a detached index. Synchronous
    /// FTS5 commits with the canonical rows and therefore has no revision.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
}

impl SearchState {
    pub fn synchronous() -> Self {
        Self {
            readiness: SearchReadiness::Ready,
            projection_epoch: None,
            revision: None,
        }
    }

    pub fn failed() -> Self {
        Self {
            readiness: SearchReadiness::Failed,
            projection_epoch: None,
            revision: None,
        }
    }
}

/// Backend-neutral structured query. Engine-specific handles and connection
/// types are deliberately absent from this boundary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchQuery {
    pub text: String,
    pub limit: i64,
    pub session: Option<String>,
    pub runtime: Option<String>,
    pub project: Option<String>,
    pub kind: Option<String>,
    pub after_ts: Option<Ms>,
    pub before_ts: Option<Ms>,
}

/// One atomic backend answer. The state and hits must describe the same search
/// snapshot, which avoids a readiness check racing the actual query.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutput {
    pub state: SearchState,
    pub hits: Vec<SearchHit>,
}

impl SearchOutput {
    /// An explicit non-ready answer. This is intentionally distinct from an
    /// empty result produced by a ready index.
    pub fn unavailable(state: SearchState) -> Self {
        debug_assert_ne!(state.readiness, SearchReadiness::Ready);
        Self {
            state,
            hits: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    InvalidQuery(String),
    Backend(String),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQuery(message) => write!(f, "invalid search query: {message}"),
            Self::Backend(message) => write!(f, "search backend: {message}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Synchronous interface because search engines and SQLite are blocking. The
/// async server runs calls on its blocking pool.
pub trait SearchBackend: Send + Sync {
    /// Current status for `/api/status`. A search response uses the state
    /// returned inside [`SearchOutput`] so state and results stay coherent.
    fn state(&self) -> SearchState;

    fn search(&self, query: SearchQuery) -> Result<SearchOutput, SearchError>;
}

/// Shipped reference backend: external-content FTS5 in the canonical SQLite
/// projection. It remains synchronous and ready whenever that projection is
/// admitted by the server.
#[derive(Debug, Clone)]
pub struct Fts5Search {
    db: PathBuf,
}

impl Fts5Search {
    pub fn new(db: impl Into<PathBuf>) -> Self {
        Self { db: db.into() }
    }

    pub fn path(&self) -> &Path {
        &self.db
    }
}

impl SearchBackend for Fts5Search {
    fn state(&self) -> SearchState {
        SearchState::synchronous()
    }

    fn search(&self, query: SearchQuery) -> Result<SearchOutput, SearchError> {
        let conn = Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| SearchError::Backend(format!("opening projection: {error}")))?;
        let filter = SearchFilter {
            session: query.session,
            runtime: query.runtime,
            project: query.project,
            kind: query.kind,
            after_ts: query.after_ts,
            before_ts: query.before_ts,
        };
        // Preserve the reference endpoint's existing contract: MATCH parser
        // failures are invalid user queries. Opening/dispatch failures remain
        // backend errors above rather than being mislabeled as HTTP 400.
        let hits = queries::search_filtered(&conn, &query.text, query.limit, &filter)
            .map_err(|error| SearchError::InvalidQuery(error.to_string()))?;
        Ok(SearchOutput {
            state: SearchState::synchronous(),
            hits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_uses_the_wire_vocabulary() {
        assert_eq!(
            serde_json::to_string(&SearchReadiness::CatchingUp).unwrap(),
            "\"catching-up\""
        );
        assert_eq!(
            serde_json::to_string(&SearchReadiness::Ready).unwrap(),
            "\"ready\""
        );
    }

    #[test]
    fn missing_projection_is_a_backend_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let backend = Fts5Search::new(dir.path().join("missing.sqlite"));
        let error = backend
            .search(SearchQuery {
                text: "hello".into(),
                limit: 10,
                ..SearchQuery::default()
            })
            .unwrap_err();
        assert!(matches!(error, SearchError::Backend(_)));
    }

    #[test]
    fn unavailable_answers_are_explicitly_non_ready() {
        let output = SearchOutput::unavailable(SearchState {
            readiness: SearchReadiness::Building,
            projection_epoch: Some("epoch-1".into()),
            revision: Some(0),
        });
        assert_eq!(output.state.readiness, SearchReadiness::Building);
        assert!(output.hits.is_empty());
    }
}
