use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rosetta_search::{TextClause, TextQuery};
use rosetta_store::queries::{self, SearchFilter, SearchHit};
use rusqlite::{Connection, OpenFlags};
use tantivy::Order;
use tantivy::collector::TopDocs;
use tantivy::collector::sort_key::{SortBySimilarityScore, SortByString};
use tantivy::query::{
    BooleanQuery, ConstScoreQuery, Occur, Query, QueryParser, RangeQuery, TermQuery,
};
use tantivy::schema::{IndexRecordOption, TantivyDocument, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, ReloadPolicy, Term};

use crate::RUN_FORMAT;
use crate::fingerprint::{fingerprint_file, read_json, verify_campaign};
use crate::model::{
    CampaignManifest, CaseReport, QueryCase, QueryFilterSpec, RunConfig, RunReport,
};
use crate::stats::summarize;

use super::index::{Fields, build_index, fingerprint_directory};
use super::model::{TantivyCaseReport, TantivyClassReport, TantivyLaneReport, TantivyRunReport};

pub const TANTIVY_RUN_FORMAT: u32 = 2;

#[derive(Debug, Clone)]
pub struct TantivyRunOptions {
    pub campaign: PathBuf,
    pub ledger: PathBuf,
    pub queries: PathBuf,
    pub database: PathBuf,
    pub baseline: PathBuf,
    pub index: PathBuf,
    pub implementation: String,
    pub machine: String,
    pub threads: usize,
    pub memory_bytes: usize,
    pub warmup_cycles: u32,
    pub measured_cycles: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct RankedHit {
    timeline_id: String,
    session_id: String,
}

fn open_read(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("opening {} read-only: {error}", path.display()))?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .map_err(|error| format!("configuring read-only query connection: {error}"))?;
    Ok(connection)
}

fn filter(spec: &QueryFilterSpec) -> SearchFilter {
    SearchFilter {
        session: spec.session.clone(),
        runtime: spec.runtime.clone(),
        project: spec.project.clone(),
        kind: spec.kind.clone(),
        after_ts: spec.after_ts,
        before_ts: spec.before_ts,
    }
}

fn fts_hits(conn: &Connection, case: &QueryCase) -> Result<Vec<RankedHit>, String> {
    let expression = TextQuery::parse(&case.query)
        .map_err(|error| format!("FTS reference case `{}` failed: {error}", case.label))?
        .fts5_expression();
    queries::search_filtered(conn, &expression, case.limit, &filter(&case.filter))
        .map_err(|error| format!("FTS reference case `{}` failed: {error}", case.label))
        .map(|hits| {
            hits.into_iter()
                .map(|hit: SearchHit| RankedHit {
                    timeline_id: hit.timeline_id,
                    session_id: hit.session_id,
                })
                .collect()
        })
}

fn result_fingerprint(hits: &[RankedHit]) -> String {
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

fn term_filter(field: tantivy::schema::Field, text: &str) -> Box<dyn Query> {
    Box::new(ConstScoreQuery::new(
        Box::new(TermQuery::new(
            Term::from_field_text(field, text),
            IndexRecordOption::Basic,
        )),
        0.0,
    ))
}

fn range_filter(
    field: tantivy::schema::Field,
    lower: Bound<i64>,
    upper: Bound<i64>,
) -> Box<dyn Query> {
    let lower = match lower {
        Bound::Included(value) => Bound::Included(Term::from_field_i64(field, value)),
        Bound::Excluded(value) => Bound::Excluded(Term::from_field_i64(field, value)),
        Bound::Unbounded => Bound::Unbounded,
    };
    let upper = match upper {
        Bound::Included(value) => Bound::Included(Term::from_field_i64(field, value)),
        Bound::Excluded(value) => Bound::Excluded(Term::from_field_i64(field, value)),
        Bound::Unbounded => Bound::Unbounded,
    };
    Box::new(ConstScoreQuery::new(
        Box::new(RangeQuery::new(lower, upper)),
        0.0,
    ))
}

fn compile_query(
    parser: &QueryParser,
    fields: Fields,
    case: &QueryCase,
) -> Result<Box<dyn Query>, String> {
    let common = TextQuery::parse(&case.query)
        .map_err(|error| format!("Tantivy query case `{}` failed: {error}", case.label))?;
    let expression = common
        .clauses()
        .iter()
        .map(|clause| match clause {
            TextClause::Term(text) | TextClause::Phrase(text) => {
                let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
                format!("\"{escaped}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let text = parser
        .parse_query(&expression)
        .map_err(|error| format!("Tantivy query case `{}` failed: {error}", case.label))?;
    let mut clauses = vec![(Occur::Must, text)];
    for (value, field) in [
        (case.filter.session.as_deref(), fields.session_id),
        (case.filter.runtime.as_deref(), fields.runtime),
        (case.filter.project.as_deref(), fields.project),
        (case.filter.kind.as_deref(), fields.kind),
    ] {
        if let Some(value) = value {
            clauses.push((Occur::Must, term_filter(field, value)));
        }
    }
    if case.filter.after_ts.is_some() || case.filter.before_ts.is_some() {
        clauses.push((
            Occur::Must,
            range_filter(
                fields.ts,
                case.filter
                    .after_ts
                    .map_or(Bound::Unbounded, Bound::Included),
                case.filter
                    .before_ts
                    .map_or(Bound::Unbounded, Bound::Included),
            ),
        ));
    }
    if clauses.len() == 1 {
        Ok(clauses.pop().expect("text clause exists").1)
    } else {
        Ok(Box::new(BooleanQuery::new(clauses)))
    }
}

fn text_value(doc: &TantivyDocument, field: tantivy::schema::Field) -> &str {
    doc.get_first(field)
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

fn matches_filter(doc: &TantivyDocument, fields: Fields, filter: &QueryFilterSpec) -> bool {
    if let Some(value) = &filter.session
        && text_value(doc, fields.session_id) != value
    {
        return false;
    }
    if let Some(value) = &filter.runtime
        && text_value(doc, fields.runtime) != value
    {
        return false;
    }
    if let Some(value) = &filter.project
        && text_value(doc, fields.project) != value
    {
        return false;
    }
    if let Some(value) = &filter.kind
        && text_value(doc, fields.kind) != value
    {
        return false;
    }
    let ts = doc
        .get_first(fields.ts)
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if let Some(after) = filter.after_ts
        && ts < after
    {
        return false;
    }
    if let Some(before) = filter.before_ts
        && ts > before
    {
        return false;
    }
    true
}

fn search_one(
    reader: &tantivy::IndexReader,
    parser: &QueryParser,
    fields: Fields,
    case: &QueryCase,
) -> Result<(Vec<RankedHit>, u64, u64), String> {
    let started = Instant::now();
    let searcher = reader.searcher();
    let query = compile_query(parser, fields, case)?;
    let mut snippets = [fields.content, fields.tool_input, fields.tool_result]
        .into_iter()
        .map(|field| {
            let mut generator = SnippetGenerator::create(&searcher, query.as_ref(), field)
                .map_err(|error| format!("creating snippet generator: {error}"))?;
            generator.set_max_num_chars(150);
            Ok(generator)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let top_docs = searcher
        .search(
            query.as_ref(),
            &TopDocs::with_limit(case.limit as usize).order_by((
                (SortBySimilarityScore, Order::Desc),
                (SortByString::for_field("timeline_id"), Order::Asc),
            )),
        )
        .map_err(|error| format!("running Tantivy case `{}`: {error}", case.label))?;
    let mut hits = Vec::with_capacity(top_docs.len());
    let mut violations = 0_u64;
    let mut snippet_checksum = 0_u64;
    for ((_score, _timeline_id), address) in top_docs {
        let doc: TantivyDocument = searcher
            .doc(address)
            .map_err(|error| format!("loading Tantivy hit: {error}"))?;
        if !matches_filter(&doc, fields, &case.filter) {
            violations += 1;
        }
        for generator in &mut snippets {
            let snippet = generator.snippet_from_doc(&doc);
            snippet_checksum = snippet_checksum.wrapping_add(snippet.fragment().len() as u64);
            if !snippet.is_empty() {
                break;
            }
        }
        hits.push(RankedHit {
            timeline_id: text_value(&doc, fields.timeline_id).to_string(),
            session_id: text_value(&doc, fields.session_id).to_string(),
        });
    }
    black_box(snippet_checksum);
    let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Ok((hits, violations, nanos))
}

fn schedule(cases: &[QueryCase]) -> Vec<usize> {
    let mut schedule = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        schedule.extend(std::iter::repeat_n(index, case.weight as usize));
    }
    schedule
}

fn baseline_cases(report: &RunReport) -> Result<BTreeMap<&str, &CaseReport>, String> {
    let lane = report
        .lanes
        .iter()
        .find(|lane| lane.lane == "shared-connection")
        .ok_or("baseline report has no shared-connection lane")?;
    Ok(lane
        .cases
        .iter()
        .map(|case| (case.label.as_str(), case))
        .collect())
}

fn overlap(left: &[RankedHit], right: &[RankedHit]) -> u64 {
    let right: BTreeSet<&str> = right.iter().map(|hit| hit.timeline_id.as_str()).collect();
    left.iter()
        .filter(|hit| right.contains(hit.timeline_id.as_str()))
        .count() as u64
}

fn execute_queries(
    index: &Index,
    fields: Fields,
    cases: &[QueryCase],
    reference: &[Vec<RankedHit>],
    warmup_cycles: u32,
    measured_cycles: u32,
) -> Result<TantivyLaneReport, String> {
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .map_err(|error| format!("opening Tantivy reader: {error}"))?;
    let mut parser = QueryParser::for_index(
        index,
        vec![fields.content, fields.tool_input, fields.tool_result],
    );
    parser.set_conjunction_by_default();
    let schedule = schedule(cases);
    let mut expected: Vec<Option<(Vec<RankedHit>, u64)>> = vec![None; cases.len()];

    let observe = |case_index: usize,
                   sample: (Vec<RankedHit>, u64, u64),
                   expected: &mut [Option<(Vec<RankedHit>, u64)>]|
     -> Result<u64, String> {
        let (hits, violations, nanos) = sample;
        match &expected[case_index] {
            None => expected[case_index] = Some((hits, violations)),
            Some((expected_hits, expected_violations))
                if *expected_hits == hits && *expected_violations == violations => {}
            Some(_) => {
                return Err(format!(
                    "Tantivy case `{}` changed result within one frozen run",
                    cases[case_index].label
                ));
            }
        }
        Ok(nanos)
    };

    for cycle in 0..warmup_cycles as usize {
        for offset in 0..schedule.len() {
            let case_index = schedule[(offset + cycle) % schedule.len()];
            let sample = search_one(&reader, &parser, fields, &cases[case_index])?;
            observe(case_index, sample, &mut expected)?;
        }
    }

    let mut case_samples: Vec<Vec<u64>> = vec![Vec::new(); cases.len()];
    let mut all_samples = Vec::new();
    for cycle in 0..measured_cycles as usize {
        for offset in 0..schedule.len() {
            let case_index = schedule[(offset + cycle) % schedule.len()];
            let sample = search_one(&reader, &parser, fields, &cases[case_index])?;
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
        let (hits, violations) = expected[index]
            .take()
            .expect("every query case appears in the schedule");
        if violations != 0 {
            return Err(format!(
                "Tantivy case `{}` returned {violations} filter-violating hits",
                case.label
            ));
        }
        let fts = &reference[index];
        let common = overlap(fts, &hits);
        let top_10 = overlap(&fts[..fts.len().min(10)], &hits[..hits.len().min(10)]);
        let filter_json = serde_json::to_vec(&case.filter).map_err(|error| error.to_string())?;
        reports.push(TantivyCaseReport {
            label: case.label.clone(),
            class: case.class.clone(),
            query_blake3: blake3::hash(case.query.as_bytes()).to_hex().to_string(),
            filter_blake3: blake3::hash(&filter_json).to_hex().to_string(),
            weight: case.weight,
            limit: case.limit,
            fts_hits: fts.len() as u64,
            tantivy_hits: hits.len() as u64,
            fts_result_blake3: result_fingerprint(fts),
            tantivy_result_blake3: result_fingerprint(&hits),
            overlap: common,
            overlap_percent_of_fts: if fts.is_empty() {
                100.0
            } else {
                common as f64 * 100.0 / fts.len() as f64
            },
            top_10_overlap: top_10,
            top_1_same: fts.first().map(|hit| &hit.timeline_id)
                == hits.first().map(|hit| &hit.timeline_id),
            filter_violations: violations,
            latency: summarize(&case_samples[index]),
        });
    }
    let classes = class_samples
        .into_iter()
        .map(|(class, samples)| TantivyClassReport {
            class: class.to_string(),
            latency: summarize(&samples),
        })
        .collect();
    Ok(TantivyLaneReport {
        lane: "resident-reader".into(),
        latency: summarize(&all_samples),
        classes,
        cases: reports,
    })
}

fn validate_options(options: &TantivyRunOptions) -> Result<(), String> {
    if options.implementation.trim().is_empty() || options.machine.trim().is_empty() {
        return Err("implementation and machine labels must not be empty".into());
    }
    if options.warmup_cycles == 0 {
        return Err("at least one warmup cycle is required".into());
    }
    if options.measured_cycles < 100 {
        return Err("at least 100 measured cycles are required for p99".into());
    }
    if options.threads == 0 || options.memory_bytes == 0 {
        return Err("Tantivy threads and memory must be positive".into());
    }
    Ok(())
}

pub fn run_tantivy_prototype(options: &TantivyRunOptions) -> Result<TantivyRunReport, String> {
    validate_options(options)?;
    let manifest: CampaignManifest = read_json(&options.campaign)?;
    let suite = verify_campaign(&manifest, &options.ledger, &options.queries)?;
    let baseline: RunReport = read_json(&options.baseline)?;
    if baseline.format != RUN_FORMAT {
        return Err(format!(
            "baseline report format {} is not supported (expected {RUN_FORMAT})",
            baseline.format
        ));
    }
    if baseline.campaign_id != manifest.campaign_id
        || baseline.ledger != manifest.ledger
        || baseline.query_suite != manifest.query_suite
    {
        return Err("baseline report does not belong to the frozen campaign".into());
    }
    if baseline.machine != options.machine
        || baseline.os != std::env::consts::OS
        || baseline.arch != std::env::consts::ARCH
    {
        return Err("baseline and Tantivy run must use the same declared machine".into());
    }
    let requested_config = RunConfig {
        warmup_cycles: options.warmup_cycles,
        measured_cycles: options.measured_cycles,
    };
    if baseline.config != requested_config {
        return Err("baseline and Tantivy run need identical warmup/cycle counts".into());
    }
    let database = fingerprint_file(&options.database, true)?;
    if database.bytes != baseline.database.bytes || database.blake3 != baseline.database.blake3 {
        return Err("database differs from the frozen FTS baseline report".into());
    }

    let baseline_cases = baseline_cases(&baseline)?;
    let conn = open_read(&options.database)?;
    let mut reference = Vec::with_capacity(suite.cases.len());
    for case in &suite.cases {
        let hits = fts_hits(&conn, case)?;
        let digest = result_fingerprint(&hits);
        let expected = baseline_cases
            .get(case.label.as_str())
            .ok_or_else(|| format!("baseline is missing case `{}`", case.label))?;
        if expected.hits != hits.len() as u64 || expected.result_blake3 != digest {
            return Err(format!(
                "FTS reference result for `{}` differs from its frozen baseline",
                case.label
            ));
        }
        reference.push(hits);
    }
    drop(conn);

    let (build, index, fields) = build_index(
        &options.database,
        &options.index,
        options.threads,
        options.memory_bytes,
    )?;
    if build.documents != baseline.database.counts.messages {
        return Err(format!(
            "Tantivy indexed {} documents but baseline has {} messages",
            build.documents, baseline.database.counts.messages
        ));
    }
    let lane = execute_queries(
        &index,
        fields,
        &suite.cases,
        &reference,
        options.warmup_cycles,
        options.measured_cycles,
    )?;

    let index_after = fingerprint_directory(&options.index, true)?;
    if index_after != build.index {
        return Err("Tantivy index changed during its read-only query run".into());
    }
    let database_after = fingerprint_file(&options.database, true)?;
    if database_after != database {
        return Err("database changed during the Tantivy prototype".into());
    }
    let suite_after = verify_campaign(&manifest, &options.ledger, &options.queries)?;
    if suite_after != suite {
        return Err("query suite changed during the Tantivy prototype".into());
    }

    Ok(TantivyRunReport {
        format: TANTIVY_RUN_FORMAT,
        campaign_id: manifest.campaign_id,
        implementation: options.implementation.clone(),
        baseline_implementation: baseline.implementation,
        machine: options.machine.clone(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        suite_name: suite.name,
        suite_basis: suite.basis,
        database: baseline.database,
        build,
        config: requested_config,
        lane,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rosetta_core::IngestOp;
    use rosetta_core::rows::{
        MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
    };
    use rosetta_store::{Batch, Store};

    use super::*;
    use crate::model::QueryFilterSpec;
    use crate::tantivy_proto::index::unseal_for_test;

    fn message(id: &str, ordinal: i64, kind: MessageKind, content: &str, ts: i64) -> MessageRow {
        MessageRow {
            timeline_id: id.into(),
            session_id: "s1".into(),
            ordinal,
            kind,
            subtype: None,
            uuid: None,
            parent_uuid: None,
            ts: Some(ts),
            model: None,
            effort: None,
            content: Some(content.into()),
            tool: None,
            branch_tool_id: None,
            usage: None,
            agent_version: None,
            src_cursor: None,
            src_offset: None,
        }
    }

    #[test]
    fn prototype_indexes_canonical_rows_and_applies_filters_before_ranking() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.sqlite");
        let mut store = Store::open(&db).unwrap();
        store
            .apply(&Batch {
                ops: vec![
                    IngestOp::Runtime(RuntimeRow {
                        id: "rt1".into(),
                        kind: "test".into(),
                        data_root: "/fixture".into(),
                        enabled: true,
                        account: None,
                        config: None,
                        skills: None,
                        versions_seen: None,
                        discovered_at: 1,
                    }),
                    IngestOp::Project(ProjectRow {
                        id: "p1".into(),
                        name: "fixture".into(),
                        root_paths: vec!["/fixture".into()],
                        git_repo: None,
                    }),
                    IngestOp::SessionMeta(SessionRow {
                        id: "s1".into(),
                        native_session_id: "native-s1".into(),
                        runtime_id: "rt1".into(),
                        project_id: Some("p1".into()),
                        parent_session_id: None,
                        root_session_id: "s1".into(),
                        role: SessionRole::Main,
                        title: None,
                        summary: None,
                        started_at: 1,
                        updated_at: 1,
                        stream_state: StreamState::Indexed,
                    }),
                    IngestOp::Message(Box::new(message(
                        "m-user",
                        0,
                        MessageKind::User,
                        "ionosphere",
                        100,
                    ))),
                    IngestOp::Message(Box::new(message(
                        "m-tool",
                        1,
                        MessageKind::ToolUse,
                        "ionosphere",
                        200,
                    ))),
                ],
                checkpoints: vec![],
                source: None,
            })
            .unwrap();
        drop(store);

        let index_path = temp.path().join("tantivy");
        let (build, index, fields) = build_index(&db, &index_path, 1, 50 * 1024 * 1024).unwrap();
        assert_eq!(build.documents, 2);
        assert!(build.index.bytes > 0);

        let cases = vec![
            QueryCase {
                label: "all".into(),
                class: "broad".into(),
                query: "ionosphere".into(),
                weight: 1,
                limit: 10,
                filter: QueryFilterSpec::default(),
            },
            QueryCase {
                label: "tool".into(),
                class: "filtered".into(),
                query: "ionosphere".into(),
                weight: 1,
                limit: 10,
                filter: QueryFilterSpec {
                    kind: Some("tool_use".into()),
                    ..QueryFilterSpec::default()
                },
            },
        ];
        let conn = open_read(&db).unwrap();
        let reference = cases
            .iter()
            .map(|case| fts_hits(&conn, case).unwrap())
            .collect::<Vec<_>>();
        let reader = index.reader().unwrap();
        let parser = QueryParser::for_index(
            &index,
            vec![fields.content, fields.tool_input, fields.tool_result],
        );
        let (tied_hits, _, _) = search_one(&reader, &parser, fields, &cases[0]).unwrap();
        assert_eq!(
            tied_hits
                .iter()
                .map(|hit| hit.timeline_id.as_str())
                .collect::<Vec<_>>(),
            vec!["m-tool", "m-user"],
            "equal scores use ascending timeline identity, not document address"
        );
        let report = execute_queries(&index, fields, &cases, &reference, 1, 2).unwrap();
        assert_eq!(report.cases[0].tantivy_hits, 2);
        assert_eq!(report.cases[1].tantivy_hits, 1);
        assert_eq!(report.cases[1].filter_violations, 0);
        assert_eq!(report.cases[1].overlap, 1);

        drop(index);
        unseal_for_test(&index_path);
        fs::remove_dir_all(&index_path).unwrap();
    }
}
