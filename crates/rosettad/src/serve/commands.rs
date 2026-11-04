//! Full commands read back from the raw ledger, wherever it lives right now.
//!
//! A fresh build captures into a named staged generation and renames it onto
//! the public ledger root only at publication. Its trees become readable long
//! before that, so a command a reader opens during the build is read from the
//! staged root. The rename can land under a read in flight; such a read is
//! retried once at the public root, which from then on is the only root.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use rosetta_pipeline::{RawCommandLocator, read_raw_command};
use rosetta_server::RawCommandReader;
use rosetta_store::queries;

use crate::runtime_ledger::ledger_dir_for;

pub(crate) struct LedgerCommandReader {
    roots: LedgerRoots,
    adapters: Vec<&'static dyn rosetta_adapter_support::AgentAdapter>,
}

impl LedgerCommandReader {
    pub(crate) fn new(
        public: PathBuf,
        adapters: Vec<&'static dyn rosetta_adapter_support::AgentAdapter>,
    ) -> Self {
        Self {
            roots: LedgerRoots::new(public),
            adapters,
        }
    }

    /// The cold stage opened at `root`: until it is renamed onto the public
    /// root, commands are read there.
    pub(crate) fn stage(&self, root: &Path) {
        self.roots.stage(root);
    }
}

impl RawCommandReader for LedgerCommandReader {
    fn read_command(&self, source: &queries::CommandSource) -> Result<Option<String>, String> {
        let adapter = self
            .adapters
            .iter()
            .copied()
            .find(|adapter| adapter.kind() == source.agent_kind)
            .ok_or_else(|| format!("no adapter for {}", source.agent_kind))?;
        let locator = RawCommandLocator {
            runtime_id: source.runtime_id.clone(),
            session_id: source.session_id.clone(),
            cursor_id: source.cursor_id,
            relative_path: source.relative_path.clone(),
            generation: source.generation,
            offset: source.offset,
            timeline_id: source.timeline_id.clone(),
            correlation_key: source.correlation_key.clone(),
        };
        // The runtime's ledger directory follows its identity root: the
        // per-kind directory for a discovered runtime, `<kind>~<digest>` for
        // a supervisor-enrolled one.
        self.roots.read(|root| {
            let dir = ledger_dir_for(root, &source.agent_kind, &source.data_root);
            read_raw_command(adapter, &dir, &locator).map_err(|error| error.to_string())
        })
    }
}

/// Where the raw ledger is: the staged generation while a fresh build owns
/// it, the public root otherwise.
pub(crate) struct LedgerRoots {
    staged: RwLock<Option<PathBuf>>,
    public: PathBuf,
}

impl LedgerRoots {
    pub(crate) fn new(public: PathBuf) -> Self {
        Self {
            staged: RwLock::new(None),
            public,
        }
    }

    pub(crate) fn stage(&self, root: &Path) {
        *self.staged.write().expect("ledger roots poisoned") = Some(root.to_path_buf());
    }

    /// Run `read` against the current root. A staged read that fails, or finds
    /// nothing, after the stage was renamed away is retried once at the
    /// public root, and the staged root is forgotten: publication renames it
    /// exactly once, onto `public`.
    pub(crate) fn read<T>(
        &self,
        read: impl Fn(&Path) -> Result<Option<T>, String>,
    ) -> Result<Option<T>, String> {
        let staged = self
            .staged
            .read()
            .expect("ledger roots poisoned")
            .clone()
            .filter(|staged| *staged != self.public);
        let Some(staged) = staged else {
            return read(&self.public);
        };
        let answer = read(&staged);
        if matches!(answer, Ok(Some(_))) || staged.exists() {
            return answer;
        }
        self.forget(&staged);
        read(&self.public)
    }

    fn forget(&self, staged: &Path) {
        let mut current = self.staged.write().expect("ledger roots poisoned");
        if current.as_deref() == Some(staged) {
            *current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ledger stand-in: listing a missing root fails, a missing record in
    /// a present root is an honest "not yet".
    fn record(dir: &Path) -> Result<Option<String>, String> {
        if !dir.exists() {
            return Err(format!("listing {}: not found", dir.display()));
        }
        match std::fs::read_to_string(dir.join("record")) {
            Ok(body) => Ok(Some(body)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    #[test]
    fn a_staged_build_reads_the_stage_then_the_public_root_after_the_rename() {
        let dir = tempfile::TempDir::new().unwrap();
        let public = dir.path().join("ledger");
        let staged = dir.path().join("ledger.staged");
        let roots = LedgerRoots::new(public.clone());

        // No stage yet: the public root (absent on a fresh start) answers.
        assert!(roots.read(record).is_err());

        std::fs::create_dir(&staged).unwrap();
        roots.stage(&staged);
        // Admitted but not yet flushed: an honest "nothing yet" from the stage.
        assert_eq!(roots.read(record), Ok(None));
        std::fs::write(staged.join("record"), "staged").unwrap();
        assert_eq!(roots.read(record), Ok(Some("staged".into())));

        // Publication renames the stage onto the public root.
        std::fs::rename(&staged, &public).unwrap();
        assert_eq!(roots.read(record), Ok(Some("staged".into())));
        assert!(roots.staged.read().unwrap().is_none(), "stage forgotten");
        assert_eq!(roots.read(record), Ok(Some("staged".into())));
    }

    #[test]
    fn a_read_racing_the_rename_retries_the_public_root_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let public = dir.path().join("ledger");
        let staged = dir.path().join("ledger.staged");
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("record"), "body").unwrap();
        let roots = LedgerRoots::new(public.clone());
        roots.stage(&staged);
        let calls = std::sync::Mutex::new(Vec::new());
        let answer = roots.read(|root| {
            calls.lock().unwrap().push(root.to_path_buf());
            if root == staged {
                // The rename lands while this read walks the stage.
                std::fs::rename(&staged, &public).unwrap();
                return Err("segment vanished".into());
            }
            record(root)
        });
        assert_eq!(answer, Ok(Some("body".into())));
        assert_eq!(*calls.lock().unwrap(), vec![staged.clone(), public.clone()]);
    }

    #[test]
    fn a_stage_already_at_the_public_root_reads_it_directly() {
        let dir = tempfile::TempDir::new().unwrap();
        let public = dir.path().join("ledger");
        std::fs::create_dir(&public).unwrap();
        std::fs::write(public.join("record"), "body").unwrap();
        let roots = LedgerRoots::new(public.clone());
        // A resumed generation whose ledger was already renamed before a crash.
        roots.stage(&public);
        assert_eq!(roots.read(record), Ok(Some("body".into())));
    }

    /// The real reader over a real Claude ledger: the staged root during the
    /// build, nothing yet before its frame lands, the public root after the
    /// publish rename.
    #[test]
    fn a_full_command_follows_its_ledger_from_the_stage_to_the_public_root() {
        use rosetta_adapter_claude_code::ClaudeCodeAdapter;
        use rosetta_adapter_support::{AgentAdapter, NormalizeCtx};
        use rosetta_capture::{Ledger, SourceDef};
        use rosetta_core::{IngestOp, TimelineBody};

        let full = "cargo build\ncargo test\ncargo clippy";
        let line = serde_json::json!({
            "type": "assistant",
            "parentUuid": null,
            "isSidechain": false,
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-5",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "Bash",
                    "input": {"command": full}
                }],
                "stop_reason": "tool_use",
                "stop_sequence": null
            },
            "uuid": "11111111-1111-1111-1111-111111111111",
            "timestamp": "2026-08-25T02:35:19.072Z",
            "sessionId": "native-session",
            "version": "2.1.241",
            "cwd": "/tmp/project",
            "gitBranch": "main",
            "userType": "external"
        })
        .to_string();
        let adapter = ClaudeCodeAdapter;
        let mut context = NormalizeCtx::new("rt1", "s1");
        context.native_session_id = Some("native-session".into());
        context.state = adapter.init_state();
        context.src_cursor = Some(7);
        context.src_offset = Some(0);
        context.source_identity = Some("projects/session.jsonl".into());
        context.source_generation = 1;
        let (timeline_id, correlation_key) = adapter
            .ingest_line(&mut context, "transcript", line.as_bytes())
            .ops
            .iter()
            .find_map(|op| match op {
                IngestOp::Message(entry) => match &entry.body {
                    TimelineBody::ToolUse { tool } => {
                        Some((entry.timeline_id.clone(), tool.correlation.key.clone()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .expect("a Claude command row");
        let source = queries::CommandSource {
            cursor_id: 7,
            runtime_id: "rt1".into(),
            agent_kind: "claude-code".into(),
            data_root: "~/.claude".into(),
            relative_path: "projects/session.jsonl".into(),
            session_id: "s1".into(),
            generation: 1,
            offset: 0,
            timeline_id,
            correlation_key,
        };

        let dir = tempfile::TempDir::new().unwrap();
        let public = dir.path().join("ledger");
        let staged = dir.path().join("ledger.staged");
        let runtime_dir = ledger_dir_for(&staged, "claude-code", "~/.claude");
        let mut ledger = Ledger::open(&runtime_dir).unwrap();
        let reader = LedgerCommandReader::new(public.clone(), crate::registry());
        reader.stage(&staged);
        assert_eq!(reader.read_command(&source), Ok(None), "not saved yet");

        ledger
            .append_raw(
                &SourceDef {
                    ledger_only: false,
                    relative_path: "projects/session.jsonl".into(),
                    stream_id: "transcript".into(),
                    session_id: "s1".into(),
                    native_session_id: "native-session".into(),
                    agent_version: Some("2.1.241".into()),
                },
                1,
                [(0, line.as_bytes())],
            )
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);
        assert_eq!(reader.read_command(&source), Ok(Some(full.into())));

        std::fs::rename(&staged, &public).unwrap();
        assert_eq!(reader.read_command(&source), Ok(Some(full.into())));
        assert!(reader.roots.staged.read().unwrap().is_none());
    }
}
