//! The per-adapter tree harness. Every adapter's recorded fixtures, and a
//! generated nested family for each root-tree adapter, go through the real
//! metadata plan, the writer's setup and closed-source capture:
//!
//! - every adapter declares the admission this harness expects of it;
//! - its trees do not depend on roster or link order;
//! - a root-tree adapter's sources write only sessions of their own tree,
//!   and a physical file several trees share emits no ops (R1 invariant I7).
//!   Other adapters are measured the same way and only reported: widening
//!   one is its own decision, on this evidence;
//! - only a transcript carries a metadata cut;
//! - runtime adapters and truncated walks get no trees.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rosetta_adapter_support::{AgentAdapter, ColdAdmission, StreamDriver};
use rosetta_core::rows::RuntimeRow;
use rosetta_core::{IngestOp, ids};
use rosetta_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{SessionLink, TreeMap};
use crate::closed::tests::{adapters, copy_tree, fixture_roots};
use crate::closed::{
    BlockingEmit, ClosedSource, ColdWalkOptions, stream_closed_sources_into, streamed_setup_for,
};
use crate::instrument::Instrument;
use crate::meta::SessionFile;
use crate::{IngestOptions, discover, expand_data_root, meta_ingest_inventory_into};

/// What each adapter declares. Root trees only where this harness proves the
/// closure promise on the adapter's fixtures and nested families.
fn expected_admission(kind: &str) -> ColdAdmission {
    match kind {
        "claude-code" | "codex" => ColdAdmission::RootTree,
        "antigravity" | "copilot" | "grok" | "hermes" | "kimi-code" | "opencode" | "pi" => {
            ColdAdmission::Runtime
        }
        other => panic!("{other} has no declared cold admission in this harness"),
    }
}

/// The one session an op writes into, if any: [`IngestOp::named_session`],
/// the exhaustive classification the preparer's closure scan uses. Links an
/// op merely carries (a spawned child's id, a handoff target) write nothing.
fn written_session(op: &IngestOp) -> Option<&str> {
    op.named_session()
}

/// One adapter home, planned as a fresh cold build plans it.
struct Planned {
    runtime: RuntimeRow,
    root: PathBuf,
    files: Vec<SessionFile>,
    links: Vec<SessionLink>,
    planner: Store,
    truncated: bool,
}

fn plan(adapter: &'static dyn AgentAdapter, home: &Path, opts: &IngestOptions) -> Planned {
    plan_after(adapter, home, opts, |_| {})
}

/// [`plan`], with `between` run on the discovery census before the skim:
/// whatever it changes on disk, the census no longer sees.
fn plan_after(
    adapter: &'static dyn AgentAdapter,
    home: &Path,
    opts: &IngestOptions,
    between: impl FnOnce(&crate::Discovery),
) -> Planned {
    let instrument = Instrument::new();
    let discovery = discover(&[adapter], home, opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    between(&discovery);
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    let mut planner = Store::open_in_memory_for_cold_planning().unwrap();
    let (meta, files) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut planner,
        &discovery.inventory,
        discovery.walk_truncated,
        opts,
        &instrument,
        None,
    )
    .unwrap();
    let links = planner
        .planned_metadata_batches()
        .unwrap()
        .into_iter()
        .flat_map(|batch| batch.ops)
        .filter_map(|op| match op {
            IngestOp::SessionMeta(row) => {
                Some((row.id, row.parent_session_id, Some(row.root_session_id)))
            }
            _ => None,
        })
        .collect();
    Planned {
        runtime,
        root: discovery.root,
        files,
        links,
        planner,
        truncated: discovery.walk_truncated || meta.walk_truncated,
    }
}

/// Capture every closed source once the setup has taken the metadata:
/// `(source path, its ops)` in emission order.
fn capture(adapter: &dyn AgentAdapter, planned: &mut Planned) -> Vec<(String, Vec<IngestOp>)> {
    let mut emitted = Vec::new();
    let mut emit = BlockingEmit(|_, sources: Vec<ClosedSource>| {
        emitted.extend(
            sources
                .into_iter()
                .map(|source| (source.source.relative_path, source.ops)),
        );
        Ok(())
    });
    stream_closed_sources_into(
        adapter,
        &planned.runtime,
        &planned.root,
        &mut planned.planner,
        &planned.files,
        &IngestOptions::default(),
        &Instrument::new(),
        None,
        None,
        &ColdWalkOptions::default(),
        &mut emit,
    )
    .unwrap();
    emitted
}

/// A home holding `data` where `adapter` looks for its data root.
fn home_with(adapter: &dyn AgentAdapter, data: &Path) -> TempDir {
    let home = TempDir::new().unwrap();
    copy_tree(
        data,
        &expand_data_root(&adapter.layout().data_root, home.path()),
    );
    home
}

/// Every home the harness runs `adapter` on: each recorded fixture; for a
/// root-tree adapter, all its fixtures in one home, then (last) the
/// generated nested family.
pub(crate) fn homes(adapter: &dyn AgentAdapter) -> Vec<(String, TempDir)> {
    let mut homes: Vec<(String, TempDir)> = fixture_roots(adapter.kind())
        .into_iter()
        .map(|(name, data)| (name, home_with(adapter, &data)))
        .collect();
    let family: Option<fn(&Path)> = match adapter.kind() {
        "claude-code" => Some(claude_family),
        "codex" => Some(codex_family),
        _ => None,
    };
    if let Some(write) = family {
        homes.push((MERGED.to_owned(), merged_fixtures(adapter)));
        let home = TempDir::new().unwrap();
        write(&expand_data_root(&adapter.layout().data_root, home.path()));
        homes.push(("nested-family".to_owned(), home));
    }
    homes
}

/// The home holding every recorded fixture of an adapter at once.
const MERGED: &str = "merged-fixtures";

/// Every recorded fixture of `adapter` in one home. Each fixture alone is a
/// single tree, where no op can reach another tree's session; together, the
/// recorded content meets other trees. A path an earlier fixture wrote (a
/// config file, a state database) keeps that fixture's copy, and so do the
/// database's sidecars.
fn merged_fixtures(adapter: &dyn AgentAdapter) -> TempDir {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    for (_, data) in fixture_roots(adapter.kind()) {
        merge_tree(&data, &root);
    }
    home
}

fn merge_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    let mut entries = std::fs::read_dir(from)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    // A database sorts before its `-wal`/`-shm`/`-journal` sidecars.
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut kept_earlier: HashSet<String> = HashSet::new();
    for entry in entries {
        let source = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let destination = to.join(&name);
        if source.is_dir() {
            merge_tree(&source, &destination);
            continue;
        }
        let database = ["-wal", "-shm", "-journal"]
            .iter()
            .find_map(|suffix| name.strip_suffix(suffix));
        if destination.exists() || database.is_some_and(|db| kept_earlier.contains(db)) {
            kept_earlier.insert(name);
            continue;
        }
        std::fs::copy(source, destination).unwrap();
    }
}

/// Deterministic Fisher-Yates over a xorshift stream.
fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for at in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(at, (state % (at as u64 + 1)) as usize);
    }
}

/// Physical files whose members belong to more than one tree.
fn shared_files<'a>(files: &'a [SessionFile], trees: &TreeMap) -> HashSet<&'a str> {
    let mut readers: HashMap<&str, BTreeSet<u32>> = HashMap::new();
    for file in files
        .iter()
        .filter(|file| file.driver != StreamDriver::JsonLines)
    {
        readers
            .entry(file.physical_path())
            .or_default()
            .insert(trees.tree_of_file(file).unwrap());
    }
    readers
        .into_iter()
        .filter_map(|(path, trees)| (trees.len() > 1).then_some(path))
        .collect()
}

/// What closure looked like on one home.
#[derive(Debug, Default)]
struct Closure {
    sources: usize,
    ops: usize,
    /// Ops writing a session outside their source's tree.
    crossings: Vec<String>,
    /// Ops emitted by members of a physical file several trees share.
    shared_ops: Vec<String>,
    /// Ops of a source writing a session other than its own, inside its tree.
    foreign_in_tree: usize,
}

fn closure(files: &[SessionFile], trees: &TreeMap, emitted: &[(String, Vec<IngestOp>)]) -> Closure {
    let by_path: HashMap<&str, &SessionFile> = files
        .iter()
        .map(|file| (file.relative_path.as_str(), file))
        .collect();
    let shared = shared_files(files, trees);
    let mut report = Closure::default();
    for (path, ops) in emitted {
        let file = by_path[path.as_str()];
        let tree = trees.tree_of_file(file).unwrap();
        report.sources += 1;
        report.ops += ops.len();
        if shared.contains(file.physical_path()) && !ops.is_empty() {
            report.shared_ops.push(format!("{path}: {} ops", ops.len()));
        }
        for session in ops.iter().filter_map(written_session) {
            if trees.tree_of(session) != Some(tree) {
                report.crossings.push(format!("{path} writes {session}"));
            } else if session != file.session_id {
                report.foreign_in_tree += 1;
            }
        }
    }
    report
}

#[test]
fn every_adapter_declares_the_admission_its_evidence_supports() {
    let adapters = adapters();
    assert_eq!(adapters.len(), 9);
    for adapter in adapters {
        assert_eq!(
            adapter.cold_admission(),
            expected_admission(adapter.kind()),
            "{}",
            adapter.kind()
        );
    }
}

#[test]
fn trees_do_not_depend_on_roster_or_link_order_on_any_adapter() {
    for adapter in adapters() {
        for (name, home) in homes(adapter) {
            let planned = plan(adapter, home.path(), &IngestOptions::default());
            let label = format!("{}/{name}", adapter.kind());
            assert!(!planned.files.is_empty(), "{label} has no roster");
            let reference = TreeMap::build(&planned.files, &planned.links);

            // Every roster session and every row is placed, each once.
            let mut nodes = BTreeSet::new();
            for file in &planned.files {
                assert!(reference.tree_of_file(file).is_some(), "{label}");
                nodes.insert(file.session_id.as_str());
            }
            for (id, _, _) in &planned.links {
                assert!(reference.tree_of(id).is_some(), "{label}");
            }
            let placed: usize = reference
                .trees()
                .iter()
                .map(|tree| tree.sessions.len())
                .sum();
            let mut members = BTreeSet::new();
            for tree in reference.trees() {
                assert_eq!(tree.id, tree.sessions[0], "{label}");
                members.extend(tree.sessions.iter().map(String::as_str));
            }
            assert_eq!(placed, members.len(), "{label}: a session in two trees");
            assert!(nodes.is_subset(&members), "{label}");

            let (mut files, mut links) = (planned.files.clone(), planned.links.clone());
            for seed in 0..20 {
                shuffle(&mut files, seed);
                shuffle(&mut links, seed.wrapping_add(7_919));
                assert_eq!(
                    TreeMap::build(&files, &links),
                    reference,
                    "{label} seed {seed}"
                );
            }
            files.reverse();
            links.reverse();
            assert_eq!(
                TreeMap::build(&files, &links),
                reference,
                "{label} reversed"
            );
        }
    }
}

#[test]
fn root_tree_sources_write_only_their_own_tree() {
    for adapter in adapters() {
        let root_tree = adapter.cold_admission() == ColdAdmission::RootTree;
        for (name, home) in homes(adapter) {
            let label = format!("{}/{name}", adapter.kind());
            let mut planned = plan(adapter, home.path(), &IngestOptions::default());
            let setup = streamed_setup_for(
                &planned.files,
                &mut planned.planner,
                adapter.cold_admission(),
                planned.truncated,
            )
            .unwrap();
            let trees = TreeMap::build(&planned.files, &planned.links);
            if root_tree {
                assert_eq!(setup.trees().map(|trees| &**trees), Some(&trees), "{label}");
            }
            let emitted = capture(adapter, &mut planned);
            let report = closure(&planned.files, &trees, &emitted);
            eprintln!(
                "tree-closure: agent={label} admission={:?} trees={} sources={} ops={} \
                 crossings={} shared_ops={} in_tree_foreign={}",
                adapter.cold_admission(),
                trees.trees().len(),
                report.sources,
                report.ops,
                report.crossings.len(),
                report.shared_ops.len(),
                report.foreign_in_tree,
            );
            if root_tree {
                assert!(report.ops > 0, "{label} captured nothing");
                if name == MERGED {
                    // Recorded content only proves closure where there is
                    // another tree to cross into.
                    assert!(trees.trees().len() > 1, "{label} is one tree");
                }
                assert!(
                    report.crossings.is_empty(),
                    "{label}: {:?}",
                    report.crossings
                );
                assert!(
                    report.shared_ops.is_empty(),
                    "{label}: {:?}",
                    report.shared_ops
                );
            }
        }
    }
}

#[test]
fn only_a_transcript_carries_a_metadata_cut() {
    for adapter in adapters() {
        for (name, home) in homes(adapter) {
            let label = format!("{}/{name}", adapter.kind());
            let planned = plan(adapter, home.path(), &IngestOptions::default());
            for file in &planned.files {
                let transcript = file.driver == StreamDriver::JsonLines;
                assert_eq!(
                    file.cut_len.is_some(),
                    transcript,
                    "{label} {}",
                    file.relative_path
                );
                assert_eq!(
                    file.cut_modified_ms.is_some(),
                    transcript,
                    "{label} {}",
                    file.relative_path
                );
                if transcript {
                    // Nothing wrote between census and skim.
                    assert_eq!(file.cut_len, Some(file.size), "{label}");
                    assert_eq!(file.cut_modified_ms, Some(file.modified_ms), "{label}");
                }
                assert!(
                    file.tree_parent.is_none() || file.ledger_only,
                    "{label}: a public source carries its links in its row"
                );
            }
        }
    }
}

/// The census is taken before the consent wait, so it can be arbitrarily
/// old by the skim. A transcript that grew and was touched in between is
/// cut, measured and ranked at what the skim's own fstat saw.
#[test]
fn the_cut_is_the_skims_own_fstat_not_the_census() {
    for adapter in adapters()
        .into_iter()
        .filter(|adapter| adapter.cold_admission() == ColdAdmission::RootTree)
    {
        let label = adapter.kind();
        let (_, home) = homes(adapter).into_iter().next().unwrap();
        let reference = plan(adapter, home.path(), &IngestOptions::default());
        let target = reference
            .files
            .iter()
            .find(|file| file.driver == StreamDriver::JsonLines && !file.ledger_only)
            .unwrap()
            .relative_path
            .clone();
        let mut grown = (0, 0);
        let planned = plan_after(adapter, home.path(), &IngestOptions::default(), |found| {
            // Append a copy of the last record and stamp a later mtime.
            let path = found.root.join(&target);
            let bytes = std::fs::read(&path).unwrap();
            let last = bytes[..bytes.len() - 1]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |at| at + 1);
            let census = found
                .inventory
                .iter()
                .find(|file| file.relative_path == target)
                .unwrap();
            // A year on: newer than anything else in the fixture's tree.
            let later = census.modified_ms + 365 * 86_400_000;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            std::io::Write::write_all(&mut file, &bytes[last..]).unwrap();
            file.set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(later as u64),
            )
            .unwrap();
            grown = ((bytes.len() * 2 - last) as u64, later);
        });
        let file = planned
            .files
            .iter()
            .find(|file| file.relative_path == target)
            .unwrap();
        let before = reference
            .files
            .iter()
            .find(|file| file.relative_path == target)
            .unwrap();
        // The census facts are the stale ones; the cut is the skim's.
        assert_eq!(
            (file.size, file.modified_ms),
            (before.size, before.modified_ms)
        );
        assert_eq!(
            (file.cut_len, file.cut_modified_ms),
            (Some(grown.0), Some(grown.1)),
            "{label}"
        );
        let trees = TreeMap::build(&planned.files, &planned.links);
        let tree = trees.tree(trees.tree_of_file(file).unwrap());
        assert_eq!(tree.k, grown.1, "{label}: a tree ranks by its newest cut");
        assert!(tree.max_source_bytes >= grown.0, "{label}");
    }
}

#[test]
fn runtime_adapters_and_truncated_walks_get_no_trees() {
    for adapter in adapters() {
        let Some((_, home)) = homes(adapter).into_iter().next() else {
            continue;
        };
        let label = adapter.kind();
        let mut planned = plan(adapter, home.path(), &IngestOptions::default());
        assert!(!planned.truncated, "{label}");
        let setup = streamed_setup_for(
            &planned.files,
            &mut planned.planner,
            adapter.cold_admission(),
            planned.truncated,
        )
        .unwrap();
        assert_eq!(setup.admission(), adapter.cold_admission(), "{label}");
        let root_tree = adapter.cold_admission() == ColdAdmission::RootTree;
        assert_eq!(setup.trees().is_some(), root_tree, "{label}");
        assert!(!setup.fell_back(), "{label}");

        // The kill switch asks for runtime admission: no trees either.
        let mut planned = plan(adapter, home.path(), &IngestOptions::default());
        let setup = streamed_setup_for(
            &planned.files,
            &mut planned.planner,
            ColdAdmission::Runtime,
            false,
        )
        .unwrap();
        assert!(setup.trees().is_none() && !setup.fell_back(), "{label}");
    }

    // A walk that stopped early cannot prove a tree is whole.
    for adapter in adapters()
        .into_iter()
        .filter(|adapter| adapter.cold_admission() == ColdAdmission::RootTree)
    {
        let (_, home) = homes(adapter).pop().unwrap();
        let opts = IngestOptions {
            max_files: 2,
            ..IngestOptions::default()
        };
        let mut planned = plan(adapter, home.path(), &opts);
        assert!(planned.truncated, "{}", adapter.kind());
        let setup = streamed_setup_for(
            &planned.files,
            &mut planned.planner,
            adapter.cold_admission(),
            planned.truncated,
        )
        .unwrap();
        assert_eq!(setup.admission(), ColdAdmission::RootTree);
        assert!(
            setup.trees().is_none() && setup.fell_back(),
            "{}",
            adapter.kind()
        );
    }
}

/// The generated families' trees, by native id: nested parents, sidecar-only
/// placeholders, a guardian and a state-only session.
#[test]
fn nested_families_form_one_tree_per_root() {
    let adapter = |kind: &str| {
        adapters()
            .into_iter()
            .find(|adapter| adapter.kind() == kind)
            .unwrap()
    };
    for (adapter, expected, guardian) in [
        (
            adapter("claude-code"),
            vec![vec!["a", "c", "m", "z", "zz"], vec!["n"]],
            None,
        ),
        (
            adapter("codex"),
            vec![vec!["c", "g", "r", "x"], vec!["s"], vec!["u"]],
            Some(("x", "g")),
        ),
    ] {
        let label = adapter.kind();
        let (_, home) = homes(adapter).pop().unwrap();
        let mut planned = plan(adapter, home.path(), &IngestOptions::default());
        let runtime = planned.runtime.id.clone();
        let session = |native: &str| ids::session_id(&runtime, native);
        let trees = TreeMap::build(&planned.files, &planned.links);
        let mut grouped: Vec<Vec<String>> = expected
            .iter()
            .map(|natives| {
                let mut ids: Vec<String> = natives.iter().map(|native| session(native)).collect();
                ids.sort();
                ids
            })
            .collect();
        grouped.sort();
        let mut actual: Vec<Vec<String>> = trees
            .trees()
            .iter()
            .map(|tree| tree.sessions.clone())
            .collect();
        actual.sort();
        assert_eq!(actual, grouped, "{label}");

        let emitted = capture(adapter, &mut planned);
        if let Some((guardian, parent)) = guardian {
            // The guardian admits no row; only its source links it, and its
            // usage lands on the session it reviewed.
            let file = planned
                .files
                .iter()
                .find(|file| file.ledger_only && file.session_id == session(guardian))
                .unwrap();
            assert_eq!(file.tree_parent, Some(session(parent)), "{label}");
            assert!(
                planned
                    .links
                    .iter()
                    .all(|(id, _, _)| id != &session(guardian)),
                "{label}"
            );
            let usage = emitted
                .iter()
                .filter(|(path, _)| path == &file.relative_path)
                .flat_map(|(_, ops)| ops)
                .filter(|op| written_session(op) == Some(session(parent).as_str()))
                .count();
            assert!(
                usage > 0,
                "{label}: the guardian wrote nothing to its parent"
            );
            // The state database is every tree's: its members emit nothing.
            assert!(
                shared_files(&planned.files, &trees).contains("state_5.sqlite"),
                "{label}"
            );
        }
        let report = closure(&planned.files, &trees, &emitted);
        assert!(
            report.crossings.is_empty(),
            "{label}: {:?}",
            report.crossings
        );
        assert!(
            report.shared_ops.is_empty(),
            "{label}: {:?}",
            report.shared_ops
        );
    }
}

fn write_lines(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(path, text).unwrap();
}

fn claude_record(session: &str, agent: Option<&str>, uuid: &str, text: &str) -> Value {
    let mut record = json!({
        "parentUuid": null,
        "isSidechain": agent.is_some(),
        "type": "user",
        "message": {"role": "user", "content": text},
        "uuid": uuid,
        "timestamp": "2026-08-25T02:35:19.054Z",
        "userType": "external",
        "cwd": "/w/project",
        "sessionId": session,
        "version": "2.1.241",
        "gitBranch": "main"
    });
    if let Some(agent) = agent {
        record["agentId"] = json!(agent);
    }
    record
}

/// Main session `m` with subagent `a`, subagent `c` whose sidecar names `a`
/// as its parent agent, and a sidecar-only subagent `z` whose parent agent
/// `zz` exists only as a placeholder; a separate main session `n`.
fn claude_family(root: &Path) {
    let project = root.join("projects/-w-project");
    write_lines(
        &project.join("m.jsonl"),
        &[claude_record("m", None, "m-1", "start the work")],
    );
    let subagents = project.join("m/subagents");
    for agent in ["a", "c"] {
        write_lines(
            &subagents.join(format!("agent-{agent}.jsonl")),
            &[claude_record(
                "m",
                Some(agent),
                &format!("{agent}-1"),
                "delegated work",
            )],
        );
    }
    for (agent, parent) in [("a", None), ("c", Some("a")), ("z", Some("zz"))] {
        let mut meta = json!({"agentType": "Explore"});
        if let Some(parent) = parent {
            meta["parentAgentId"] = json!(parent);
        }
        std::fs::write(
            subagents.join(format!("agent-{agent}.meta.json")),
            meta.to_string(),
        )
        .unwrap();
    }
    write_lines(
        &project.join("n.jsonl"),
        &[claude_record("n", None, "n-1", "another session")],
    );
}

fn codex_record(timestamp: &str, ordinal: u64, kind: &str, payload: Value) -> Value {
    json!({"timestamp": timestamp, "ordinal": ordinal, "type": kind, "payload": payload})
}

fn codex_message(ordinal: u64, role: &str, text: &str) -> Value {
    codex_record(
        "2026-09-20T10:00:01.000Z",
        ordinal,
        "response_item",
        json!({
            "type": "message", "role": role,
            "content": [{"type": if role == "user" { "input_text" } else { "output_text" }, "text": text}],
        }),
    )
}

fn codex_header(id: &str, extra: Value) -> Value {
    let mut meta = json!({
        "id": id, "timestamp": "2026-09-20T10:00:00.100Z", "cwd": "/w/project",
        "originator": "codex_exec", "cli_version": "0.153.4", "source": "exec",
        "model_provider": "openai", "history_mode": "paginated",
    });
    meta.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    codex_record("2026-09-20T10:00:00.100Z", 0, "session_meta", meta)
}

fn codex_usage(input: i64, output: i64) -> Value {
    json!({
        "input_tokens": input, "cached_input_tokens": 0, "cache_write_input_tokens": 0,
        "output_tokens": output, "reasoning_output_tokens": 0, "total_tokens": input + output,
    })
}

/// Root `r`, its child `c`, grandchild `g` (each child's root is its
/// immediate parent, as Codex writes it), a guardian `x` reviewing `g`, an
/// unrelated root `u`, and `s`, which exists only as a state-database row.
fn codex_family(root: &Path) {
    let rollout = |id: &str| root.join(format!("sessions/2026/09/20/rollout-{id}.jsonl"));
    for (id, parent) in [("r", None), ("c", Some("r")), ("g", Some("c")), ("u", None)] {
        let extra = match parent {
            Some(parent) => json!({"parent_thread_id": parent, "thread_source": "subagent",
                "source": {"subagent": {"thread_spawn": {"parent_thread_id": parent}}}}),
            None => json!({}),
        };
        write_lines(
            &rollout(id),
            &[
                codex_header(id, extra),
                codex_message(1, "user", "do the work"),
                codex_message(2, "assistant", "done"),
            ],
        );
    }
    write_lines(
        &rollout("x"),
        &[
            codex_header(
                "x",
                json!({"parent_thread_id": "g", "thread_source": "guardian_review",
                    "source": {"subagent": {"other": "guardian"}},
                    "subagent_history_start_ordinal": 30}),
            ),
            codex_record(
                "2026-09-20T10:00:00.200Z",
                1,
                "turn_context",
                json!({"cwd": "/w/project", "model": "codex-auto-review"}),
            ),
            codex_record(
                "2026-09-20T10:00:05.000Z",
                2,
                "event_msg",
                json!({"type": "token_count", "info": {
                    "total_token_usage": codex_usage(300, 30),
                    "last_token_usage": codex_usage(300, 30),
                }}),
            ),
        ],
    );
    let state = rusqlite::Connection::open(root.join("state_5.sqlite")).unwrap();
    state
        .execute_batch(
            "CREATE TABLE threads(id TEXT PRIMARY KEY, source TEXT, thread_source TEXT, \
             cwd TEXT, title TEXT, cli_version TEXT, created_at INTEGER, updated_at INTEGER);",
        )
        .unwrap();
    for (id, source, thread_source) in [
        ("r", json!("exec"), "user"),
        (
            "c",
            json!({"subagent": {"thread_spawn": {"parent_thread_id": "r"}}}),
            "subagent",
        ),
        (
            "g",
            json!({"subagent": {"thread_spawn": {"parent_thread_id": "c"}}}),
            "subagent",
        ),
        (
            "x",
            json!({"subagent": {"other": "guardian"}}),
            "guardian_review",
        ),
        ("u", json!("exec"), "user"),
        ("s", json!("exec"), "user"),
    ] {
        let source = source
            .as_str()
            .map_or_else(|| source.to_string(), str::to_owned);
        state
            .execute(
                "INSERT INTO threads VALUES (?1, ?2, ?3, '/w/project', 'Work', '0.153.4', 1, 2)",
                rusqlite::params![id, source, thread_source],
            )
            .unwrap();
    }
}
