//! Files an agent sent the user by path. The one producer of the
//! [`PresentedMedia`] fact and of the path-reference attachment it upgrades.
//!
//! Pure: the transcript names a path, a size and a type, and nothing here
//! opens the file. The owning adapter proves the tool result and reads its
//! own native field names; this module only decides which of those files can
//! be presented and gives each one a stable slot. Capture reads the bytes
//! later, in the pipeline.

use std::collections::BTreeSet;

use rosetta_core::presented_media::{
    FILES_PER_RECORD, PresentedMedia, REFERENCE_MUTATION, upgrade_mutation,
};
use rosetta_core::rows::Ms;
use rosetta_core::timeline::{AttachmentRef, AttachmentSource, ResourceRef};
use rosetta_core::{IngestOp, ToolUpdatePatch};

use crate::adapter::NormalizeCtx;
use crate::{media, tool};

/// One file a tool result lists as sent, in the adapter's native order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedFile<'a> {
    /// The path as recorded.
    pub path: &'a str,
    /// The working directory a relative path resolves against, if known.
    pub cwd: Option<&'a str>,
    /// The size the transcript recorded for the file.
    pub len: u64,
    /// The type the transcript recorded for the file.
    pub media_type: &'a str,
}

/// Attach the presentable files of one tool result to their tool row as path
/// references, in one tool update, and return one fact per attached file.
///
/// Only the first [`FILES_PER_RECORD`] files are examined; a file's `ordinal`
/// is its native index, so a skipped file never shifts a later file's slot.
/// A file is skipped when its type is not a presented type, its size is
/// zero, its path is HTML (the publication seam owns those), is a network
/// path, holds a backslash, does not resolve lexically to an absolute path,
/// or repeats; and when the finished fact does not validate (a device or
/// process path, an over-long one), so nothing emitted here can fail a store
/// batch.
///
/// A recorded path is presented as written or not at all. Capture opens the
/// fact's path, so a path this producer rewrote would name a file the
/// transcript did not: a backslash is a legal POSIX file-name byte, not a
/// separator, and `..` after a symlinked directory is not its lexical
/// parent. An absolute path is therefore kept only when it is already in
/// normal form (no `.`, `..`, empty or trailing component), and a relative
/// path only when it and its `cwd` are, so that the fact's path is the two
/// joined by one `/` and nothing else.
///
/// The caller wraps each returned fact as [`IngestOp::PresentedMedia`].
pub fn push(
    ctx: &NormalizeCtx,
    correlation_key: &str,
    occurrence: &str,
    files: &[PresentedFile<'_>],
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) -> Vec<PresentedMedia> {
    let scope = blake3::hash(occurrence.as_bytes()).to_hex().to_string();
    let mut seen = BTreeSet::new();
    let mut attachments = Vec::new();
    let mut facts = Vec::new();
    for (ordinal, file) in files.iter().take(FILES_PER_RECORD).enumerate() {
        let Some(fact) = fact(ctx, correlation_key, &scope, ordinal, file, at) else {
            continue;
        };
        if !seen.insert(fact.path.clone()) {
            continue;
        }
        attachments.push(fact.attachment.clone());
        facts.push(fact);
    }
    if !attachments.is_empty() {
        IngestOp::push_tool_update(
            ops,
            ctx.session_id.clone(),
            correlation_key.to_owned(),
            ctx.evidence(REFERENCE_MUTATION),
            ToolUpdatePatch {
                attachments,
                ..Default::default()
            },
            at,
        );
    }
    facts
}

fn fact(
    ctx: &NormalizeCtx,
    correlation_key: &str,
    scope: &str,
    ordinal: usize,
    file: &PresentedFile<'_>,
    at: Option<Ms>,
) -> Option<PresentedMedia> {
    let media_type = media::presented_media_type(file.media_type)?;
    let kind = media::presented_kind(media_type)?;
    if file.len == 0
        || file.path.contains(['\0', '\\'])
        || file.path.starts_with("//")
        || rosetta_core::html_artifacts::is_html_path(file.path)
    {
        return None;
    }
    let Ok(ResourceRef::File {
        path,
        resolved: true,
    }) = tool::lexical_resource(file.path, file.cwd, false)
    else {
        return None;
    };
    if !path.starts_with('/') || rosetta_core::html_artifacts::is_html_path(&path) {
        return None;
    }
    // Resolution may only join a relative path to its `cwd`. It never
    // collapses a component of either: see `push`.
    let as_written = if file.path.starts_with('/') {
        path == file.path
    } else {
        file.cwd.is_some_and(|cwd| {
            path.strip_prefix(cwd)
                .and_then(|rest| rest.strip_prefix('/'))
                == Some(file.path)
        })
    };
    if !as_written {
        return None;
    }
    let mut attachment = AttachmentRef::new(
        format!("{scope}:sent-{ordinal}"),
        kind,
        AttachmentSource::Reference {
            reference: ResourceRef::File {
                path: path.clone(),
                resolved: true,
            },
        },
    );
    attachment.name = path.rsplit('/').next().and_then(media::bounded_name);
    attachment.media_type = Some(media_type.to_owned());
    attachment.byte_len = Some(file.len);
    let evidence = ctx.evidence(upgrade_mutation(u32::try_from(ordinal).ok()?));
    let fact = PresentedMedia {
        session_id: ctx.session_id.clone(),
        source_offset: evidence.record_offset,
        path,
        expected_len: file.len,
        media_type: media_type.to_owned(),
        correlation_key: correlation_key.to_owned(),
        attachment,
        evidence,
        at,
    };
    fact.validate().ok()?;
    Some(fact)
}

#[cfg(test)]
mod tests {
    use rosetta_core::timeline::{AttachmentKind, CORE_SCALAR_MAX_BYTES};

    use super::*;

    fn ctx() -> NormalizeCtx {
        let mut ctx = NormalizeCtx::new("runtime", "session");
        ctx.src_offset = Some(4096);
        ctx.source_identity = Some("source".into());
        ctx.source_generation = 3;
        ctx
    }

    fn file<'a>(path: &'a str, media_type: &'a str) -> PresentedFile<'a> {
        PresentedFile {
            path,
            cwd: None,
            len: 10,
            media_type,
        }
    }

    /// The one tool update a call pushed, as `(evidence index, attachments)`.
    fn update(ops: &[IngestOp]) -> (u32, Vec<AttachmentRef>) {
        let [
            IngestOp::ToolUpdate {
                session_id,
                correlation_key,
                evidence,
                patch,
                at,
            },
        ] = ops
        else {
            panic!("expected exactly one tool update, got {ops:?}");
        };
        assert_eq!(session_id, "session");
        assert_eq!(correlation_key, "tool-1");
        assert_eq!(*at, Some(77));
        assert_eq!(evidence.record_offset, 4096);
        assert_eq!(evidence.generation, 3);
        assert!(patch.request.is_none() && patch.outcome.is_none());
        (evidence.mutation_index, patch.attachments.clone())
    }

    #[test]
    fn sent_files_become_reference_slots_and_one_fact_each() {
        let files = [
            file("/work/out/shot.png", "image/png"),
            file("/work/out/clip.mp4", "video/mp4"),
            PresentedFile {
                len: 99,
                ..file("/work/out/screen recording.MOV", " Video/QuickTime ")
            },
            file("/work/out/photo.jpeg", "image/jpg"),
        ];
        let mut ops = Vec::new();
        let facts = push(&ctx(), "tool-1", "result-uuid", &files, Some(77), &mut ops);

        let scope = blake3::hash(b"result-uuid").to_hex().to_string();
        let (index, attachments) = update(&ops);
        assert_eq!(index, REFERENCE_MUTATION);
        assert_eq!(
            attachments.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            (0..4)
                .map(|ordinal| format!("{scope}:sent-{ordinal}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            attachments.iter().map(|a| a.kind).collect::<Vec<_>>(),
            [
                AttachmentKind::Image,
                AttachmentKind::Video,
                AttachmentKind::Video,
                AttachmentKind::Image
            ]
        );
        assert_eq!(
            attachments
                .iter()
                .map(|a| (a.name.as_deref(), a.media_type.as_deref(), a.byte_len))
                .collect::<Vec<_>>(),
            [
                (Some("shot.png"), Some("image/png"), Some(10)),
                (Some("clip.mp4"), Some("video/mp4"), Some(10)),
                (
                    Some("screen recording.MOV"),
                    Some("video/quicktime"),
                    Some(99)
                ),
                (Some("photo.jpeg"), Some("image/jpeg"), Some(10)),
            ]
        );
        for attachment in &attachments {
            attachment.validate().unwrap();
            assert!(attachment.dimensions.is_none() && attachment.label.is_none());
            assert!(attachment.message_ranges.is_empty());
        }

        // One fact per slot, in native order, each carrying its slot as
        // attached and an upgrade stamp above the reference.
        assert_eq!(facts.len(), 4);
        for (ordinal, (fact, attachment)) in facts.iter().zip(&attachments).enumerate() {
            fact.validate().unwrap();
            assert_eq!(&fact.attachment, attachment);
            assert_eq!(fact.session_id, "session");
            assert_eq!(fact.correlation_key, "tool-1");
            assert_eq!(fact.source_offset, 4096);
            assert_eq!(fact.at, Some(77));
            assert_eq!(
                fact.evidence,
                ctx().evidence(upgrade_mutation(ordinal as u32))
            );
            assert!(fact.evidence > ctx().evidence(REFERENCE_MUTATION));
            assert!(fact.evidence < ctx().evidence(u32::MAX - 1));
            assert_eq!(
                attachment.source,
                AttachmentSource::Reference {
                    reference: ResourceRef::File {
                        path: fact.path.clone(),
                        resolved: true
                    }
                }
            );
        }
        assert_eq!(
            facts
                .iter()
                .map(|f| (f.path.as_str(), f.media_type.as_str(), f.expected_len))
                .collect::<Vec<_>>(),
            [
                ("/work/out/shot.png", "image/png", 10),
                ("/work/out/clip.mp4", "video/mp4", 10),
                ("/work/out/screen recording.MOV", "video/quicktime", 99),
                ("/work/out/photo.jpeg", "image/jpeg", 10),
            ]
        );
    }

    #[test]
    fn a_skipped_file_keeps_its_ordinal_and_emits_nothing() {
        let long_name = format!("/{}.png", "n".repeat(CORE_SCALAR_MAX_BYTES));
        let long_path = format!("/{}/a.png", "d/".repeat(CORE_SCALAR_MAX_BYTES / 2));
        let files = [
            file("/work/a.png", "image/png"),         // 0 kept
            file("/work/report.html", "text/html"),   // 1 unknown type
            file("/work/page.HTML", "image/png"),     // 2 html path
            file("/work/notes.md", "text/markdown"),  // 3 unknown type
            file("/work/doc.pdf", "application/pdf"), // 4 not presented
            file("/work/clip.webm", "video/webm"),    // 5 unknown type
            file("/work/untyped.png", ""),            // 6 no type
            PresentedFile {
                len: 0,
                ..file("/work/empty.png", "image/png")
            }, // 7 zero size
            file("relative/b.png", "image/png"),      // 8 relative, no cwd
            file("//server/share/c.png", "image/png"), // 9 network path
            file("\\\\server\\share\\c.png", "image/png"), // 10 network path
            file("C:/Users/me/d.png", "image/png"),   // 11 not rooted at /
            file("/work/nul\0.png", "image/png"),     // 12 NUL
            file(&long_name, "image/png"),            // 13 over-long name
            file(&long_path, "image/png"),            // 14 over-long path
            file("/work/a.png", "image/png"),         // 15 repeats 0
            file("", "image/png"),                    // 16 empty path
            PresentedFile {
                cwd: Some("relative/cwd"),
                ..file("e.png", "image/png")
            }, // 17 cwd is not absolute
            PresentedFile {
                cwd: Some(""),
                ..file("f.png", "image/png")
            }, // 18 empty cwd
            PresentedFile {
                cwd: Some("/work"),
                ..file("page.htm/.", "image/png")
            }, // 19 html once resolved
            PresentedFile {
                cwd: Some("/work/sub"),
                ..file("g.mov", "video/quicktime")
            }, // 20 kept
            PresentedFile {
                cwd: Some("/work"),
                ..file("a.png", "image/png")
            }, // 21 repeats 0 once resolved
        ];
        let mut ops = Vec::new();
        let facts = push(&ctx(), "tool-1", "occurrence", &files, Some(77), &mut ops);
        let scope = blake3::hash(b"occurrence").to_hex().to_string();
        let (_, attachments) = update(&ops);
        assert_eq!(
            attachments.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            [format!("{scope}:sent-0"), format!("{scope}:sent-20")]
        );
        assert_eq!(
            facts
                .iter()
                .map(|f| (f.path.as_str(), f.evidence.mutation_index))
                .collect::<Vec<_>>(),
            [
                ("/work/a.png", upgrade_mutation(0)),
                ("/work/sub/g.mov", upgrade_mutation(20)),
            ]
        );
        for fact in &facts {
            fact.validate().unwrap();
        }
    }

    /// Capture opens the fact's path, so the producer never presents a path
    /// it would have to rewrite: the file opened would not be the file sent.
    #[test]
    fn a_recorded_path_is_presented_as_written_or_not_at_all() {
        let in_work = |path| PresentedFile {
            cwd: Some("/work"),
            ..file(path, "image/png")
        };
        let files = [
            file("/work/a.png", "image/png"),           // 0 kept
            file("/work/out/shot\\1.png", "image/png"), // 1 a backslash in a name
            file("/work/link/../b.png", "image/png"),   // 2 `..` after a directory
            file("/\\server/share/x.png", "image/png"), // 3 `/\server`
            file("\\work\\c.png", "image/png"),         // 4 backslash-rooted
            file("C:\\Users\\me\\d.png", "image/png"),  // 5 a drive path
            file("/work/./e.png", "image/png"),         // 6 a `.` component
            file("/work//f.png", "image/png"),          // 7 an empty component
            file("/work/g.png/", "image/png"),          // 8 a trailing slash
            file("/../h.png", "image/png"),             // 9 `..` at the root
            file("/work/./x/../a.png", "image/png"),    // 10 would resolve to 0
            in_work("/work/sub/../i.png"),              // 11 absolute, beside a cwd
            in_work("sub\\j.png"),                      // 12 a backslash, relative
            PresentedFile {
                cwd: Some("/work\\sub"),
                ..file("k.png", "image/png")
            }, // 13 a backslash in the cwd
            file("/work/..hidden/l.png", "image/png"),  // 14 kept: `..` inside a name
            file("/work/.cache/m.png", "image/png"),    // 15 kept: a dot name
            in_work("/work/n.png"),                     // 16 kept: absolute as written
            in_work("./sub/../o.png"),                  // 17 relative, not as written
            in_work("../p.png"),                        // 18 leaves its cwd
            PresentedFile {
                cwd: Some("/work/x/.."),
                ..file("q.png", "image/png")
            }, // 19 a cwd not as written
            PresentedFile {
                cwd: Some("/work/"),
                ..file("r.png", "image/png")
            }, // 20 a cwd with a trailing slash
            in_work("sub/s.png"),                       // 21 kept: joined to its cwd
        ];
        let mut ops = Vec::new();
        let facts = push(&ctx(), "tool-1", "occurrence", &files, Some(77), &mut ops);
        let scope = blake3::hash(b"occurrence").to_hex().to_string();
        let (_, attachments) = update(&ops);
        assert_eq!(
            attachments.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            [0, 14, 15, 16, 21].map(|ordinal| format!("{scope}:sent-{ordinal}"))
        );
        assert_eq!(
            facts
                .iter()
                .map(|f| (f.path.as_str(), f.evidence.mutation_index))
                .collect::<Vec<_>>(),
            [
                ("/work/a.png", upgrade_mutation(0)),
                ("/work/..hidden/l.png", upgrade_mutation(14)),
                ("/work/.cache/m.png", upgrade_mutation(15)),
                ("/work/n.png", upgrade_mutation(16)),
                ("/work/sub/s.png", upgrade_mutation(21)),
            ]
        );
        // A kept absolute path is the recorded string, byte for byte, in the
        // fact and in the slot a reader sees.
        for (fact, ordinal) in facts.iter().zip([0, 14, 15, 16]) {
            fact.validate().unwrap();
            assert_eq!(fact.path, files[ordinal].path);
            assert_eq!(
                fact.attachment.source,
                AttachmentSource::Reference {
                    reference: ResourceRef::File {
                        path: files[ordinal].path.to_owned(),
                        resolved: true
                    }
                }
            );
        }
        assert_eq!(attachments[1].name.as_deref(), Some("l.png"));
    }

    /// Opening `/dev/fd/N` duplicates the reader's own descriptor; a device
    /// or process path is never presented, however it is spelled.
    #[test]
    fn a_device_or_process_path_is_skipped() {
        let under = |cwd, path| PresentedFile {
            cwd: Some(cwd),
            ..file(path, "image/png")
        };
        let files = [
            file("/dev/fd/3", "image/png"),        // 0
            file("/dev/null", "video/mp4"),        // 1
            file("/dev", "image/png"),             // 2
            file("/proc/self/fd/3", "image/png"),  // 3
            file("/proc", "video/quicktime"),      // 4
            file("/DEV/fd/3", "image/png"),        // 5 a case-folding volume
            file("/dev/../dev/fd/3", "image/png"), // 6 not as written, either
            under("/dev", "fd/3"),                 // 7 joined to a device cwd
            under("/proc", "self/fd/3"),           // 8 joined to a process cwd
            under("/work", "../dev/fd/3"),         // 9 not as written, either
            file("/devices/a.png", "image/png"),   // 10 kept
            file("/work/dev/b.png", "image/png"),  // 11 kept
            file("/work/proc/c.mp4", "video/mp4"), // 12 kept
        ];
        let mut ops = Vec::new();
        let facts = push(&ctx(), "tool-1", "occurrence", &files, Some(77), &mut ops);
        assert_eq!(
            facts
                .iter()
                .map(|f| (f.path.as_str(), f.evidence.mutation_index))
                .collect::<Vec<_>>(),
            [
                ("/devices/a.png", upgrade_mutation(10)),
                ("/work/dev/b.png", upgrade_mutation(11)),
                ("/work/proc/c.mp4", upgrade_mutation(12)),
            ]
        );
        assert_eq!(update(&ops).1.len(), 3);
        // Alone, such a file pushes no update at all.
        let mut ops = Vec::new();
        assert!(
            push(
                &ctx(),
                "tool-1",
                "occurrence",
                &files[..10],
                Some(77),
                &mut ops
            )
            .is_empty()
        );
        assert!(ops.is_empty());
    }

    #[test]
    fn nothing_presentable_pushes_nothing() {
        let mut ops = Vec::new();
        assert!(push(&ctx(), "tool-1", "occurrence", &[], Some(77), &mut ops).is_empty());
        let files = [
            file("/work/report.html", "text/html"),
            file("relative.png", "image/png"),
        ];
        assert!(push(&ctx(), "tool-1", "occurrence", &files, Some(77), &mut ops).is_empty());
        assert!(ops.is_empty());
        // A fact with no session or no correlation key cannot be applied.
        let files = [file("/work/a.png", "image/png")];
        assert!(push(&ctx(), "", "occurrence", &files, None, &mut ops).is_empty());
        let orphan = NormalizeCtx::new("runtime", "");
        assert!(push(&orphan, "tool-1", "occurrence", &files, None, &mut ops).is_empty());
        assert!(ops.is_empty());
    }

    #[test]
    fn only_the_first_thirty_two_files_are_examined() {
        let paths: Vec<String> = (0..40).map(|index| format!("/work/{index}.png")).collect();
        let files: Vec<_> = paths.iter().map(|path| file(path, "image/png")).collect();
        let mut ops = Vec::new();
        let facts = push(&ctx(), "tool-1", "occurrence", &files, Some(77), &mut ops);
        let (_, attachments) = update(&ops);
        assert_eq!(attachments.len(), FILES_PER_RECORD);
        assert_eq!(facts.len(), FILES_PER_RECORD);
        assert_eq!(facts[31].path, "/work/31.png");
        assert_eq!(facts[31].evidence.mutation_index, u32::MAX - 2);
        // A skipped file inside the window is not replaced by one past it.
        let mut files = files;
        files[0].len = 0;
        let mut ops = Vec::new();
        let facts = push(&ctx(), "tool-1", "occurrence", &files, Some(77), &mut ops);
        assert_eq!(facts.len(), FILES_PER_RECORD - 1);
        assert_eq!(facts[0].path, "/work/1.png");
        assert_eq!(facts.last().unwrap().path, "/work/31.png");
    }

    #[test]
    fn slot_ids_are_scoped_by_occurrence_and_a_missing_offset_reads_as_zero() {
        let files = [file("/work/a.png", "image/png")];
        let mut first = Vec::new();
        let mut second = Vec::new();
        let a = push(&ctx(), "tool-1", "result-a", &files, Some(77), &mut first);
        let b = push(&ctx(), "tool-1", "result-b", &files, Some(77), &mut second);
        assert_ne!(a[0].attachment.id, b[0].attachment.id);
        // Deterministic: the same inputs give the same slot.
        let mut again = Vec::new();
        assert_eq!(
            push(&ctx(), "tool-1", "result-a", &files, Some(77), &mut again),
            a
        );
        assert_eq!(again, first);

        let mut direct = NormalizeCtx::new("runtime", "session");
        direct.src_offset = Some(-1);
        let mut ops = Vec::new();
        let facts = push(&direct, "tool-1", "result-a", &files, None, &mut ops);
        assert_eq!(facts[0].source_offset, 0);
        assert_eq!(facts[0].evidence.record_offset, 0);
        facts[0].validate().unwrap();
    }
}
