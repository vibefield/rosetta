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

pub const MAX_SEARCH_TEXT_BYTES: usize = 4_096;
pub const MAX_SEARCH_CLAUSES: usize = 64;

/// One backend-neutral text clause. Every clause is required; adjacent terms
/// are an implicit AND and quoted phrases require adjacency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextClause {
    Term(String),
    Phrase(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextQueryError(String);

impl fmt::Display for TextQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TextQueryError {}

/// Parsed product query language shared by every search backend.
///
/// The grammar intentionally has no engine operators: Unicode whitespace
/// separates required terms, double quotes delimit a required phrase, and
/// `\"`/`\\` escape those two characters inside a phrase. Words such as
/// `OR`, `NEAR`, and field selectors are ordinary text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextQuery {
    clauses: Vec<TextClause>,
}

impl TextQuery {
    pub fn parse(input: &str) -> Result<Self, TextQueryError> {
        if input.len() > MAX_SEARCH_TEXT_BYTES {
            return Err(TextQueryError(format!(
                "q exceeds {MAX_SEARCH_TEXT_BYTES} bytes"
            )));
        }
        if input
            .chars()
            .any(|ch| ch.is_control() && !ch.is_whitespace())
        {
            return Err(TextQueryError(
                "q contains an unsupported control character".into(),
            ));
        }

        let mut chars = input.chars().peekable();
        let mut clauses = Vec::new();
        while chars.peek().is_some() {
            while chars.next_if(|ch| ch.is_whitespace()).is_some() {}
            let Some(first) = chars.next() else {
                break;
            };

            let clause = if first == '"' {
                let mut phrase = String::new();
                let mut closed = false;
                while let Some(ch) = chars.next() {
                    match ch {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => match chars.next() {
                            Some('"') => phrase.push('"'),
                            Some('\\') => phrase.push('\\'),
                            Some(other) => {
                                return Err(TextQueryError(format!(
                                    "unsupported phrase escape `\\{other}`"
                                )));
                            }
                            None => {
                                return Err(TextQueryError("unterminated phrase escape".into()));
                            }
                        },
                        other => phrase.push(other),
                    }
                }
                if !closed {
                    return Err(TextQueryError("unterminated phrase".into()));
                }
                if chars.peek().is_some_and(|ch| !ch.is_whitespace()) {
                    return Err(TextQueryError(
                        "quoted phrases must be separated by whitespace".into(),
                    ));
                }
                if phrase.trim().is_empty() {
                    return Err(TextQueryError("quoted phrases must not be empty".into()));
                }
                TextClause::Phrase(phrase)
            } else {
                let mut term = String::from(first);
                while let Some(ch) = chars.next_if(|ch| !ch.is_whitespace()) {
                    if ch == '"' {
                        return Err(TextQueryError(
                            "quotes must begin a whitespace-delimited clause".into(),
                        ));
                    }
                    term.push(ch);
                }
                TextClause::Term(term)
            };

            clauses.push(clause);
            if clauses.len() > MAX_SEARCH_CLAUSES {
                return Err(TextQueryError(format!(
                    "q exceeds {MAX_SEARCH_CLAUSES} clauses"
                )));
            }
        }

        if clauses.is_empty() {
            return Err(TextQueryError("q must not be empty".into()));
        }
        Ok(Self { clauses })
    }

    pub fn clauses(&self) -> &[TextClause] {
        &self.clauses
    }

    /// Compile the common grammar to a trusted FTS5 MATCH expression. This
    /// method lives at the backend boundary so engine syntax never crosses
    /// the transport contract.
    pub fn fts5_expression(&self) -> String {
        self.clauses
            .iter()
            .map(|clause| match clause {
                TextClause::Term(text) | TextClause::Phrase(text) => {
                    format!("\"{}\"", text.replace('"', "\"\""))
                }
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

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
#[derive(Debug, Clone, PartialEq)]
pub struct SearchQuery {
    pub text: TextQuery,
    pub limit: i64,
    pub session: Option<String>,
    pub runtime: Option<String>,
    pub project: Option<String>,
    pub kind: Option<String>,
    pub is_error: Option<bool>,
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
            is_error: query.is_error,
            after_ts: query.after_ts,
            before_ts: query.before_ts,
        };
        let expression = query.text.fts5_expression();
        let hits = queries::search_filtered(&conn, &expression, query.limit, &filter)
            .map_err(|error| SearchError::Backend(error.to_string()))?;
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
                text: TextQuery::parse("hello").unwrap(),
                limit: 10,
                session: None,
                runtime: None,
                project: None,
                kind: None,
                is_error: None,
                after_ts: None,
                before_ts: None,
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

    #[test]
    fn common_text_grammar_quotes_engine_operators_and_preserves_phrases() {
        let query = TextQuery::parse(r#"error OR "cargo test" near:content"#).unwrap();
        assert_eq!(
            query.clauses(),
            [
                TextClause::Term("error".into()),
                TextClause::Term("OR".into()),
                TextClause::Phrase("cargo test".into()),
                TextClause::Term("near:content".into()),
            ]
        );
        assert_eq!(
            query.fts5_expression(),
            r#""error" AND "OR" AND "cargo test" AND "near:content""#
        );

        let escaped = TextQuery::parse(r#""say \"hello\" in C:\\tmp""#).unwrap();
        assert_eq!(
            escaped.clauses(),
            [TextClause::Phrase("say \"hello\" in C:\\tmp".into())]
        );
        assert_eq!(escaped.fts5_expression(), r#""say ""hello"" in C:\tmp""#);
    }

    #[test]
    fn common_text_grammar_rejects_ambiguous_or_unbounded_input() {
        for input in [r#""unterminated"#, r#"word"phrase"#, r#"""#, r#""\q""#] {
            assert!(TextQuery::parse(input).is_err());
        }
        let too_many = std::iter::repeat_n("word", MAX_SEARCH_CLAUSES + 1)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(TextQuery::parse(&too_many).is_err());
        assert!(TextQuery::parse(&"x".repeat(MAX_SEARCH_TEXT_BYTES + 1)).is_err());
        assert!(TextQuery::parse("word\0word").is_err());
    }
}
