//! The pool's estimate of what is left to read stops at each source's
//! metadata cut, as a root-tree pass does.

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{FileDiscipline, FileFormat, StreamDriver};
use rosetta_capture::TailConfig;

use super::*;

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: "runtime-cut".into(),
        kind: "claude-code".into(),
        data_root: "~/.claude".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 0,
        label: None,
        provenance: Default::default(),
    }
}

fn unit(index: u32, size: u64, cut_len: Option<u64>) -> UnitSpec {
    UnitSpec {
        file: SessionFile {
            input: None,
            ledger_only: false,
            relative_path: format!("projects/-w-cut/s{index}.jsonl"),
            stream_id: "transcript".into(),
            file_format: FileFormat::Jsonl,
            discipline: FileDiscipline::AppendOnly,
            driver: StreamDriver::JsonLines,
            session_id: format!("session-{index}"),
            native_session_id: format!("s{index}"),
            size,
            modified_ms: 1,
            agent_version: None,
            source_missing: false,
            tree_parent: None,
            cut_len,
            cut_modified_ms: cut_len.map(|_| 2),
        },
        cursor: CursorState {
            id: i64::from(index) + 1,
            checkpoint: String::new(),
            active_generation: 0,
            highest_generation: 0,
            session_id: Some(format!("session-{index}")),
        },
        key: index,
    }
}

#[test]
fn pool_remaining_uses_the_cut() {
    // No workers: nothing steps, so the registration's estimate stays put.
    let pool = CapturePool::new(1, PoolBudgets::DEFAULT);
    let units = vec![
        unit(0, 1_000, Some(400)),
        unit(1, 64, Some(64)),
        // A source without a cut (a document member would have none) is
        // estimated at its census size.
        unit(2, 300, None),
    ];
    let expected: Vec<u64> = units.iter().map(|unit| unit.file.read_len()).collect();
    assert_eq!(expected, [400, 64, 300]);
    let lane = pool.register_lane(
        Shared {
            adapter: &ClaudeCodeAdapter,
            runtime: runtime(),
            root: std::env::temp_dir(),
            opts: IngestOptions::default(),
            tailer: Tailer::new(TailConfig::json_lines()).unwrap(),
            frames: false,
            compress: None,
            units,
        },
        LaneClass::Shared,
    );
    {
        let state = pool.lock();
        let registered = state.runtimes[0].as_ref().expect("registered");
        assert_eq!(registered.remaining, expected.iter().sum::<u64>());
        let unread: Vec<u64> = registered
            .units
            .iter()
            .map(|source| source.unread)
            .collect();
        assert_eq!(unread, expected);
    }

    // Moving past the head retires exactly its cut-bounded estimate.
    lane.advance(0);
    let state = pool.lock();
    let registered = state.runtimes[0].as_ref().expect("registered");
    assert_eq!(registered.remaining, 64 + 300);
    assert_eq!(registered.units[0].unread, 0);
    // The lane withdraws its runtime under the same lock.
    drop(state);
    drop(lane);
}
