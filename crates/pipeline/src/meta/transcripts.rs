//! Plan logical transcripts from bounded native headers and DB selectors.
//! Plans are transient. Their selected header and inherited raw records are
//! captured together in one source generation, making offline replay closed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rosetta_adapter_support::AgentAdapter;
use rosetta_adapter_support::transcripts::TranscriptHeader;
use rosetta_capture::input::{InputSegment, InputSource};
use rosetta_core::rows::RuntimeRow;
use rosetta_store::{Store, queries};

use super::{MetaOutput, MetaReport, SessionFile};
use crate::{IngestOptions, PipelineError};

struct Node {
    file: SessionFile,
    header: TranscriptHeader,
    header_len: u64,
    header_hash: [u8; 32],
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    store: &Store,
    inventory: &[rosetta_capture::ScannedFile],
    opts: &IngestOptions,
    report: &mut MetaReport,
    output: &mut MetaOutput,
) -> Result<(), PipelineError> {
    let candidates: BTreeMap<_, _> = inventory
        .iter()
        .filter_map(|file| {
            adapter
                .transcript_location(&file.relative_path)
                .map(|l| (l.logical_path, l.source))
        })
        .collect();
    if candidates.is_empty() && output.selections.is_empty() {
        return Ok(());
    }
    let cursors = queries::cursor_sources_with_prefix(store.conn(), &runtime.id, "")?;
    for (path, stream, session) in &cursors {
        if let Some(input) = candidates.get(path)
            && !output.files.iter().any(|file| file.relative_path == *path)
            && let Some(row) = queries::session(store.conn(), session)?
        {
            let facts = inventory.iter().find(|file| {
                adapter
                    .transcript_location(&file.relative_path)
                    .is_some_and(|location| location.logical_path == *path)
            });
            output.files.push(SessionFile {
                input: Some(input.clone()),
                ledger_only: false,
                relative_path: path.clone(),
                stream_id: stream.clone(),
                file_format: rosetta_adapter_support::FileFormat::Jsonl,
                discipline: rosetta_adapter_support::FileDiscipline::AppendOnly,
                driver: rosetta_adapter_support::StreamDriver::JsonLines,
                session_id: session.clone(),
                native_session_id: row.native_session_id,
                size: facts.map_or(0, |file| file.size),
                modified_ms: facts.map_or(row.updated_at, |file| file.modified_ms),
                agent_version: None,
                source_missing: false,
                tree_parent: None,
                cut_len: None,
                cut_modified_ms: None,
            });
        }
    }
    let mut nodes = BTreeMap::new();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in &output.files {
        opts.check_cancelled()?;
        let Some(input) = &file.input else {
            continue;
        };
        let head = match input.head(root, crate::MAX_PROJECTED_RECORD_BYTES) {
            Ok(Some(head)) => head,
            Ok(None) => {
                groups.entry(file.native_session_id.clone()).or_default();
                continue;
            }
            Err(error) if error.incomplete_input() => {
                groups.entry(file.native_session_id.clone()).or_default();
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let header = match adapter.transcript_header(&file.relative_path, &head.bytes) {
            Ok(Some(header)) => header,
            Ok(None) => continue,
            Err(_) => {
                groups.entry(file.native_session_id.clone()).or_default();
                report.files_unidentified += 1;
                continue;
            }
        };
        groups
            .entry(header.session_id.clone())
            .or_default()
            .push(header.source_id.clone());
        if nodes
            .insert(
                header.source_id.clone(),
                Node {
                    file: file.clone(),
                    header,
                    header_len: head.bytes.len() as u64,
                    header_hash: *blake3::hash(&head.bytes).as_bytes(),
                },
            )
            .is_some()
        {
            return Err(PipelineError::Config(
                "ambiguous immutable transcript identity".into(),
            ));
        }
    }
    // Reuse an older plain-path cursor, including after its native file was
    // archived or removed. This keeps upgrades and later reverts in one
    // logical source rather than merging abandoned branches into a session.
    let mut retained = Vec::new();
    let mut handled = BTreeSet::new();
    for (native, selection) in &output.selections {
        groups.entry(native.clone()).or_default();
        let _ = selection;
    }
    for (native, ids) in &groups {
        opts.check_cancelled()?;
        let selected = if let Some(selection) = output.selections.get(native) {
            let path = Path::new(&selection.path);
            let relative = if path.is_absolute() {
                path.strip_prefix(root).ok()
            } else {
                Some(path)
            };
            relative
                .and_then(Path::to_str)
                .and_then(|path| adapter.transcript_location(path))
                .and_then(|location| {
                    ids.iter().find_map(|id| {
                        let node = &nodes[id];
                        (node.file.relative_path == location.logical_path).then_some(node)
                    })
                })
        } else if ids.len() == 1 {
            nodes.get(&ids[0])
        } else {
            // Multiple branches without an authoritative selector are
            // ambiguous. Never guess "newest" and expose discarded history.
            None
        };
        let template = selected
            .map(|n| &n.file)
            .or_else(|| ids.first().map(|id| &nodes[id].file))
            .or_else(|| output.files.iter().find(|f| f.native_session_id == *native));
        let Some(template) = template else {
            continue;
        };
        let mut file = template.clone();
        handled.insert(native.clone());
        file.driver = rosetta_adapter_support::StreamDriver::JsonLines;
        file.file_format = rosetta_adapter_support::FileFormat::Jsonl;
        file.discipline = rosetta_adapter_support::FileDiscipline::AppendOnly;
        // A missing selected source still contributes an incomplete transcript
        // obligation, so its metadata-only DB member cannot mark it Indexed.
        if file.input.is_none()
            && let Some(selection) = output.selections.get(native)
        {
            let path = Path::new(&selection.path);
            if let Some(location) = path
                .strip_prefix(root)
                .ok()
                .or(Some(path))
                .and_then(Path::to_str)
                .and_then(|p| adapter.transcript_location(p))
            {
                file.relative_path = location.logical_path;
                if let Some(stream) = adapter.layout().matching_stream(&file.relative_path) {
                    file.stream_id = stream.id.clone();
                }
            } else {
                file.relative_path = format!("missing-transcript/{native}");
            }
        }
        let existing: Vec<_> = cursors
            .iter()
            .filter(|(path, _, session)| {
                session == &file.session_id && adapter.transcript_location(path).is_some()
            })
            .collect();
        file.relative_path = existing
            .first()
            .map(|(path, _, _)| path.clone())
            .unwrap_or_else(|| {
                ids.iter()
                    .map(|id| &nodes[id].file.relative_path)
                    .min()
                    .cloned()
                    .unwrap_or_else(|| file.relative_path.clone())
            });
        file.input = Some(match selected {
            Some(node) => compose(node, &nodes, root).unwrap_or_else(|_| InputSource {
                prefix: Vec::new(),
                segments: vec![],
            }),
            None => InputSource {
                prefix: Vec::new(),
                segments: vec![],
            },
        });
        if selected.is_some_and(|node| node.header.base.is_some()) {
            // This is a new virtual byte stream; its ancestor cuts are
            // explicit and its own file is pinned when capture starts.
            file.cut_len = None;
        }
        // Pre-selection importers could have projected several branches as
        // independent sources. Keep those bounded compatibility cursors on
        // the SAME selected view, so each replaces its abandoned contribution
        // and offline replay cannot resurrect it. No new alias is created;
        // fresh imports still have exactly one logical transcript per thread.
        // Mirroring later changes is essential: merely fixing the aliases
        // once would leave their old selected view alive after another revert.
        for (path, stream, _) in existing.into_iter().skip(1) {
            let mut alias = file.clone();
            alias.relative_path = path.clone();
            alias.stream_id = stream.clone();
            retained.push(alias);
        }
        retained.push(file);
    }
    output
        .files
        .retain(|file| file.input.is_none() || !handled.contains(&file.native_session_id));
    output.files.extend(retained);
    Ok(())
}

fn segment(node: &Node, start: u64, end: Option<u64>) -> InputSegment {
    let mut segment = node.file.input.as_ref().expect("decoded source").segments[0].clone();
    segment.start = start;
    segment.end = end;
    segment.header_hash = Some(node.header_hash);
    segment
}

fn compose(
    selected: &Node,
    nodes: &BTreeMap<String, Node>,
    root: &Path,
) -> Result<InputSource, ()> {
    if selected.header.base.is_none() {
        return Ok(InputSource {
            prefix: Vec::new(),
            segments: vec![segment(selected, 0, None)],
        });
    }
    let mut seen = BTreeSet::new();
    let mut chain = Vec::new();
    let mut node = selected;
    let mut end = None;
    let mut end_record_index = None;
    loop {
        if chain.len() >= 128
            || !seen.insert(node.header.source_id.clone())
            || !node.header.paginated
        {
            return Err(());
        }
        let mut part = segment(node, 0, end);
        part.end_record_index = end_record_index;
        chain.push(part);
        let Some(base) = &node.header.base else {
            break;
        };
        if base.end_ordinal == 0
            || base.end_offset == 0
            || node.header.start_ordinal != base.end_ordinal
        {
            return Err(());
        }
        node = nodes.get(&base.source_id).ok_or(())?;
        if base.end_offset < node.header_len || base.end_ordinal <= node.header.start_ordinal {
            return Err(());
        }
        end = Some(base.end_offset);
        end_record_index = Some(base.end_ordinal);
    }
    chain.reverse();
    // The selected header configures the normalizer first. Each original
    // header then marks the owner of its following records. These are native
    // bytes, not a synthetic protocol; the ledger is self-contained.
    let prefix = selected
        .file
        .input
        .as_ref()
        .ok_or(())?
        .head(root, crate::MAX_PROJECTED_RECORD_BYTES)
        .map_err(|_| ())?
        .ok_or(())?
        .bytes;
    if *blake3::hash(&prefix).as_bytes() != selected.header_hash {
        return Err(());
    }
    Ok(InputSource {
        prefix,
        segments: chain,
    })
}
