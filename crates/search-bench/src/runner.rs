use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use rosetta_search::{Fts5SidecarSearch, SearchBackend, SearchQuery, SearchReadiness, TextQuery};
use rosetta_store::queries::SearchHit;
use rusqlite::{Connection, OpenFlags};

use crate::RUN_FORMAT;
use crate::fingerprint::{fingerprint_file, verify_campaign};
use crate::model::{
    CampaignManifest, CaseReport, ClassReport, DatabaseFingerprint, LaneReport, LatencySummary,
    ProjectionCounts, QueryCase, RunConfig, RunReport,
};
use crate::stats::summarize;

#[derive(Debug, Clone, Copy)]
enum Lane {
    SharedBackend,
    FreshBackend,
}

impl Lane {
    const ALL: [Self; 2] = [Self::SharedBackend, Self::FreshBackend];

    fn name(self) -> &'static str {
        match self {
            Self::SharedBackend => "shared-backend",
            Self::FreshBackend => "fresh-backend",
        }
    }
}

fn open_read(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("opening {} read-only: {error}", path.display()))?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|error| format!("configuring read-only query connection: {error}"))?;
    Ok(connection)
}

fn nonnegative_count(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{label} count is negative: {value}"))
}

fn validate_projection(
    canonical: &Connection,
    generation: &Connection,
) -> Result<(i64, String, ProjectionCounts), String> {
    let schema_version: i64 = canonical
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("reading projection schema: {error}"))?;
    if schema_version != rosetta_store::schema::SCHEMA_VERSION {
        return Err(format!(
            "projection schema {schema_version} does not match {}",
            rosetta_store::schema::SCHEMA_VERSION
        ));
    }
    let quick: String = canonical
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| format!("running quick_check: {error}"))?;
    if quick != "ok" {
        return Err(format!("SQLite quick_check returned {quick}"));
    }
    let mut foreign = canonical
        .prepare("PRAGMA foreign_key_check")
        .map_err(|error| format!("preparing foreign_key_check: {error}"))?;
    if foreign
        .query([])
        .and_then(|mut rows| rows.next().map(|row| row.is_some()))
        .map_err(|error| format!("running foreign_key_check: {error}"))?
    {
        return Err("foreign_key_check reported a violation".into());
    }
    let (messages, indexed_sessions): (i64, i64) = canonical
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM message),
               (SELECT COUNT(*) FROM session WHERE stream_state = 'indexed')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| format!("checking projection cardinalities: {error}"))?;
    let search_quick: String = generation
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| format!("running detached search quick_check: {error}"))?;
    if search_quick != "ok" {
        return Err(format!(
            "detached search SQLite quick_check returned {search_quick}"
        ));
    }
    let (documents, fts): (i64, i64) = generation
        .query_row(
            "SELECT (SELECT COUNT(*) FROM search_document),
                    (SELECT COUNT(*) FROM message_fts)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| format!("checking detached search cardinalities: {error}"))?;
    let searchable = rosetta_store::queries::search_document_count(canonical)
        .map_err(|error| format!("counting canonical search documents: {error}"))?;
    if nonnegative_count(documents, "search document")? != searchable || documents != fts {
        return Err(format!(
            "canonical/search cardinality differs: {searchable} searchable messages, {documents} documents, {fts} FTS rows"
        ));
    }
    let sqlite_version: String = canonical
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .map_err(|error| format!("reading SQLite version: {error}"))?;
    Ok((
        schema_version,
        sqlite_version,
        ProjectionCounts {
            messages: nonnegative_count(messages, "message")?,
            fts_rows: nonnegative_count(fts, "FTS")?,
            indexed_sessions: nonnegative_count(indexed_sessions, "indexed session")?,
            source_ops: 0,
            pending_merges: 0,
        },
    ))
}

fn product_query(case: &QueryCase) -> Result<SearchQuery, String> {
    Ok(SearchQuery {
        text: TextQuery::parse(&case.query)
            .map_err(|error| format!("query case `{}` failed: {error}", case.label))?,
        prefix: false,
        limit: case.limit,
        session: case.filter.session.clone(),
        sessions: Vec::new(),
        runtime: case.filter.runtime.clone(),
        agent: case.filter.agent.clone(),
        project: case.filter.project.clone(),
        kind: case.filter.kind.clone(),
        is_error: case.filter.is_error,
        after_ts: case.filter.after_ts,
        before_ts: case.filter.before_ts,
    })
}

fn result_fingerprint(hits: &[SearchHit]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rosetta-search-results-v1\0");
    hasher.update(&(hits.len() as u64).to_be_bytes());
    for hit in hits {
        hasher.update(&(hit.timeline_id.len() as u64).to_be_bytes());
        hasher.update(hit.timeline_id.as_bytes());
        hasher.update(&(hit.session_id.len() as u64).to_be_bytes());
        hasher.update(hit.session_id.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn schedule(cases: &[QueryCase]) -> Vec<usize> {
    let mut schedule = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        schedule.extend(std::iter::repeat_n(index, case.weight as usize));
    }
    schedule
}

fn run_one(
    lane: Lane,
    db: &Path,
    shared: Option<&Fts5SidecarSearch>,
    case: &QueryCase,
) -> Result<(u64, u64, String), String> {
    let started = Instant::now();
    let query = product_query(case)?;
    let output = match lane {
        Lane::SharedBackend => shared.expect("shared lane has a backend").search(query),
        Lane::FreshBackend => Fts5SidecarSearch::new(db).search(query),
    }
    .map_err(|error| format!("query case `{}` failed: {error}", case.label))?;
    if output.state.readiness != SearchReadiness::Ready {
        return Err(format!(
            "query case `{}` saw detached search state {:?}",
            case.label, output.state.readiness
        ));
    }
    let hits = output.hits;
    let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Ok((hits.len() as u64, nanos, result_fingerprint(&hits)))
}

fn execute_lane(
    lane: Lane,
    db: &Path,
    cases: &[QueryCase],
    warmup_cycles: u32,
    measured_cycles: u32,
) -> Result<LaneReport, String> {
    let shared = match lane {
        Lane::SharedBackend => Some(Fts5SidecarSearch::new(db)),
        Lane::FreshBackend => None,
    };
    let schedule = schedule(cases);
    let mut expected: Vec<Option<(u64, String)>> = vec![None; cases.len()];

    let observe = |case_index: usize,
                   sample: (u64, u64, String),
                   expected: &mut [Option<(u64, String)>]|
     -> Result<u64, String> {
        let (hits, nanos, digest) = sample;
        match &expected[case_index] {
            None => expected[case_index] = Some((hits, digest)),
            Some((expected_hits, expected_digest))
                if *expected_hits == hits && *expected_digest == digest => {}
            Some(_) => {
                return Err(format!(
                    "query case `{}` returned a different ranked result during one run",
                    cases[case_index].label
                ));
            }
        }
        Ok(nanos)
    };

    for cycle in 0..warmup_cycles as usize {
        for offset in 0..schedule.len() {
            let case_index = schedule[(offset + cycle) % schedule.len()];
            let sample = run_one(lane, db, shared.as_ref(), &cases[case_index])?;
            observe(case_index, sample, &mut expected)?;
        }
    }

    let mut case_samples: Vec<Vec<u64>> = vec![Vec::new(); cases.len()];
    let mut all_samples = Vec::new();
    for cycle in 0..measured_cycles as usize {
        for offset in 0..schedule.len() {
            let case_index = schedule[(offset + cycle) % schedule.len()];
            let sample = run_one(lane, db, shared.as_ref(), &cases[case_index])?;
            let nanos = observe(case_index, sample, &mut expected)?;
            case_samples[case_index].push(nanos);
            all_samples.push(nanos);
        }
    }

    let mut class_samples: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut reports = Vec::with_capacity(cases.len());
    for (index, case) in cases.iter().enumerate() {
        class_samples
            .entry(&case.class)
            .or_default()
            .extend_from_slice(&case_samples[index]);
        let (hits, result_blake3) = expected[index]
            .clone()
            .expect("every case appears in the schedule");
        let filter_json = serde_json::to_vec(&case.filter).map_err(|error| error.to_string())?;
        reports.push(CaseReport {
            label: case.label.clone(),
            class: case.class.clone(),
            query_blake3: blake3::hash(case.query.as_bytes()).to_hex().to_string(),
            filter_blake3: blake3::hash(&filter_json).to_hex().to_string(),
            weight: case.weight,
            limit: case.limit,
            hits,
            result_blake3,
            latency: summarize(&case_samples[index]),
        });
    }
    let classes = class_samples
        .into_iter()
        .map(|(class, samples)| ClassReport {
            class: class.to_string(),
            latency: summarize(&samples),
        })
        .collect();
    Ok(LaneReport {
        lane: lane.name().into(),
        latency: summarize(&all_samples),
        classes,
        cases: reports,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn run_campaign(
    manifest: &CampaignManifest,
    ledger_root: &Path,
    query_suite: &Path,
    db: &Path,
    implementation: &str,
    machine: &str,
    warmup_cycles: u32,
    measured_cycles: u32,
) -> Result<RunReport, String> {
    if implementation.trim().is_empty() || machine.trim().is_empty() {
        return Err("implementation and machine labels must not be empty".into());
    }
    if warmup_cycles == 0 {
        return Err("at least one warmup cycle is required".into());
    }
    if measured_cycles < 100 {
        return Err("at least 100 measured cycles are required for p99".into());
    }

    let suite = verify_campaign(manifest, ledger_root, query_suite)?;
    let database_artifact = fingerprint_file(db, true)?;
    let backend = Fts5SidecarSearch::new(db);
    let generation_path = backend
        .current_generation_path()
        .map_err(|error| format!("resolving detached search generation: {error}"))?
        .ok_or("detached search has no CURRENT generation")?;
    let search_artifact = fingerprint_file(&generation_path, true)?;
    let connection = open_read(db)?;
    let search_connection = open_read(&generation_path)?;
    let (schema_version, sqlite_version, counts) =
        validate_projection(&connection, &search_connection)?;
    if backend.state().readiness != SearchReadiness::Ready {
        return Err("detached search is not coherent with the canonical projection".into());
    }
    drop(search_connection);
    drop(connection);

    let mut lanes = Vec::new();
    for lane in Lane::ALL {
        lanes.push(execute_lane(
            lane,
            db,
            &suite.cases,
            warmup_cycles,
            measured_cycles,
        )?);
    }

    let database_after = fingerprint_file(db, true)?;
    if database_after != database_artifact {
        return Err("database changed during the read-only benchmark".into());
    }
    let generation_after = backend
        .current_generation_path()
        .map_err(|error| format!("rechecking detached search generation: {error}"))?
        .ok_or("detached search lost its CURRENT generation")?;
    if generation_after != generation_path
        || fingerprint_file(&generation_after, true)? != search_artifact
    {
        return Err("detached search generation changed during the benchmark".into());
    }
    let suite_after = verify_campaign(manifest, ledger_root, query_suite)?;
    if suite_after != suite {
        return Err("query suite changed during the benchmark".into());
    }

    Ok(RunReport {
        format: RUN_FORMAT,
        campaign_id: manifest.campaign_id.clone(),
        implementation: implementation.into(),
        machine: machine.into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        suite_name: suite.name,
        suite_basis: suite.basis,
        ledger: manifest.ledger.clone(),
        query_suite: manifest.query_suite.clone(),
        database: DatabaseFingerprint {
            bytes: database_artifact.bytes,
            blake3: database_artifact.blake3,
            schema_version,
            sqlite_version,
            counts,
        },
        search_generation: search_artifact,
        config: RunConfig {
            warmup_cycles,
            measured_cycles,
        },
        lanes,
    })
}

pub(crate) fn format_millis(nanos: u64) -> String {
    format!("{:.3}", nanos as f64 / 1_000_000.0)
}

pub fn summary_line(name: &str, summary: &LatencySummary) -> String {
    format!(
        "{name:<24} {:>8} samples  p50 {:>9} ms  p95 {:>9} ms  p99 {:>9} ms",
        summary.samples,
        format_millis(summary.p50_nanos),
        format_millis(summary.p95_nanos),
        format_millis(summary.p99_nanos),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use rosetta_core::IngestOp;
    use rosetta_core::rows::{
        MessageKind, MessageRow, RuntimeRow, SessionRole, SessionRow, StreamState,
    };
    use rosetta_search::{Fts5SidecarSearch, synchronize_fts5_once};
    use rosetta_store::{Batch, Store};

    use super::{Lane, run_one, schedule};
    use crate::model::{QueryCase, QueryFilterSpec};

    #[test]
    fn schedule_materializes_declared_weights() {
        let cases = [
            QueryCase {
                label: "a".into(),
                class: "x".into(),
                query: "alpha".into(),
                weight: 2,
                limit: 10,
                filter: QueryFilterSpec::default(),
            },
            QueryCase {
                label: "b".into(),
                class: "x".into(),
                query: "beta".into(),
                weight: 1,
                limit: 10,
                filter: QueryFilterSpec::default(),
            },
        ];
        assert_eq!(schedule(&cases), vec![0, 0, 1]);
    }

    #[test]
    fn benchmark_query_uses_the_detached_product_backend() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let mut store = Store::open(&db).unwrap();
        store
            .apply(&Batch {
                ops: vec![
                    IngestOp::Runtime(RuntimeRow {
                        id: "rt".into(),
                        kind: "codex".into(),
                        data_root: "/agent".into(),
                        enabled: true,
                        account: None,
                        config: None,
                        skills: None,
                        versions_seen: None,
                        discovered_at: 1,
                    }),
                    IngestOp::SessionMeta(SessionRow {
                        id: "s".into(),
                        native_session_id: "native".into(),
                        runtime_id: "rt".into(),
                        project_id: None,
                        cwd: None,
                        parent_session_id: None,
                        delegation: None,
                        root_session_id: "s".into(),
                        role: SessionRole::Main,
                        title: None,
                        summary: None,
                        started_at: 1,
                        updated_at: 2,
                        stream_state: StreamState::Indexed,
                    }),
                    IngestOp::project_message(MessageRow {
                        timeline_id: "m".into(),
                        session_id: "s".into(),
                        ordinal: 0,
                        kind: MessageKind::Agent,
                        subtype: None,
                        uuid: None,
                        parent_uuid: None,
                        ts: Some(1),
                        model: None,
                        effort: None,
                        content: Some("detached needle".into()),
                        tool: None,
                        attachments: Vec::new(),
                        citations: Vec::new(),
                        group: None,
                        response: None,
                        block_index: None,
                        branch_tool_id: None,
                        usage: None,
                        agent_version: None,
                        src_cursor: None,
                        src_offset: None,
                    })
                    .pop()
                    .expect("one canonical message"),
                ],
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        let mut thinking = rosetta_store::queries::timeline_page(store.conn(), "s", None, 10)
            .unwrap()
            .pop()
            .unwrap();
        thinking.timeline_id = "thinking".into();
        thinking.ordinal = 1;
        thinking.body = rosetta_core::TimelineBody::Thinking {
            text: rosetta_core::CanonicalText::inline("hidden needle").unwrap(),
        };
        store
            .apply(&Batch {
                ops: vec![IngestOp::Message(Box::new(thinking))],
                ..Batch::default()
            })
            .unwrap();
        let backend = Fts5SidecarSearch::new(&db);
        let ack = synchronize_fts5_once(&backend, &AtomicBool::new(false))
            .unwrap()
            .unwrap();
        assert!(
            store
                .acknowledge_search(&ack.projection_epoch, ack.revision)
                .unwrap()
        );
        let case = QueryCase {
            label: "detached".into(),
            class: "test".into(),
            query: "needle".into(),
            weight: 1,
            limit: 10,
            filter: QueryFilterSpec::default(),
        };

        let shared = run_one(Lane::SharedBackend, &db, Some(&backend), &case).unwrap();
        let fresh = run_one(Lane::FreshBackend, &db, None, &case).unwrap();
        assert_eq!(shared.0, 1);
        assert_eq!((shared.0, shared.2), (fresh.0, fresh.2));
        let generation = backend.current_generation_path().unwrap().unwrap();
        let generation = super::open_read(&generation).unwrap();
        let (_, _, counts) = super::validate_projection(store.conn(), &generation).unwrap();
        assert_eq!(counts.messages, 2);
        assert_eq!(counts.fts_rows, 1);
    }
}
