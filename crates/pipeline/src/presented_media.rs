//! Files an agent sent the user by path: the capture seam and its replay.
//!
//! An adapter emits a pure [`rosetta_core::presented_media::PresentedMedia`]
//! fact per sent image or video; the bytes are not in the transcript. IO
//! belongs here, at capture, never in adapters or replay:
//!
//! - capture turns each fact into a `Document` ledger frame that carries a
//!   media claim, asking the ledger for an earlier capture before it reads
//!   the filesystem, or, on append growth, once the filesystem had nothing
//!   to capture ([`prior`]);
//! - one pure projection turns a captured frame plus its fact into a
//!   projection asset and the tool update that swaps the attachment's source
//!   from a path reference to that asset;
//! - replay joins frames to facts by `(source_offset, path)` and reads no
//!   filesystem.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use rosetta_adapter_support::media::{from_presented_bytes, presented_limit};
use rosetta_capture::{CapturedDocument, DocumentCaptureStatus as Status, MediaClaim};
use rosetta_core::presented_media::{PresentedMedia, RECORD_MAX_BYTES};
use rosetta_core::timeline::AttachmentSource;
use rosetta_core::{IngestOp, ToolUpdatePatch};

use crate::html_artifacts::{FileStat, now, open_regular, read_stable};

pub(crate) mod prior;
mod slots;

pub(crate) use slots::Slots;

use prior::Loaded;

/// Whether a record's ops hold a presented-media fact: what a pass checks
/// before it pays for anything else here.
pub(crate) fn has_fact(ops: &[IngestOp]) -> bool {
    ops.iter()
        .any(|op| matches!(op, IngestOp::PresentedMedia(_)))
}

fn claim(fact: &PresentedMedia) -> MediaClaim {
    MediaClaim {
        media_type: fact.media_type.clone(),
        expected_len: fact.expected_len,
    }
}

/// Pure reconstruction from ledger evidence, shared by capture and replay:
/// a captured frame plus its fact become the member and the tool update that
/// swaps the attachment's source to it. `false` pushes nothing: the frame
/// captured nothing, answers another claim or path, or its bytes are not
/// what the fact declared. Nothing pushed can fail a store batch: the fact,
/// the member and the upgraded attachment are all validated first.
pub(crate) fn project(
    fact: &PresentedMedia,
    document: &CapturedDocument,
    ops: &mut Vec<IngestOp>,
) -> bool {
    if document.status != Status::Captured
        || document.path != fact.path
        || document.media.as_ref() != Some(&claim(fact))
        || document.bytes.len() as u64 != fact.expected_len
        || fact.validate().is_err()
    {
        return false;
    }
    let Some((asset, dimensions)) = from_presented_bytes(&fact.media_type, document.bytes.clone())
    else {
        return false;
    };
    let mut upgraded = fact.attachment.clone();
    upgraded.media_type = Some(asset.media_type.clone());
    upgraded.byte_len = Some(asset.bytes.len() as u64);
    upgraded.dimensions = dimensions;
    upgraded.source = AttachmentSource::Asset {
        asset: asset.reference(),
    };
    if upgraded.validate().is_err() {
        return false;
    }
    ops.push(IngestOp::ProjectionAsset { asset });
    IngestOp::push_tool_update(
        ops,
        fact.session_id.clone(),
        fact.correlation_key.clone(),
        fact.evidence.clone(),
        ToolUpdatePatch {
            attachments: vec![upgraded],
            ..Default::default()
        },
        fact.at,
    );
    true
}

/// Replay's one entry point for a `Document` frame: an HTML frame projects
/// as it always has; a media frame joins the fact noted at its coordinate.
/// The first captured frame that projects takes the slot, so a later frame
/// at the same coordinate is ignored; a frame that captured nothing emits
/// nothing and leaves the slot for one that did; a frame no fact was noted
/// for is ignored. No read of today's source file.
pub(crate) fn project_document(
    session_id: &str,
    offset: u64,
    document: &CapturedDocument,
    slots: &mut Slots,
    ops: &mut Vec<IngestOp>,
) {
    if document.media.is_none() {
        crate::html_artifacts::project(session_id, offset, document, ops);
        return;
    }
    if slots
        .get(offset, &document.path)
        .is_some_and(|fact| project(fact, document, ops))
    {
        slots.remove(offset, &document.path);
    }
}

/// The ledger as the second resort of a pass that reads the filesystem
/// first: asked only for a file that could not be captured (missing,
/// changed, unsupported), and walked at most once, at the first such file.
/// `loaded` is where the pass keeps the answer, so later records of the
/// pass reuse it.
pub(crate) struct Fallback<'a> {
    loaded: &'a mut Option<Loaded>,
    load: &'a dyn Fn() -> Loaded,
}

impl<'a> Fallback<'a> {
    pub(crate) fn new(loaded: &'a mut Option<Loaded>, load: &'a dyn Fn() -> Loaded) -> Self {
        Self { loaded, load }
    }

    fn loaded(&mut self) -> &Loaded {
        self.loaded.get_or_insert_with(|| (self.load)())
    }
}

/// Observe the files one record's facts name and project every capture into
/// `ops`. Returns the frames to ledger at each fact's record, like the HTML
/// capture; a capture carried from the very coordinate it already sits at
/// returns none.
///
/// Per fact at or above `ledgered_through`, once per `(source_offset, path)`:
/// the ledger first (`prior`, when the pass can meet earlier frames), then
/// the claim checks, then the filesystem, then the ledger after all
/// (`fallback`, on append growth) when the file could not be captured. A
/// pass that must not observe
/// ([`crate::IngestOptions::observe_presented_media`]) does not call this,
/// so every fact stays a path reference and no frame is written.
pub(crate) fn capture(
    ops: &mut Vec<IngestOp>,
    ledgered_through: u64,
    generation: u64,
    prior: Option<&Loaded>,
    mut fallback: Option<Fallback<'_>>,
) -> Vec<(u64, CapturedDocument)> {
    let facts: Vec<PresentedMedia> = ops
        .iter()
        .filter_map(|op| match op {
            IngestOp::PresentedMedia(fact)
                if fact.source_offset >= ledgered_through && fact.validate().is_ok() =>
            {
                Some(fact.clone())
            }
            _ => None,
        })
        .collect();
    let mut documents = Vec::new();
    let mut seen = BTreeSet::new();
    // Captured bytes per raw record, so one record's files stay within
    // `RECORD_MAX_BYTES` however many it lists.
    let mut captured: BTreeMap<u64, u64> = BTreeMap::new();
    for fact in facts {
        if !seen.insert((fact.source_offset, fact.path.clone())) {
            continue;
        }
        let total = captured.entry(fact.source_offset).or_default();
        if let Some(prior) = prior
            && let Some(frame) = carry(&fact, prior, generation, *total, ops)
        {
            *total += fact.expected_len;
            documents.extend(frame.map(|document| (fact.source_offset, document)));
            continue;
        }
        let document = observe_file(&fact, *total, ops);
        if document.status == Status::Captured {
            *total += fact.expected_len;
        } else if document.status != Status::TooLarge
            && let Some(fallback) = fallback.as_mut()
            && let Some(frame) = carry(&fact, fallback.loaded(), generation, *total, ops)
        {
            // The source already holds a capture of this file: carried, it
            // stands in for the observation that found nothing to keep. A
            // claim that is too large is not looked up: no capture of it
            // could be carried either, under the same two limits.
            *total += fact.expected_len;
            documents.extend(frame.map(|document| (fact.source_offset, document)));
            continue;
        }
        if document.validate().is_ok() {
            documents.push((fact.source_offset, document));
        }
    }
    documents
}

/// Ledger first: the earlier capture of this claim, if it projects and fits
/// the record's budget. `Some(None)` is a capture already ledgered at this
/// exact coordinate: used, and nothing to append. `Some(Some(_))` is the
/// copy to append here. `None` leaves the fact to the filesystem.
fn carry(
    fact: &PresentedMedia,
    prior: &Loaded,
    generation: u64,
    total: u64,
    ops: &mut Vec<IngestOp>,
) -> Option<Option<CapturedDocument>> {
    let prior = prior.find(fact.source_offset, &fact.path, &claim(fact))?;
    if total.saturating_add(prior.document.bytes.len() as u64) > RECORD_MAX_BYTES {
        return None;
    }
    let here = prior.generation == generation && prior.offset == fact.source_offset;
    let copy = (!here).then(|| CapturedDocument {
        path: fact.path.clone(),
        captured_at: prior.document.captured_at,
        status: Status::Captured,
        recovered_from: Some(format!("ledger:{}:{}", prior.generation, prior.offset)),
        media: Some(claim(fact)),
        bytes: prior.document.bytes.clone(),
    });
    // The frame is checked before anything is projected, so a capture is
    // never shown that replay could not find again.
    if copy.as_ref().is_some_and(|copy| copy.validate().is_err())
        || !project(fact, &prior.document, ops)
    {
        return None;
    }
    Some(copy)
}

fn observation(fact: &PresentedMedia, status: Status) -> CapturedDocument {
    CapturedDocument {
        path: fact.path.clone(),
        captured_at: now(),
        status,
        recovered_from: None,
        media: Some(claim(fact)),
        bytes: Vec::new(),
    }
}

/// The claim checks, then one bounded observation of the file.
fn observe_file(fact: &PresentedMedia, total: u64, ops: &mut Vec<IngestOp>) -> CapturedDocument {
    let Some(limit) = presented_limit(&fact.media_type) else {
        return observation(fact, Status::Unsupported);
    };
    if fact.expected_len > limit || total.saturating_add(fact.expected_len) > RECORD_MAX_BYTES {
        // Decided from the transcript's own claim: the file is not opened.
        return observation(fact, Status::TooLarge);
    }
    match open_regular(&fact.path) {
        Ok(file) => read_file(fact, &mut &file, || FileStat::of(&file), ops),
        Err(status) => observation(fact, status),
    }
}

/// Read an opened file under the identity check and project it. The reader
/// and `stat` are the stable read's seam.
fn read_file<R: Read>(
    fact: &PresentedMedia,
    reader: &mut R,
    stat: impl FnMut() -> Option<FileStat>,
    ops: &mut Vec<IngestOp>,
) -> CapturedDocument {
    let mut document = observation(fact, Status::Unsupported);
    document.status = read_stable(
        reader,
        stat,
        fact.expected_len,
        Some(fact.expected_len),
        &mut document.bytes,
    );
    // The frame is checked before anything is projected, so a capture is
    // never shown that replay could not find again.
    if document.status == Status::Captured
        && !(document.validate().is_ok() && project(fact, &document, ops))
    {
        document.status = Status::Unsupported;
    }
    if document.status != Status::Captured {
        document.bytes = Vec::new();
    }
    document
}

#[cfg(test)]
mod fallback_tests;
#[cfg(test)]
mod store_tests;
#[cfg(test)]
mod tests;
