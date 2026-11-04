//! One writer item of a streamed runtime and the transactions it is applied
//! in (R1 brief §4.5).
//!
//! An item is either some small closed sources coalesced into one
//! transaction, or one source whose budget bytes exceed the chunk bound.
//! The writer steps an item one transaction per turn:
//!
//! 1. **Begin** (no transaction): validate, stamp each op's cursor, resolve
//!    the sessions the item completes from the facts the preparer withheld
//!    (R1 D22), bind resolved and released facts to their cursors, and plan
//!    the slices.
//! 2. **Stage slices** of the source's immediate ops, cut greedily by
//!    [`IngestOp::approx_bytes`]: a slice closes before an op that would
//!    push it past the chunk bound and always keeps one op.
//! 3. **Resolved-row slices** of the sessions it completes, while what is
//!    left would push the terminal past the bound.
//! 4. **The terminal**: the last op slice, the checkpoint CAS, Activate,
//!    the last resolved rows and the session touches.
//!
//! A coalesced item and an Abandon item are one terminal transaction: an
//! Abandon applies drift only, exactly as a whole-source Abandon. Each step
//! returns the budget bytes to release: a slice its share of the item's
//! bytes (by approx bytes), the terminal whatever the item still holds, so
//! the item releases exactly what it acquired.

use std::collections::VecDeque;

use rosetta_core::IngestOp;
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, StreamedSource};

use crate::early::ClosedTree;
use crate::live::PollEvidence;

/// What a step left of its item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// More transactions follow.
    More,
    /// The terminal committed.
    Done,
}

/// Which transaction a step ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxKind {
    Stage,
    ResolvedRows,
    Terminal,
}

/// What the last step of an item wrote, for the writer's profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepReport {
    pub kind: TxKind,
    /// Approx bytes the transaction wrote: its ops' (assets included) and
    /// its resolved rows'.
    pub approx_bytes: u64,
    /// Of `approx_bytes`, its `ProjectionAsset` ops' (counted only when the
    /// runtime measures steps, else zero) and its resolved tool rows'; the
    /// rest is its other immediate ops' (measurement only).
    pub asset_bytes: u64,
    pub resolved_bytes: u64,
    /// The sources it wrote for: one for a slice, each of a terminal's.
    pub sources: u32,
    pub wall_nanos: u64,
}

impl StepReport {
    /// Approx bytes of its immediate ops other than assets.
    pub fn immediate_bytes(&self) -> u64 {
        self.approx_bytes
            .saturating_sub(self.asset_bytes)
            .saturating_sub(self.resolved_bytes)
    }
}

/// What a source's terminal commit settles on the runtime.
#[derive(Debug)]
pub(super) struct Settled {
    pub(super) path: String,
    pub(super) session_id: String,
    pub(super) complete: bool,
    pub(super) evidence: PollEvidence,
    pub(super) closed_trees: Vec<ClosedTree>,
}

/// One writer item and its progress: coalesced sources, or one source
/// stepped in slices.
#[derive(Debug)]
pub struct ApplyItem {
    /// The terminal transaction's sources.
    pub(super) terminal: Vec<StreamedSource>,
    pub(super) settled: Vec<Settled>,
    /// Stage slices not applied yet, with their approx bytes.
    pub(super) stage: VecDeque<(StreamedSource, u64)>,
    /// Approx bytes of the terminal's own ops.
    pub(super) terminal_bytes: u64,
    /// Resolved-row slices are cut at this bound; `u64::MAX` for none.
    pub(super) chunk_bytes: u64,
    /// Budget bytes the item acquired, and released so far.
    bytes: u64,
    released: u64,
    /// Approx bytes of everything the item writes.
    approx_total: u64,
    /// Transactions committed so far.
    pub(super) txs: u32,
    pub(super) last: Option<StepReport>,
}

impl ApplyItem {
    pub(super) fn new(
        terminal: Vec<StreamedSource>,
        settled: Vec<Settled>,
        stage: VecDeque<(StreamedSource, u64)>,
        terminal_bytes: u64,
        chunk_bytes: u64,
        bytes: u64,
    ) -> Self {
        let resolved = resolved_bytes(&terminal);
        let approx_total = stage
            .iter()
            .map(|(_, bytes)| *bytes)
            .fold(terminal_bytes, u64::saturating_add)
            .saturating_add(resolved);
        Self {
            terminal,
            settled,
            stage,
            terminal_bytes,
            chunk_bytes,
            bytes,
            released: 0,
            approx_total,
            txs: 0,
            last: None,
        }
    }

    /// Budget bytes the item acquired.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Whether the item has committed a transaction and has more to go.
    pub fn in_progress(&self) -> bool {
        self.txs > 0
    }

    /// Transactions committed so far.
    pub fn txs(&self) -> u32 {
        self.txs
    }

    /// What the last committed step wrote.
    pub fn last_step(&self) -> Option<StepReport> {
        self.last
    }

    /// Measurement only: approx bytes of the `ProjectionAsset` ops the next
    /// step writes — its Stage slice's, none for a resolved-row slice, else
    /// the terminal's sources'.
    pub(super) fn next_asset_bytes(&self) -> u64 {
        if let Some((slice, _)) = self.stage.front() {
            asset_bytes(&slice.batch.ops)
        } else if self.needs_resolved_slice() {
            0
        } else {
            self.terminal
                .iter()
                .map(|source| asset_bytes(&source.batch.ops))
                .fold(0, u64::saturating_add)
        }
    }

    /// Whether the next step is a resolved-row slice: rows are left and the
    /// terminal would otherwise write more than the chunk bound.
    pub(super) fn needs_resolved_slice(&self) -> bool {
        self.chunk_bytes != u64::MAX
            && self.stage.is_empty()
            && resolved_bytes(&self.terminal).saturating_add(self.terminal_bytes) > self.chunk_bytes
            && self
                .terminal
                .iter()
                .any(|source| source.resolved.iter().any(|session| session.rows_left()))
    }

    /// The bytes a non-last step of `approx` approx bytes releases: its
    /// share of the item's bytes, never more than the item still holds.
    pub(super) fn release_share(&mut self, approx: u64) -> u64 {
        let share = if self.approx_total == 0 {
            0
        } else {
            u64::try_from(
                u128::from(self.bytes) * u128::from(approx) / u128::from(self.approx_total),
            )
            .unwrap_or(u64::MAX)
        };
        let share = share.min(self.bytes - self.released);
        self.released += share;
        share
    }

    /// The terminal releases whatever the item still holds.
    pub(super) fn release_rest(&mut self) -> u64 {
        let rest = self.bytes - self.released;
        self.released = self.bytes;
        rest
    }
}

/// Bytes of the resolved rows the terminal's sources have not written yet.
pub(super) fn resolved_bytes(sources: &[StreamedSource]) -> u64 {
    sources
        .iter()
        .flat_map(|source| source.resolved.iter())
        .map(|session| session.rows_bytes())
        .fold(0, u64::saturating_add)
}

/// Σ approx bytes of the `ProjectionAsset` ops among `ops`.
fn asset_bytes(ops: &[IngestOp]) -> u64 {
    ops.iter()
        .filter(|op| matches!(op, IngestOp::ProjectionAsset { .. }))
        .map(IngestOp::approx_bytes)
        .fold(0, u64::saturating_add)
}

/// Σ approx bytes of `ops`.
pub(super) fn ops_bytes(ops: &[IngestOp]) -> u64 {
    ops.iter()
        .map(IngestOp::approx_bytes)
        .fold(0, u64::saturating_add)
}

/// Cut `ops` greedily into slices of at most `chunk_bytes` approx bytes: a
/// slice closes before an op that would push it past the bound, and always
/// keeps at least one op, so a slice is over the bound only when one op
/// alone is. Returns each slice with its approx bytes.
pub fn cut_slices(ops: Vec<IngestOp>, chunk_bytes: u64) -> Vec<(Vec<IngestOp>, u64)> {
    let mut slices = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes = 0_u64;
    for op in ops {
        let bytes = op.approx_bytes();
        if !current.is_empty() && current_bytes.saturating_add(bytes) > chunk_bytes {
            slices.push((std::mem::take(&mut current), current_bytes));
            current_bytes = 0;
        }
        current_bytes = current_bytes.saturating_add(bytes);
        current.push(op);
    }
    if !current.is_empty() || slices.is_empty() {
        slices.push((current, current_bytes));
    }
    slices
}

/// The batches of one chunked source: its Stage slices, then its terminal.
/// `terminal` is the source's whole terminal (all ops, checkpoint, terminal
/// mode); its ops are cut, every slice but the last becomes a Stage batch at
/// its own first sequence, and the last stays with the terminal. When the
/// item's resolved rows will be written in slices before the terminal, the
/// generation is opened by a Stage slice first — even one holding every op
/// — so the terminal's first-generation sweep can never meet a row a
/// resolved-row slice already wrote under this cursor.
pub(super) fn plan_stage(
    terminal: &mut StreamedSource,
    chunk_bytes: u64,
    resolved_slices: bool,
) -> (VecDeque<(StreamedSource, u64)>, u64) {
    let Some(source) = terminal.batch.source.clone() else {
        let bytes = ops_bytes(&terminal.batch.ops);
        return (VecDeque::new(), bytes);
    };
    let mut slices = cut_slices(std::mem::take(&mut terminal.batch.ops), chunk_bytes);
    let (last_ops, last_bytes) = if resolved_slices && slices.len() == 1 {
        (Vec::new(), 0)
    } else {
        slices.pop().expect("a cut keeps at least one slice")
    };
    let mut stage = VecDeque::with_capacity(slices.len());
    let mut sequence = source.first_sequence;
    for (ops, bytes) in slices {
        let count = u64::try_from(ops.len()).unwrap_or(u64::MAX);
        stage.push_back((
            StreamedSource {
                batch: Batch {
                    ops,
                    checkpoints: Vec::new(),
                    source: Some(SourceWrite {
                        mode: SourceMode::Stage,
                        first_sequence: sequence,
                        ..source.clone()
                    }),
                },
                withheld: terminal.withheld,
                resolved: Vec::new(),
                released: Vec::new(),
            },
            bytes,
        ));
        sequence = sequence.saturating_add(count);
    }
    terminal.batch.ops = last_ops;
    if let Some(write) = &mut terminal.batch.source {
        write.first_sequence = sequence;
    }
    (stage, last_bytes)
}

/// A terminal source batch: the source's ops, its checkpoint CAS from the
/// fresh `initial` checkpoint to its final one, and its terminal mode.
#[allow(clippy::too_many_arguments)]
pub(super) fn terminal_batch(
    runtime_id: &str,
    source: &rosetta_capture::SourceDef,
    cursor_id: i64,
    initial: String,
    checkpoint: String,
    generation: u64,
    activate: bool,
    ops: Vec<IngestOp>,
) -> Batch {
    Batch {
        ops,
        checkpoints: vec![CheckpointUpdate {
            runtime_id: runtime_id.to_owned(),
            session_id: source.projected_session_id(),
            stream: source.stream_id.clone(),
            path: source.relative_path.clone(),
            expected: Some(initial),
            next: checkpoint,
            updated_at: None,
        }],
        source: Some(SourceWrite {
            cursor_id,
            session_id: source.session_id.clone(),
            generation,
            first_sequence: 0,
            mode: if activate {
                SourceMode::Activate
            } else {
                SourceMode::Abandon
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use rosetta_core::rows::{MessageKind, MessageRow};

    use super::*;

    fn text(len: usize) -> IngestOp {
        IngestOp::project_message(MessageRow {
            timeline_id: format!("t{len}"),
            session_id: "s".into(),
            ordinal: 0,
            kind: MessageKind::User,
            subtype: None,
            uuid: None,
            parent_uuid: None,
            ts: Some(1),
            model: None,
            effort: None,
            content: Some("x".repeat(len)),
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
        .unwrap()
    }

    /// T6 test 13(a): clustered large ops never push a slice past the bound
    /// by more than one op, and the cut keeps every op once, in order.
    #[test]
    fn cut_points_follow_measured_op_bytes() {
        // Small ops, then a cluster of large ones, then small again: a
        // per-op average (`payload / len(ops)`) would put whole clusters in
        // one slice.
        let mut ops = Vec::new();
        for _ in 0..200 {
            ops.push(text(100));
        }
        for _ in 0..12 {
            ops.push(text(30_000));
        }
        for _ in 0..200 {
            ops.push(text(100));
        }
        let bound = 64 * 1024;
        let largest = ops.iter().map(IngestOp::approx_bytes).max().unwrap();
        let total = ops.len();
        let slices = cut_slices(ops, bound);
        assert!(slices.len() >= 6, "{} slices", slices.len());
        for (slice, bytes) in &slices {
            assert!(!slice.is_empty());
            assert_eq!(*bytes, ops_bytes(slice));
            assert!(*bytes <= bound + largest, "{bytes} > {bound} + one op");
            if slice.len() > 1 {
                assert!(*bytes <= bound, "a multi-op slice stays within the bound");
            }
        }
        assert_eq!(
            slices.iter().map(|(ops, _)| ops.len()).sum::<usize>(),
            total
        );
    }

    #[test]
    fn one_op_over_the_bound_is_its_own_slice() {
        let slices = cut_slices(vec![text(10), text(5_000), text(10)], 1_000);
        let lengths: Vec<usize> = slices.iter().map(|(ops, _)| ops.len()).collect();
        assert_eq!(lengths, [1, 1, 1]);
        assert_eq!(cut_slices(Vec::new(), 1_000).len(), 1);
    }

    /// Measurement only: the next step's asset bytes are its Stage slice's
    /// while slices are left, then every terminal source's together.
    #[test]
    fn the_next_steps_asset_bytes_follow_the_step_order() {
        let asset = |len| IngestOp::ProjectionAsset {
            asset: rosetta_core::ProjectionAsset::new("image/png", vec![7; len]).unwrap(),
        };
        let source = |ops| {
            StreamedSource::from(Batch {
                ops,
                ..Batch::default()
            })
        };
        let (staged, last, other) = (asset(300), asset(500), asset(40));
        let staged_bytes = staged.approx_bytes();
        let terminal_bytes = last.approx_bytes() + other.approx_bytes();
        assert!(staged_bytes >= 300 && terminal_bytes >= 540);
        let stage = VecDeque::from([(source(vec![text(10), staged, text(20)]), 0)]);
        let terminal = vec![source(vec![last, text(30)]), source(vec![other])];
        let mut item = ApplyItem::new(terminal, Vec::new(), stage, 0, u64::MAX, 0);
        assert_eq!(item.next_asset_bytes(), staged_bytes);
        item.stage.pop_front();
        assert_eq!(item.next_asset_bytes(), terminal_bytes);
        let plain = ApplyItem::new(
            vec![source(vec![text(5)])],
            Vec::new(),
            VecDeque::new(),
            0,
            u64::MAX,
            0,
        );
        assert_eq!(plain.next_asset_bytes(), 0);
    }
}
