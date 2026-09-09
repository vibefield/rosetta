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
mod sidecar;
mod worker;

pub use sidecar::Fts5SidecarSearch;
pub use worker::{Fts5SearchWorker, SearchAck, synchronize_fts5_once};
mod progress;
pub use progress::{SearchBuildProgress, SearchBuildStage};

pub const MAX_SEARCH_TEXT_BYTES: usize = 4_096;
pub const MAX_SEARCH_CLAUSES: usize = 64;
pub const MAX_SEARCH_FILTER_BYTES: usize = 1_024;
pub const MAX_SEARCH_RESULTS: i64 = 1_000;
/// Upper bound on the session ids one request may name. A monitor of live
/// agents names each session it can jump to; anything larger is a project or
/// runtime filter in disguise.
pub const MAX_SEARCH_SESSIONS: usize = 256;
/// Upper bound on the scope after the server expands each named root to its
/// subagent tree. Well under SQLite's bound-parameter limit.
pub const MAX_SEARCH_SESSION_SCOPE: usize = 4_096;
/// Shortest term that prefix mode widens. One or two characters would match
/// most of the vocabulary and turn a keystroke into a scan of the index.
pub const MIN_PREFIX_TERM_CHARS: usize = 3;

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
        self.fts5_expression_with(false)
    }

    /// The same compilation with prefix widening: a term of at least
    /// [`MIN_PREFIX_TERM_CHARS`] characters matches every token it begins, so
    /// `puppet` finds `puppeteer`. Phrases and shorter terms stay exact. The
    /// `*` sits outside the quoted token, the one place FTS5 reads it as an
    /// operator, so user text still never reaches the engine unquoted.
    pub fn fts5_expression_with(&self, prefix: bool) -> String {
        self.clauses
            .iter()
            .map(|clause| match clause {
                TextClause::Term(text) => {
                    let quoted = format!("\"{}\"", text.replace('"', "\"\""));
                    if prefix && text.chars().count() >= MIN_PREFIX_TERM_CHARS {
                        format!("{quoted}*")
                    } else {
                        quoted
                    }
                }
                TextClause::Phrase(text) => format!("\"{}\"", text.replace('"', "\"\"")),
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
    /// Live work in an unpublished generation; absent for ready, failed, or
    /// synchronous search. It is never persisted in the canonical projection.
    #[serde(default)]
    pub progress: Option<SearchBuildProgress>,
    /// Owning canonical projection identifier for a detached search index.
    /// Synchronous FTS5 has no independent projection identity.
    pub projection_epoch: Option<String>,
    /// Canonical search revision committed by a detached index. Synchronous
    /// FTS5 commits with the canonical rows and therefore has no revision.
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub revision: Option<u64>,
}

impl SearchState {
    pub fn building() -> Self {
        Self {
            readiness: SearchReadiness::Building,
            progress: None,
            projection_epoch: None,
            revision: None,
        }
    }

    pub fn synchronous() -> Self {
        Self {
            readiness: SearchReadiness::Ready,
            progress: None,
            projection_epoch: None,
            revision: None,
        }
    }

    pub fn failed() -> Self {
        Self {
            readiness: SearchReadiness::Failed,
            progress: None,
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
    /// Widen each term to the tokens it begins; see
    /// [`TextQuery::fts5_expression_with`].
    pub prefix: bool,
    pub limit: i64,
    pub session: Option<String>,
    /// An explicit set of canonical session ids matched as one query. The
    /// server expands each named root to its subagent tree before the backend
    /// sees the scope; the backend matches exactly what it is given.
    pub sessions: Vec<String>,
    pub runtime: Option<String>,
    pub agent: Option<String>,
    pub project: Option<String>,
    pub kind: Option<String>,
    pub is_error: Option<bool>,
    pub after_ts: Option<Ms>,
    pub before_ts: Option<Ms>,
}

impl SearchQuery {
    pub fn validate(&self) -> Result<(), SearchError> {
        if !(1..=MAX_SEARCH_RESULTS).contains(&self.limit) {
            return Err(SearchError::InvalidQuery(format!(
                "limit must be in 1..={MAX_SEARCH_RESULTS}"
            )));
        }
        for (name, value) in [
            ("session", self.session.as_deref()),
            ("runtime", self.runtime.as_deref()),
            ("agent", self.agent.as_deref()),
            ("project", self.project.as_deref()),
        ] {
            if let Some(value) = value {
                if value.is_empty() || value.trim() != value {
                    return Err(SearchError::InvalidQuery(format!(
                        "{name} must be a non-blank canonical value"
                    )));
                }
                if value.len() > MAX_SEARCH_FILTER_BYTES {
                    return Err(SearchError::InvalidQuery(format!(
                        "{name} exceeds {MAX_SEARCH_FILTER_BYTES} bytes"
                    )));
                }
            }
        }
        if self.sessions.len() > MAX_SEARCH_SESSION_SCOPE {
            return Err(SearchError::InvalidQuery(format!(
                "sessions exceeds {MAX_SEARCH_SESSION_SCOPE} ids"
            )));
        }
        if self.session.is_some() && !self.sessions.is_empty() {
            return Err(SearchError::InvalidQuery(
                "session and sessions are alternatives; send one of them".into(),
            ));
        }
        for value in &self.sessions {
            if value.is_empty() || value.trim() != value || value.contains(',') {
                return Err(SearchError::InvalidQuery(
                    "sessions must be non-blank canonical values".into(),
                ));
            }
            if value.len() > MAX_SEARCH_FILTER_BYTES {
                return Err(SearchError::InvalidQuery(format!(
                    "a sessions entry exceeds {MAX_SEARCH_FILTER_BYTES} bytes"
                )));
            }
        }
        if let Some(kind) = &self.kind
            && !queries::SEARCH_KINDS.contains(&kind.as_str())
        {
            return Err(SearchError::InvalidQuery(format!(
                "unsupported message kind `{kind}`: search indexes only prompts and replies ({})",
                queries::SEARCH_KINDS.join(", ")
            )));
        }
        if self.is_error.is_some() {
            return Err(SearchError::InvalidQuery(
                "is_error selects tool activity, which search does not index".into(),
            ));
        }
        if let (Some(after), Some(before)) = (self.after_ts, self.before_ts)
            && after > before
        {
            return Err(SearchError::InvalidQuery(
                "after_ts must not exceed before_ts".into(),
            ));
        }
        Ok(())
    }
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
        query.validate()?;
        let conn = Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| SearchError::Backend(format!("opening projection: {error}")))?;
        let filter = SearchFilter {
            session: query.session,
            sessions: query.sessions,
            runtime: query.runtime,
            agent: query.agent,
            project: query.project,
            kind: query.kind,
            is_error: query.is_error,
            after_ts: query.after_ts,
            before_ts: query.before_ts,
        };
        let expression = query.text.fts5_expression_with(query.prefix);
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
                prefix: false,
                limit: 10,
                session: None,
                sessions: Vec::new(),
                runtime: None,
                agent: None,
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
            progress: None,
            projection_epoch: Some("epoch-1".into()),
            revision: Some(0),
        });
        assert_eq!(output.state.readiness, SearchReadiness::Building);
        assert!(output.hits.is_empty());
    }

    #[test]
    fn prefix_mode_widens_terms_but_never_phrases_or_short_words() {
        let query = TextQuery::parse(r#"puppet "cargo test" ab"#).unwrap();
        assert_eq!(
            query.fts5_expression_with(true),
            r#""puppet"* AND "cargo test" AND "ab""#
        );
        assert_eq!(query.fts5_expression_with(false), query.fts5_expression());
    }

    #[test]
    fn session_scope_validation_bounds_the_set_and_refuses_ambiguity() {
        let mut both = query();
        both.session = Some("s1".into());
        both.sessions = vec!["s2".into()];
        assert!(both.validate().is_err());

        let mut blank = query();
        blank.sessions = vec![" s1".into()];
        assert!(blank.validate().is_err());

        let mut too_many = query();
        too_many.sessions = (0..=MAX_SEARCH_SESSION_SCOPE)
            .map(|i| format!("s{i}"))
            .collect();
        assert!(too_many.validate().is_err());

        let mut valid = query();
        valid.sessions = vec!["s1".into(), "s2".into()];
        assert!(valid.validate().is_ok());
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

    fn query() -> SearchQuery {
        SearchQuery {
            text: TextQuery::parse("hello").unwrap(),
            prefix: false,
            limit: 10,
            session: None,
            sessions: Vec::new(),
            runtime: None,
            agent: None,
            project: None,
            kind: None,
            is_error: None,
            after_ts: None,
            before_ts: None,
        }
    }

    #[test]
    fn structured_query_validation_rejects_false_empty_results() {
        let mut invalid = query();
        invalid.kind = Some("tool".into());
        assert!(matches!(
            invalid.validate(),
            Err(SearchError::InvalidQuery(_))
        ));

        // Coverage is prompts and replies: a kind that is never indexed and a
        // tool-status selector are errors, never successful empty searches.
        for kind in ["tool_use", "thinking", "summary", "collaboration", "notice"] {
            let mut invalid = query();
            invalid.kind = Some(kind.into());
            assert!(invalid.validate().is_err(), "{kind} must be rejected");
        }
        for kind in queries::SEARCH_KINDS {
            let mut valid = query();
            valid.kind = Some(kind.into());
            assert!(valid.validate().is_ok(), "{kind} must be accepted");
        }
        let mut invalid = query();
        invalid.is_error = Some(false);
        assert!(invalid.validate().is_err());

        let mut invalid = query();
        invalid.after_ts = Some(2);
        invalid.before_ts = Some(1);
        assert!(invalid.validate().is_err());

        let mut invalid = query();
        invalid.agent = Some(" codex".into());
        assert!(invalid.validate().is_err());

        let mut invalid = query();
        invalid.limit = 0;
        assert!(invalid.validate().is_err());
    }
}
