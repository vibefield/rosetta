//! A concurrent census is the serial census: same discoveries in the same
//! order, and the first failing root in that order decides the error.

use std::path::{Path, PathBuf};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::ScannedFile;
use rosetta_pipeline::{
    CensusRoot, Discovery, IngestOptions, Instrument, discover_root, discover_roots,
    expand_data_root,
};
use tempfile::TempDir;

type Census = (
    String,
    String,
    PathBuf,
    bool,
    usize,
    u64,
    usize,
    u64,
    i64,
    bool,
    Vec<ScannedFile>,
);

/// Everything a discovery reports except the wall-clock `discovered_at`.
fn census(discovery: &Discovery) -> Census {
    (
        discovery.runtime.id.clone(),
        discovery.runtime.data_root.clone(),
        discovery.root.clone(),
        discovery.installed,
        discovery.file_count,
        discovery.total_bytes,
        discovery.streamable_count,
        discovery.streamable_bytes,
        discovery.newest_modified_ms,
        discovery.walk_truncated,
        discovery.inventory.clone(),
    )
}

fn home() -> TempDir {
    let home = TempDir::new().unwrap();
    for (adapter, agent) in [
        (&ClaudeCodeAdapter as &dyn AgentAdapter, Agent::ClaudeCode),
        (&CodexAdapter, Agent::Codex),
    ] {
        let root = expand_data_root(&adapter.layout().data_root, home.path());
        std::fs::create_dir_all(&root).unwrap();
        generate(&CorpusSpec::new(agent, 6, 20, 7), &root).unwrap();
    }
    // A layout root that holds only noise is walked but not installed.
    let copilot = expand_data_root(&CopilotAdapter.layout().data_root, home.path());
    std::fs::create_dir_all(copilot.join("cache")).unwrap();
    std::fs::write(copilot.join("cache/noise.bin"), b"x").unwrap();
    home
}

fn roots<'a>(adapters: &[&'a dyn AgentAdapter], dirs: &'a [PathBuf]) -> Vec<CensusRoot<'a>> {
    adapters
        .iter()
        .zip(dirs)
        .map(|(adapter, root)| CensusRoot {
            adapter: *adapter,
            data_root: &adapter.layout().data_root,
            root,
        })
        .collect()
}

#[test]
fn concurrent_census_equals_the_serial_census_in_order() {
    let home = home();
    let adapters: Vec<&dyn AgentAdapter> = vec![&CopilotAdapter, &ClaudeCodeAdapter, &CodexAdapter];
    // The same root twice and a missing root are both ordinary inputs.
    let mut dirs: Vec<PathBuf> = adapters
        .iter()
        .map(|adapter| expand_data_root(&adapter.layout().data_root, home.path()))
        .collect();
    let adapters = [adapters.clone(), vec![&ClaudeCodeAdapter, &CodexAdapter]].concat();
    dirs.push(dirs[1].clone());
    dirs.push(home.path().join("not-installed"));
    let roots = roots(&adapters, &dirs);
    let opts = IngestOptions::default();
    let instrument = Instrument::new();

    let serial: Vec<Census> = roots
        .iter()
        .map(|root| {
            census(
                &discover_root(root.adapter, root.data_root, root.root, &opts, &instrument)
                    .unwrap(),
            )
        })
        .collect();
    let concurrent: Vec<Census> = discover_roots(&roots, &opts, &instrument)
        .unwrap()
        .iter()
        .map(census)
        .collect();
    assert_eq!(concurrent, serial);
    assert!(
        serial[1].3 && serial[2].3,
        "generated corpora are installed"
    );
    assert!(
        !serial[0].3 && !serial[4].3,
        "noise and absence are not installs"
    );
}

#[cfg(unix)]
#[test]
fn the_first_failing_root_in_order_fails_the_census() {
    use std::os::unix::fs::PermissionsExt;

    let home = home();
    let claude = expand_data_root(&ClaudeCodeAdapter.layout().data_root, home.path());
    let codex = expand_data_root(&CodexAdapter.layout().data_root, home.path());
    // Directories each layout must enter (transcripts may live below them).
    let locked = [
        claude.join("projects/locked"),
        codex.join("sessions/locked"),
    ];
    for dir in &locked {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let enforced = std::fs::read_dir(&locked[0]).is_err();

    let adapters: Vec<&dyn AgentAdapter> = vec![&CopilotAdapter, &CodexAdapter, &ClaudeCodeAdapter];
    let dirs: Vec<PathBuf> = adapters
        .iter()
        .map(|adapter| expand_data_root(&adapter.layout().data_root, home.path()))
        .collect();
    let roots = roots(&adapters, &dirs);
    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let serial = roots.iter().try_for_each(|root| {
        discover_root(root.adapter, root.data_root, root.root, &opts, &instrument).map(|_| ())
    });
    let concurrent = discover_roots(&roots, &opts, &instrument).map(|_| ());
    for dir in &locked {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    if !enforced {
        return; // a superuser reads mode-000 directories
    }
    let serial = serial.unwrap_err().to_string();
    assert_eq!(concurrent.unwrap_err().to_string(), serial);
    assert!(serial.contains(&*path_text(&locked[1])), "{serial}");
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}
