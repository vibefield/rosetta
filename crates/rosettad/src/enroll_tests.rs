//! The enrollment file's refusals and the target list it produces. Roots are
//! temp directories; nothing here reads a live home.

use std::path::Path;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_core::ids;

use super::*;

fn registry() -> Vec<&'static dyn AgentAdapter> {
    crate::registry()
}

/// A ground-truth fixture tree (`groundtruth/claude-code/fixtures/2.1.241/
/// <name>/claude-config`) copied to `to` — the same hermetic source the serve
/// integration tests use.
pub(crate) fn copy_fixture(name: &str, to: &Path) {
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let source = entry.path();
            let destination = to.join(entry.file_name());
            if source.is_dir() {
                copy_tree(&source, &destination);
            } else {
                std::fs::copy(source, destination).unwrap();
            }
        }
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../groundtruth/claude-code/fixtures/2.1.241")
        .join(name)
        .join("claude-config");
    copy_tree(&fixture, to);
}

fn parse(text: &str, home: &Path) -> Result<Enrollment, String> {
    Enrollment::parse(text, Path::new("/tmp/enroll.json"), &registry(), home)
}

fn file(runtimes: &[(&str, &Path, Option<&str>)], enabled: &[&str]) -> String {
    let runtimes: Vec<serde_json::Value> = runtimes
        .iter()
        .map(|(kind, root, label)| {
            let mut entry =
                serde_json::json!({ "kind": kind, "data_root": root.to_string_lossy() });
            if let Some(label) = label {
                entry["label"] = serde_json::Value::from(*label);
            }
            entry
        })
        .collect();
    serde_json::json!({
        "format": 1,
        "enabled_kinds": enabled,
        "runtimes": runtimes,
        "future_field": { "ignored": true },
    })
    .to_string()
}

#[test]
fn a_valid_file_yields_enrolled_runtimes_with_absolute_identities() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let kim = temp.path().join("agents/kim");
    let lee = temp.path().join("agents/lee");
    std::fs::create_dir_all(&kim).unwrap();
    std::fs::create_dir_all(&lee).unwrap();
    let enrollment = parse(
        &file(
            &[("claude-code", &kim, Some("Kim")), ("codex", &lee, None)],
            &["claude-code", "grok"],
        ),
        &home,
    )
    .unwrap();
    assert_eq!(enrollment.enabled_kinds, ["claude-code", "grok"]);
    assert_eq!(enrollment.runtimes.len(), 2);
    assert!(enrollment.exclude_cwds.is_empty());
    let first = &enrollment.runtimes[0];
    assert_eq!(first.adapter.kind(), "claude-code");
    assert_eq!(first.label.as_deref(), Some("Kim"));
    assert_eq!(first.runtime.data_root, kim.to_string_lossy());
    assert_eq!(
        first.runtime.id,
        ids::runtime_id("claude-code", &kim.to_string_lossy())
    );
    assert!(
        !first.runtime.enabled,
        "consent is flipped by the serve path"
    );
    assert_eq!(enrollment.runtimes[1].label, None);
    let presentation: Vec<_> = enrollment.presentation().collect();
    assert_eq!(presentation[0].1.as_deref(), Some("Kim"));
    assert_eq!(enrollment.enrolled_ids().len(), 2);
}

#[test]
fn folder_exclusions_are_optional_absolute_and_independent_of_existing_roots() {
    let home = Path::new("/unused-home");
    let enrollment = parse(
        r#"{"format":1,"exclude_cwds":["/deleted/client/./", "/deleted/client"]}"#,
        home,
    )
    .unwrap();
    let opts = Enrollment::ingest_options(Some(&enrollment));
    assert!(opts.exclude_cwds.excludes("/deleted/client/sub"));
    assert!(!opts.exclude_cwds.excludes("/deleted/client-other"));
    assert!(Enrollment::ingest_options(None).exclude_cwds.is_empty());
    for invalid in ["", "relative", "~/client"] {
        let error = parse(
            &serde_json::json!({
                "format":1, "exclude_cwds":["/valid", invalid]
            })
            .to_string(),
            home,
        )
        .unwrap_err();
        assert!(
            error.contains("--enroll /tmp/enroll.json: exclude_cwds[1]"),
            "{error}"
        );
    }
    for invalid in [
        serde_json::json!(null),
        serde_json::json!("/folder"),
        serde_json::json!([1]),
    ] {
        assert!(
            parse(
                &serde_json::json!({"format":1,"exclude_cwds":invalid}).to_string(),
                home
            )
            .is_err()
        );
    }
}

/// Witness (e): every refusal at start names the entry.
#[test]
fn refusals_name_the_entry() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let native = home.join(".claude");
    std::fs::create_dir_all(native.join("projects")).unwrap();
    let kim = temp.path().join("agents/kim");
    std::fs::create_dir_all(&kim).unwrap();

    // Unsupported format.
    let error = parse(r#"{"format": 2, "runtimes": []}"#, &home).unwrap_err();
    assert!(error.contains("unsupported enrollment format 2"), "{error}");

    // Relative and `~` roots.
    let error = parse(
        r#"{"format": 1, "runtimes": [{"kind": "claude-code", "data_root": "agents/kim"}]}"#,
        &home,
    )
    .unwrap_err();
    assert!(error.contains("runtime #1"), "{error}");
    assert!(error.contains("must be an absolute path"), "{error}");
    let error = parse(
        r#"{"format": 1, "runtimes": [{"kind": "claude-code", "data_root": "~/agents/kim"}]}"#,
        &home,
    )
    .unwrap_err();
    assert!(error.contains("must be an absolute path"), "{error}");

    // Unknown kind.
    let error = parse(&file(&[("not-an-agent", &kim, None)], &[]), &home).unwrap_err();
    assert!(error.contains("runtime #1"), "{error}");
    assert!(error.contains("unknown agent kind"), "{error}");

    // Duplicate.
    let error = parse(
        &file(
            &[("claude-code", &kim, None), ("claude-code", &kim, None)],
            &[],
        ),
        &home,
    )
    .unwrap_err();
    assert!(error.contains("runtime #2"), "{error}");
    assert!(error.contains("duplicates runtime #1"), "{error}");

    // Nested inside another enrolled root.
    let nested = kim.join("inner");
    std::fs::create_dir_all(&nested).unwrap();
    let error = parse(
        &file(
            &[("claude-code", &kim, None), ("codex", &nested, None)],
            &[],
        ),
        &home,
    )
    .unwrap_err();
    assert!(error.contains("runtime #2"), "{error}");
    assert!(error.contains("is nested with runtime #1"), "{error}");

    // The native root itself, and a root nested inside it.
    let error = parse(&file(&[("claude-code", &native, None)], &[]), &home).unwrap_err();
    assert!(error.contains("is the native claude-code root"), "{error}");
    assert!(error.contains("enabled_kinds"), "{error}");
    let inside = native.join("projects");
    let error = parse(&file(&[("claude-code", &inside, None)], &[]), &home).unwrap_err();
    assert!(
        error.contains("is nested with the native claude-code root"),
        "{error}"
    );

    // A missing directory is a refusal, not a skip.
    let missing = temp.path().join("agents/never-created");
    let error = parse(&file(&[("claude-code", &missing, None)], &[]), &home).unwrap_err();
    assert!(error.contains("runtime #1"), "{error}");
    assert!(error.contains("not an existing directory"), "{error}");
    let not_a_dir = temp.path().join("agents/file");
    std::fs::write(&not_a_dir, b"x").unwrap();
    let error = parse(&file(&[("claude-code", &not_a_dir, None)], &[]), &home).unwrap_err();
    assert!(error.contains("is not a directory"), "{error}");

    // Unreadable JSON names the file.
    let error = parse("{", &home).unwrap_err();
    assert!(error.contains("not a valid enrollment file"), "{error}");
}

/// Witness (d) at the target level: enrolled roots are targets with their
/// own ids and never roll-call rows; `enabled_kinds` maps onto the
/// discovered target of that kind and warns, never fails, for the rest.
#[test]
fn targets_carry_two_runtimes_of_one_kind_and_the_roll_call_lists_only_the_native_one() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let native = home.join(".claude");
    copy_fixture("basic-chat", &native);
    let kim = temp.path().join("agents/kim");
    copy_fixture("tool-use", &kim);
    let empty = temp.path().join("agents/empty");
    std::fs::create_dir_all(&empty).unwrap();

    let enrollment = parse(
        &file(
            &[
                ("claude-code", &kim, Some("Kim")),
                ("codex", &empty, Some("Lee")),
            ],
            &["claude-code", "codex", "not-an-agent"],
        ),
        &home,
    )
    .unwrap();
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let adapters: Vec<&'static dyn AgentAdapter> = vec![&ClaudeCodeAdapter, &CodexAdapter];
    let targets = resolve_targets(&adapters, &home, Some(&enrollment), &opts, &instrument).unwrap();
    assert_eq!(targets.len(), 4);

    let native_target = &targets[0];
    assert!(!native_target.is_enrolled() && native_target.installed());
    assert_eq!(native_target.runtime.data_root, "~/.claude");
    assert_eq!(
        native_target.id(),
        ids::runtime_id("claude-code", "~/.claude")
    );
    let codex_target = &targets[1];
    assert!(!codex_target.is_enrolled() && !codex_target.installed());

    let kim_target = &targets[2];
    assert!(kim_target.is_enrolled() && kim_target.installed() && kim_target.active());
    assert_eq!(kim_target.kind(), "claude-code");
    assert_ne!(kim_target.id(), native_target.id());
    assert_eq!(
        kim_target.id(),
        ids::runtime_id("claude-code", &kim.to_string_lossy())
    );
    assert_eq!(kim_target.label.as_deref(), Some("Kim"));
    assert_eq!(kim_target.provenance, RuntimeProvenance::Enrolled);
    assert_eq!(kim_target.name(), "claude-code «Kim»");
    assert_eq!(kim_target.identity().label.as_deref(), Some("Kim"));
    assert!(kim_target.census.file_count > 0);
    assert_eq!(kim_target.discovery_row(true).sessions, Some(1));
    // The pipeline row never carries the label or provenance.
    let row = kim_target.enabled_runtime();
    assert!(row.enabled && row.label.is_none() && row.provenance.is_discovered());

    // An enrolled root that is still empty is not "installed" but is active:
    // it is watched so the persona's first session appears live.
    let empty_target = &targets[3];
    assert!(empty_target.is_enrolled() && !empty_target.installed() && empty_target.active());
    assert_eq!(empty_target.discovery_row(true).sessions, Some(0));

    // `enabled_kinds`: the installed native claude-code consents; codex is
    // not installed and the unknown kind is unknown — both warnings.
    let (ids, warnings) = enrollment.enabled_discovered(&targets);
    assert_eq!(ids, BTreeSet::from([native_target.id().to_string()]));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].contains("codex is not installed"),
        "{}",
        warnings[0]
    );
    assert!(warnings[1].contains("not-an-agent"), "{}", warnings[1]);

    // The roll call: native layout roots only, never the enrolled ones.
    let board: Vec<_> = targets
        .iter()
        .filter(|target| !target.is_enrolled())
        .map(|target| target.discovery_row(ids.contains(target.id())))
        .collect();
    assert_eq!(board.len(), 2);
    assert!(board.iter().all(|row| row.data_root.starts_with("~/")));
    assert!(board[0].enabled && board[0].installed);
    assert_eq!(board[0].sessions, Some(1));
    assert!(!board[1].enabled && !board[1].installed);
    assert_eq!(board[1].sessions, Some(0));
    assert!(!board.iter().any(|row| row.data_root.contains("agents")));

    let by_kind = discovered_by_kind(&targets);
    assert_eq!(by_kind.len(), 2);
    assert_eq!(by_kind["claude-code"].id(), native_target.id());
}
