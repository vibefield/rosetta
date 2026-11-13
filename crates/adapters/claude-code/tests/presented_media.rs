//! Sent files: a proven `SendUserFile` result attaches each sent image or
//! video to its tool row as a path reference and states one capture fact per
//! file. Synthetic transcripts only; the adapter opens nothing.

use std::path::Path;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{AgentAdapter, LineStatus, NormalizeCtx};
use rosetta_core::presented_media::{
    FILES_PER_RECORD, PresentedMedia, REFERENCE_MUTATION, upgrade_mutation,
};
use rosetta_core::timeline::{AttachmentKind, AttachmentRef, AttachmentSource, ResourceRef};
use rosetta_core::{IngestOp, html_artifacts::HtmlArtifactPublication};
use serde_json::{Value, json};

const CALL: &str = "send-call";
const OPENER_OFFSET: i64 = 10;
const RESULT_OFFSET: i64 = 4096;
const RESULT_AT: i64 = 1_790_856_001_000;

fn opener_named(id: &str, name: &str, files: &[&str]) -> Value {
    json!({"type":"assistant","parentUuid":null,"isSidechain":false,"uuid":format!("opener-{id}"),
        "timestamp":"2026-10-01T12:00:00Z","sessionId":"native","version":"2.1.283","cwd":"/work",
        "gitBranch":"main","userType":"external",
        "message":{"id":format!("response-{id}"),"type":"message","role":"assistant",
            "model":"claude-opus-5","stop_reason":"tool_use",
            "content":[{"type":"tool_use","id":id,"name":name,
                "input":{"files":files,"caption":"Here it is"}}]}})
}

fn opener(files: &[&str]) -> Value {
    opener_named(CALL, "SendUserFile", files)
}

/// A user line holding one `tool_result` per id and one `toolUseResult`.
fn result_line(ids: &[&str], failed: bool, tool_use_result: Value) -> Value {
    let content: Vec<Value> = ids
        .iter()
        .map(|id| json!({"type":"tool_result","tool_use_id":id,"content":"sent","is_error":failed}))
        .collect();
    json!({"type":"user","parentUuid":"opener","isSidechain":false,"uuid":"result",
        "timestamp":"2026-10-01T12:00:01Z","sessionId":"native","version":"2.1.283","cwd":"/work",
        "gitBranch":"main","userType":"external",
        "message":{"role":"user","content":content},
        "toolUseResult":tool_use_result})
}

fn attachment(path: &str, size: u64, media_type: Option<&str>) -> Value {
    let mut value = json!({"path":path,"size":size,"isImage":false,"pathValidated":true,
        "file_uuid":"00000000-0000-4000-8000-000000000000"});
    if let Some(media_type) = media_type {
        value["media_type"] = json!(media_type);
        value["isImage"] = json!(media_type.starts_with("image/"));
    }
    value
}

fn sent(attachments: Vec<Value>) -> Value {
    json!({"attachments":attachments,"caption":"Here it is","display":"inline"})
}

fn new_ctx() -> NormalizeCtx {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = ClaudeCodeAdapter.init_state();
    ctx.source_identity = Some("source".into());
    ctx.source_generation = 2;
    ctx
}

fn ingest(ctx: &mut NormalizeCtx, offset: i64, record: &Value) -> Vec<IngestOp> {
    ctx.src_offset = Some(offset);
    let output =
        ClaudeCodeAdapter.ingest_line(ctx, "transcript", &serde_json::to_vec(record).unwrap());
    assert!(
        !matches!(output.status, LineStatus::HardError { .. }),
        "{output:?}"
    );
    output.ops
}

fn save_and_restore(ctx: &mut NormalizeCtx) -> Value {
    let saved = ClaudeCodeAdapter
        .checkpoint_state(ctx.state.as_deref())
        .unwrap()
        .unwrap();
    ctx.state = ClaudeCodeAdapter.restore_state(Some(&saved)).unwrap();
    saved
}

/// The result line's ops after its opener, optionally across a checkpoint.
fn run_with(opener: &Value, result: &Value, resume: bool) -> Vec<IngestOp> {
    let mut ctx = new_ctx();
    let opened = ingest(&mut ctx, OPENER_OFFSET, opener);
    assert!(sent_files(&opened).is_none(), "an opener presents nothing");
    assert!(facts(&opened).is_empty(), "an opener states no fact");
    if resume {
        save_and_restore(&mut ctx);
    }
    ingest(&mut ctx, RESULT_OFFSET, result)
}

fn run(tool_use_result: Value) -> Vec<IngestOp> {
    run_with(
        &opener(&["/work/out/unused.bin"]),
        &result_line(&[CALL], false, tool_use_result),
        false,
    )
}

/// The one sent-file tool update, as its attachments, with its position.
fn sent_files(ops: &[IngestOp]) -> Option<(usize, Vec<AttachmentRef>)> {
    let mut found = None;
    for (position, op) in ops.iter().enumerate() {
        let IngestOp::ToolUpdate {
            session_id,
            correlation_key,
            evidence,
            patch,
            at,
        } = op
        else {
            continue;
        };
        if evidence.mutation_index != REFERENCE_MUTATION {
            continue;
        }
        assert!(found.is_none(), "one reference update per result");
        assert_eq!(session_id, "session");
        assert_eq!(correlation_key, CALL);
        assert_eq!(*at, Some(RESULT_AT));
        assert_eq!(evidence.record_offset, RESULT_OFFSET as u64);
        assert_eq!(evidence.generation, 2);
        assert_eq!(evidence.source_identity.0, "source");
        assert!(
            patch.request.is_none()
                && patch.outcome.is_none()
                && patch.status.is_none()
                && patch.timing.is_none(),
            "the reference update carries attachments only"
        );
        assert!(!patch.attachments.is_empty());
        found = Some((position, patch.attachments.clone()));
    }
    found
}

fn facts(ops: &[IngestOp]) -> Vec<PresentedMedia> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::PresentedMedia(fact) => Some(fact.clone()),
            _ => None,
        })
        .collect()
}

fn publications(ops: &[IngestOp]) -> Vec<HtmlArtifactPublication> {
    ops.iter()
        .filter_map(|op| match op {
            IngestOp::HtmlArtifact(row) => Some(row.clone()),
            _ => None,
        })
        .collect()
}

/// Every tool update that is not the sent-file reference: the result's own
/// lifecycle patch, which sent files must leave as it was.
fn other_updates(ops: &[IngestOp]) -> Vec<&IngestOp> {
    ops.iter()
        .filter(|op| {
            matches!(op, IngestOp::ToolUpdate { evidence, .. }
                if evidence.mutation_index != REFERENCE_MUTATION)
        })
        .collect()
}

fn assert_nothing_presented(ops: &[IngestOp], case: &str) {
    assert!(sent_files(ops).is_none(), "{case}: no attachment");
    assert!(facts(ops).is_empty(), "{case}: no fact");
    assert!(
        !ops.iter()
            .any(|op| matches!(op, IngestOp::ProjectionAsset { .. })),
        "{case}: the adapter never captures bytes"
    );
}

fn slot(index: usize) -> String {
    format!("{}:sent-{index}", blake3::hash(CALL.as_bytes()).to_hex())
}

fn reference(path: &str) -> AttachmentSource {
    AttachmentSource::Reference {
        reference: ResourceRef::File {
            path: path.to_owned(),
            resolved: true,
        },
    }
}

/// What the transcript said about one presented file.
#[derive(Clone, Copy)]
struct Sent {
    index: usize,
    kind: AttachmentKind,
    path: &'static str,
    name: &'static str,
    media_type: &'static str,
    size: u64,
}

/// One attached slot and its fact, checked against what the transcript said.
#[track_caller]
fn assert_slot(attachment: &AttachmentRef, fact: &PresentedMedia, sent: Sent) {
    let Sent {
        index,
        kind,
        path,
        name,
        media_type,
        size,
    } = sent;
    assert_eq!(attachment.id, slot(index));
    assert_eq!(attachment.kind, kind);
    assert_eq!(attachment.source, reference(path));
    assert_eq!(attachment.name.as_deref(), Some(name));
    assert_eq!(attachment.media_type.as_deref(), Some(media_type));
    assert_eq!(attachment.byte_len, Some(size));
    assert!(attachment.label.is_none() && attachment.dimensions.is_none());
    assert!(attachment.message_ranges.is_empty());
    attachment.validate().unwrap();

    fact.validate().unwrap();
    assert_eq!(&fact.attachment, attachment, "the slot exactly as attached");
    assert_eq!(fact.session_id, "session");
    assert_eq!(fact.correlation_key, CALL);
    assert_eq!(fact.source_offset, RESULT_OFFSET as u64);
    assert_eq!(fact.path, path);
    assert_eq!(fact.expected_len, size);
    assert_eq!(fact.media_type, media_type);
    assert_eq!(fact.at, Some(RESULT_AT));
    assert_eq!(fact.evidence.record_offset, RESULT_OFFSET as u64);
    assert_eq!(fact.evidence.generation, 2);
    assert_eq!(fact.evidence.source_identity.0, "source");
    assert_eq!(
        fact.evidence.mutation_index,
        upgrade_mutation(u32::try_from(index).unwrap())
    );
    assert!(fact.evidence.mutation_index > REFERENCE_MUTATION);
}

#[test]
fn one_sent_video_is_a_path_reference_and_one_fact() {
    let ops = run(sent(vec![attachment(
        "/work/out/screen recording.mp4",
        11_534_336,
        Some("video/mp4"),
    )]));
    let (position, attachments) = sent_files(&ops).expect("one reference update");
    let facts = facts(&ops);
    assert_eq!((attachments.len(), facts.len()), (1, 1));
    assert_slot(
        &attachments[0],
        &facts[0],
        Sent {
            index: 0,
            kind: AttachmentKind::Video,
            path: "/work/out/screen recording.mp4",
            name: "screen recording.mp4",
            media_type: "video/mp4",
            size: 11_534_336,
        },
    );
    // The fact follows the update it upgrades, directly.
    assert!(matches!(&ops[position + 1], IngestOp::PresentedMedia(fact) if *fact == facts[0]));
    // The result's own terminal patch is still there, and still below both.
    let others = other_updates(&ops);
    assert_eq!(others.len(), 1, "{ops:?}");
    let IngestOp::ToolUpdate {
        evidence, patch, ..
    } = others[0]
    else {
        unreachable!()
    };
    assert_eq!(evidence.mutation_index, 0);
    assert!(patch.status.is_some() && patch.attachments.is_empty());
    assert!(
        !ops.iter()
            .any(|op| matches!(op, IngestOp::ProjectionAsset { .. })),
        "the bytes are not in the transcript"
    );
}

#[test]
fn one_sent_image_is_a_path_reference_and_one_fact() {
    let ops = run(sent(vec![attachment(
        "/work/out/shot.PNG",
        48_213,
        Some("image/png"),
    )]));
    let (_, attachments) = sent_files(&ops).expect("one reference update");
    let facts = facts(&ops);
    assert_eq!((attachments.len(), facts.len()), (1, 1));
    assert_slot(
        &attachments[0],
        &facts[0],
        Sent {
            index: 0,
            kind: AttachmentKind::Image,
            path: "/work/out/shot.PNG",
            name: "shot.PNG",
            media_type: "image/png",
            size: 48_213,
        },
    );
}

#[test]
fn a_mixed_list_keeps_native_order_and_native_index_ids() {
    let ops = run_with(
        &opener(&["/work/out/report.html"]),
        &result_line(
            &[CALL],
            false,
            sent(vec![
                attachment("/work/out/a.png", 100, Some("image/png")), // 0
                attachment("/work/out/notes.md", 200, Some("text/markdown")), // 1
                attachment("/work/out/report.html", 300, Some("text/html")), // 2
                attachment("/work/out/untyped.bin", 400, None),        // 3
                attachment("/work/out/b.mov", 500, Some("video/quicktime")), // 4
                attachment("/work/out/c.jpg", 600, Some("image/jpeg")), // 5
                attachment("/work/out/d.mp4", 700, Some("video/mp4")), // 6
            ]),
        ),
        false,
    );
    let (position, attachments) = sent_files(&ops).expect("one reference update");
    let facts = facts(&ops);
    let image = AttachmentKind::Image;
    let video = AttachmentKind::Video;
    let expected = [
        (0, image, "/work/out/a.png", "a.png", "image/png", 100),
        (4, video, "/work/out/b.mov", "b.mov", "video/quicktime", 500),
        (5, image, "/work/out/c.jpg", "c.jpg", "image/jpeg", 600),
        (6, video, "/work/out/d.mp4", "d.mp4", "video/mp4", 700),
    ]
    .map(|(index, kind, path, name, media_type, size)| Sent {
        index,
        kind,
        path,
        name,
        media_type,
        size,
    });
    assert_eq!((attachments.len(), facts.len()), (4, 4));
    for ((attachment, fact), sent) in attachments.iter().zip(&facts).zip(expected) {
        assert_slot(attachment, fact, sent);
    }
    // One update, then its facts in native order, with nothing between.
    for (step, fact) in facts.iter().enumerate() {
        assert!(
            matches!(&ops[position + 1 + step], IngestOp::PresentedMedia(found) if found == fact)
        );
    }
    // The HTML file went to the publication seam and nowhere else.
    let published = publications(&ops);
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].path.as_deref(), Some("/work/out/report.html"));
}

#[test]
fn html_markdown_and_an_untyped_attachment_present_nothing() {
    let ops = run_with(
        &opener(&[
            "/work/out/report.html",
            "/work/out/notes.md",
            "/work/out/blob",
        ]),
        &result_line(
            &[CALL],
            false,
            sent(vec![
                attachment("/work/out/report.html", 300, Some("text/html")),
                attachment("/work/out/notes.md", 200, Some("text/markdown")),
                attachment("/work/out/blob", 400, None),
                // An image type cannot make an HTML path presentable.
                attachment("/work/out/page.htm", 500, Some("image/png")),
            ]),
        ),
        false,
    );
    assert_nothing_presented(&ops, "html, markdown, untyped");
    let published = publications(&ops);
    assert_eq!(
        published
            .iter()
            .map(|row| row.path.as_deref())
            .collect::<Vec<_>>(),
        [Some("/work/out/report.html")],
        "the HTML file still produces its publication"
    );
    assert_eq!(other_updates(&ops).len(), 1, "the terminal patch survives");
}

#[test]
fn an_unproven_result_presents_nothing() {
    let video = || {
        sent(vec![attachment(
            "/work/out/clip.mp4",
            900,
            Some("video/mp4"),
        )])
    };
    let send = opener(&["/work/out/clip.mp4"]);

    // The tool result is a string: there is no attachment list to read.
    let ops = run_with(
        &send,
        &result_line(&[CALL], false, json!("Sent 1 file to the user")),
        false,
    );
    assert_nothing_presented(&ops, "string toolUseResult");
    assert_eq!(other_updates(&ops).len(), 1);

    // No `toolUseResult` at all.
    let mut bare = result_line(&[CALL], false, json!(null));
    bare.as_object_mut().unwrap().remove("toolUseResult");
    assert_nothing_presented(&run_with(&send, &bare, false), "no toolUseResult");

    // A failed result sent nothing, whatever its payload lists.
    let ops = run_with(&send, &result_line(&[CALL], true, video()), false);
    assert_nothing_presented(&ops, "is_error");
    let mut refused = video();
    refused["success"] = json!(false);
    let ops = run_with(&send, &result_line(&[CALL], false, refused), false);
    assert_nothing_presented(&ops, "native failure");

    // Two results on one line: the line's one `toolUseResult` is not
    // provably this call's.
    let mut ctx = new_ctx();
    ingest(&mut ctx, OPENER_OFFSET, &send);
    ingest(
        &mut ctx,
        OPENER_OFFSET + 1,
        &opener_named("other-call", "SendUserFile", &["/work/out/other.mp4"]),
    );
    let ops = ingest(
        &mut ctx,
        RESULT_OFFSET,
        &result_line(&[CALL, "other-call"], false, video()),
    );
    assert_nothing_presented(&ops, "two results in one line");
    assert_eq!(other_updates(&ops).len(), 2);
    // The ambiguous line consumed both openers: a later look-alike for
    // either id proves nothing.
    let ops = ingest(
        &mut ctx,
        RESULT_OFFSET + 100,
        &result_line(&[CALL], false, video()),
    );
    assert_nothing_presented(&ops, "an opener is consumed once");

    // A look-alike payload on another tool's result, and on no opener.
    for name in ["Artifact", "Read", "Bash", "mcp__files__SendUserFile"] {
        let other = opener_named(CALL, name, &["/work/out/clip.mp4"]);
        let ops = run_with(&other, &result_line(&[CALL], false, video()), false);
        assert_nothing_presented(&ops, name);
    }
    let mut ctx = new_ctx();
    let ops = ingest(
        &mut ctx,
        RESULT_OFFSET,
        &result_line(&[CALL], false, video()),
    );
    assert_nothing_presented(&ops, "no opener");
    // A different call's opener does not prove this result.
    let mut ctx = new_ctx();
    ingest(
        &mut ctx,
        OPENER_OFFSET,
        &opener_named("other-call", "SendUserFile", &["/work/out/clip.mp4"]),
    );
    let ops = ingest(
        &mut ctx,
        RESULT_OFFSET,
        &result_line(&[CALL], false, video()),
    );
    assert_nothing_presented(&ops, "another call's opener");
}

#[test]
fn a_malformed_attachment_is_skipped_without_a_hard_error() {
    let good = || attachment("/work/out/good.png", 77, Some("image/png"));
    let broken: Vec<(&str, Value)> = vec![
        (
            "fractional size",
            json!({"path":"/work/out/a.png","size":10.5,"media_type":"image/png","isImage":true}),
        ),
        (
            "float-typed size",
            json!({"path":"/work/out/a.png","size":10.0,"media_type":"image/png","isImage":true}),
        ),
        (
            "string size",
            json!({"path":"/work/out/a.png","size":"10","media_type":"image/png","isImage":true}),
        ),
        (
            "absent size",
            json!({"path":"/work/out/a.png","media_type":"image/png","isImage":true}),
        ),
        (
            "zero size",
            json!({"path":"/work/out/a.png","size":0,"media_type":"image/png","isImage":true}),
        ),
        (
            "negative size",
            json!({"path":"/work/out/a.png","size":-10,"media_type":"image/png","isImage":true}),
        ),
        (
            "size past u64",
            json!({"path":"/work/out/a.png","size":1e40,"media_type":"image/png","isImage":true}),
        ),
        (
            "numeric path",
            json!({"path":42,"size":10,"media_type":"image/png","isImage":true}),
        ),
        (
            "null path",
            json!({"path":null,"size":10,"media_type":"image/png","isImage":true}),
        ),
        (
            "array path",
            json!({"path":["/work/out/a.png"],"size":10,"media_type":"image/png","isImage":true}),
        ),
        (
            "absent path",
            json!({"size":10,"media_type":"image/png","isImage":true}),
        ),
        (
            "relative path",
            json!({"path":"out/a.png","size":10,"media_type":"image/png","isImage":true}),
        ),
        (
            "dot-relative path",
            json!({"path":"./a.png","size":10,"media_type":"image/png","isImage":true}),
        ),
        (
            "empty path",
            json!({"path":"","size":10,"media_type":"image/png","isImage":true}),
        ),
        (
            "numeric media type",
            json!({"path":"/work/out/a.png","size":10,"media_type":7,"isImage":true}),
        ),
        ("a string, not an object", json!("/work/out/a.png")),
        ("null", json!(null)),
    ];
    for (case, bad) in &broken {
        // Alone: nothing at all.
        let ops = run(sent(vec![bad.clone()]));
        assert_nothing_presented(&ops, case);
        assert_eq!(other_updates(&ops).len(), 1, "{case}: terminal patch");

        // Beside a good file: only the good one, at its own native index.
        let ops = run(sent(vec![bad.clone(), good()]));
        let (_, attachments) = sent_files(&ops).unwrap_or_else(|| panic!("{case}"));
        let facts = facts(&ops);
        assert_eq!((attachments.len(), facts.len()), (1, 1), "{case}");
        assert_slot(
            &attachments[0],
            &facts[0],
            Sent {
                index: 1,
                kind: AttachmentKind::Image,
                path: "/work/out/good.png",
                name: "good.png",
                media_type: "image/png",
                size: 77,
            },
        );
    }
    // The list itself in another shape.
    for (case, payload) in [
        ("attachments is a string", json!({"attachments":"none"})),
        (
            "attachments is an object",
            json!({"attachments":{"path":"/work/out/a.png","size":10,"media_type":"image/png"}}),
        ),
        ("attachments is null", json!({"attachments":null})),
        ("attachments is empty", json!({"attachments":[]})),
        ("no attachments", json!({"caption":"nothing"})),
        ("an array payload", json!([good()])),
    ] {
        assert_nothing_presented(&run(payload), case);
    }
}

#[test]
fn repeats_and_files_past_the_window_present_nothing() {
    // The same file twice is one slot: the first native index.
    let ops = run(sent(vec![
        attachment("/work/out/a.png", 10, Some("image/png")),
        attachment("/work/out/a.png", 10, Some("image/png")),
        attachment("/work/out/b.png", 20, Some("image/png")),
    ]));
    let (_, attachments) = sent_files(&ops).unwrap();
    assert_eq!(
        attachments.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        [slot(0), slot(2)]
    );
    assert_eq!(facts(&ops).len(), 2);

    let many: Vec<Value> = (0..FILES_PER_RECORD + 8)
        .map(|index| {
            attachment(
                &format!("/work/out/{index}.png"),
                1 + index as u64,
                Some("image/png"),
            )
        })
        .collect();
    let ops = run(sent(many));
    let (_, attachments) = sent_files(&ops).unwrap();
    let facts = facts(&ops);
    assert_eq!(attachments.len(), FILES_PER_RECORD);
    assert_eq!(facts.len(), FILES_PER_RECORD);
    assert_eq!(facts.last().unwrap().path, "/work/out/31.png");
    for fact in &facts {
        fact.validate().unwrap();
    }
}

/// Capture opens the path the fact names. A recorded path that would have
/// to be rewritten first (a backslash read as a separator, a `..` collapsed)
/// names a file the transcript did not, so it presents nothing; and a device
/// or process path is never opened.
#[test]
fn a_path_that_is_not_presentable_as_written_presents_nothing() {
    let good = || attachment("/work/out/good.png", 77, Some("image/png"));
    for path in [
        // A backslash is a legal file-name byte, not a separator.
        "/work/out/shot\\1.png",
        "\\work\\out\\a.png",
        "C:\\Users\\me\\a.png",
        "\\\\server\\share\\a.png",
        "/\\server/share/a.png",
        // `link/..` is not `out` when `link` is a symlink.
        "/work/out/link/../a.png",
        "/work/out/./x/../a.png",
        "/work/out/./a.png",
        "/work/out//a.png",
        "/work/out/a.png/",
        "/../work/out/a.png",
        "//server/share/a.png",
        // Opening one of these acts on the reader itself.
        "/dev/fd/3",
        "/dev/null",
        "/DEV/fd/3",
        "/proc/self/fd/3",
    ] {
        let bad = attachment(path, 10, Some("image/png"));
        let ops = run(sent(vec![bad.clone()]));
        assert_nothing_presented(&ops, path);
        assert_eq!(other_updates(&ops).len(), 1, "{path}: terminal patch");

        // Beside a good file: only the good one, at its own native index.
        let ops = run(sent(vec![bad, good()]));
        let (_, attachments) = sent_files(&ops).unwrap_or_else(|| panic!("{path}"));
        let facts = facts(&ops);
        assert_eq!((attachments.len(), facts.len()), (1, 1), "{path}");
        assert_slot(
            &attachments[0],
            &facts[0],
            Sent {
                index: 1,
                kind: AttachmentKind::Image,
                path: "/work/out/good.png",
                name: "good.png",
                media_type: "image/png",
                size: 77,
            },
        );
    }
    // What is presented is the recorded path, byte for byte.
    for path in [
        "/work/out/..hidden/a b.png",
        "/work/.cache/a.png",
        "/devices/a.png",
        "/work/dev/a.png",
    ] {
        let ops = run(sent(vec![attachment(path, 10, Some("image/png"))]));
        let facts = facts(&ops);
        assert_eq!(facts.len(), 1, "{path}");
        assert_eq!(facts[0].path, path);
        assert_eq!(sent_files(&ops).unwrap().1[0].source, reference(path));
    }
}

#[test]
fn a_split_run_equals_a_one_shot_run() {
    let send = opener(&["/work/out/a.png", "/work/out/b.mp4"]);
    let reply = result_line(
        &[CALL],
        false,
        sent(vec![
            attachment("/work/out/a.png", 100, Some("image/png")),
            attachment("/work/out/notes.md", 200, Some("text/markdown")),
            attachment("/work/out/b.mp4", 300, Some("video/mp4")),
        ]),
    );
    let one_shot = run_with(&send, &reply, false);
    let split = run_with(&send, &reply, true);
    assert_eq!(one_shot, split);
    assert_eq!(facts(&split).len(), 2);
    assert_eq!(sent_files(&split).unwrap().1.len(), 2);

    // The opener is what the checkpoint carries, and only until its result.
    let mut ctx = new_ctx();
    let empty = save_and_restore(&mut ctx);
    assert!(empty.get("send_file_calls").is_none(), "{empty}");
    ingest(&mut ctx, OPENER_OFFSET, &send);
    let pending = save_and_restore(&mut ctx);
    assert_eq!(pending["send_file_calls"], json!([CALL]));
    let ops = ingest(&mut ctx, RESULT_OFFSET, &reply);
    assert_eq!(ops, one_shot);
    let settled = save_and_restore(&mut ctx);
    assert!(settled.get("send_file_calls").is_none(), "{settled}");

    // A checkpoint written before this state existed restores cleanly.
    let mut legacy = pending.clone();
    legacy.as_object_mut().unwrap().remove("send_file_calls");
    let mut ctx = new_ctx();
    ctx.state = ClaudeCodeAdapter.restore_state(Some(&legacy)).unwrap();
    assert_nothing_presented(
        &ingest(&mut ctx, RESULT_OFFSET, &reply),
        "legacy checkpoint",
    );
}

/// A `SendUserFile` opener under an id of its own.
fn send(id: &str) -> Value {
    opener_named(id, "SendUserFile", &["/work/out/clip.mp4"])
}

/// Whether a result for `id` presents its one file now.
fn presents(ctx: &mut NormalizeCtx, id: &str) -> bool {
    let video = sent(vec![attachment(
        "/work/out/clip.mp4",
        900,
        Some("video/mp4"),
    )]);
    let ops = ingest(ctx, RESULT_OFFSET, &result_line(&[id], false, video));
    let facts = facts(&ops);
    assert!(facts.len() <= 1, "{id}: {facts:?}");
    facts.first().is_some_and(|fact| fact.correlation_key == id)
}

#[test]
fn remembered_openers_are_bounded_and_the_oldest_go_first() {
    // Ids in an order that is not their sort order, as real ids are: the
    // 300 openers arrive as 0000, 0119, 0238, 0057, ...
    let id = |index: usize| format!("toolu_{:04}", (index * 119) % 300);
    let mut ctx = new_ctx();
    for index in 0..300 {
        ingest(&mut ctx, OPENER_OFFSET + index as i64, &send(&id(index)));
    }
    let saved = save_and_restore(&mut ctx);
    // The newest 256, oldest first: the 44 dropped are the first 44 seen,
    // whatever their ids sort as.
    let newest: Vec<Value> = (44..300).map(|index| json!(id(index))).collect();
    assert_eq!(saved["send_file_calls"], json!(newest));
    assert!(
        (44..300).map(id).min() < (0..44).map(id).max(),
        "a kept id sorts below a dropped one"
    );
    for dropped in [0, 1, 43] {
        assert!(!presents(&mut ctx, &id(dropped)), "opener {dropped}");
    }
    for kept in [44, 45, 299] {
        assert!(presents(&mut ctx, &id(kept)), "opener {kept}");
        assert!(!presents(&mut ctx, &id(kept)), "opener {kept}, again");
    }
    let saved = save_and_restore(&mut ctx);
    assert_eq!(saved["send_file_calls"].as_array().unwrap().len(), 253);

    // An opener seen again is the newest, and is still remembered once.
    let mut ctx = new_ctx();
    for index in 0..256 {
        ingest(&mut ctx, OPENER_OFFSET + index as i64, &send(&id(index)));
    }
    ingest(&mut ctx, OPENER_OFFSET + 256, &send(&id(0)));
    ingest(&mut ctx, OPENER_OFFSET + 257, &send(&id(256)));
    let saved = save_and_restore(&mut ctx);
    let remembered = saved["send_file_calls"].as_array().unwrap();
    assert_eq!(remembered.len(), 256);
    assert_eq!(remembered[0], json!(id(2)));
    assert_eq!(remembered[254..], [json!(id(0)), json!(id(256))]);
    assert!(!presents(&mut ctx, &id(1)), "the oldest went");
    assert!(presents(&mut ctx, &id(0)), "seen again, so kept");
    assert!(!presents(&mut ctx, &id(0)), "and kept once");
}

/// The remembered ids were a set before they were ordered by age. A
/// checkpoint written then is a sorted array of ids, and still restores.
#[test]
fn a_checkpoint_from_the_set_form_restores() {
    let mut ctx = new_ctx();
    for id in ["toolu_b", "toolu_c", "toolu_a"] {
        ingest(&mut ctx, OPENER_OFFSET, &send(id));
    }
    let mut saved = save_and_restore(&mut ctx);
    assert_eq!(
        saved["send_file_calls"],
        json!(["toolu_b", "toolu_c", "toolu_a"]),
        "oldest first"
    );
    saved["send_file_calls"] = json!(["toolu_a", "toolu_b", "toolu_c"]);
    ctx.state = ClaudeCodeAdapter.restore_state(Some(&saved)).unwrap();
    for id in ["toolu_c", "toolu_a", "toolu_b"] {
        assert!(presents(&mut ctx, id), "{id}");
        assert!(!presents(&mut ctx, id), "{id}, again");
    }
    let settled = save_and_restore(&mut ctx);
    assert!(settled.get("send_file_calls").is_none(), "{settled}");

    // Such a checkpoint at the bound: its sorted order stands in for age, so
    // the next opener drops its first id and no other.
    let full: Vec<String> = (0..256).map(|index| format!("toolu_{index:04}")).collect();
    saved["send_file_calls"] = json!(full);
    ctx.state = ClaudeCodeAdapter.restore_state(Some(&saved)).unwrap();
    ingest(&mut ctx, OPENER_OFFSET, &send("toolu_0000-later"));
    let saved = save_and_restore(&mut ctx);
    let remembered = saved["send_file_calls"].as_array().unwrap();
    assert_eq!(remembered.len(), 256);
    assert_eq!(remembered[0], json!("toolu_0001"));
    assert_eq!(remembered[255], json!("toolu_0000-later"));
    assert!(!presents(&mut ctx, "toolu_0000"));
    assert!(presents(&mut ctx, "toolu_0255"));
    assert!(presents(&mut ctx, "toolu_0000-later"));
}

/// What the adapter reads is what the canonical declares, and nothing else is
/// read: `path` and `size` required, `media_type` optional.
#[test]
fn the_canonical_declares_the_three_fields_read() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../groundtruth/claude-code/groundtruth.json");
    let canonical: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let stream = canonical["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stream| stream["id"] == "transcript")
        .expect("the transcript stream");
    let facet = stream["facets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|facet| facet["id"] == "tool-result")
        .expect("the tool-result facet");
    let record = &facet["record_types"]["SendUserFile"];
    assert_eq!(record["evidence"], "observed");
    // The result is an object or a bare string; the object carries the list.
    let variants = record["shape"]["variants"].as_array().unwrap();
    assert!(variants.iter().any(|variant| variant["kind"] == "string"));
    let object = variants
        .iter()
        .find(|variant| variant["kind"] == "object")
        .expect("the object variant");
    let attachments = &object["fields"]["attachments"];
    assert_eq!(attachments["shape"]["kind"], "array");
    let item = &attachments["shape"]["items"];
    assert_eq!(item["kind"], "object");
    for (field, kind, required) in [
        ("path", "string", true),
        ("size", "number", true),
        ("media_type", "string", false),
    ] {
        let declared = &item["fields"][field];
        assert_eq!(declared["shape"]["kind"], kind, "attachments[].{field}");
        assert_eq!(declared["required"], required, "attachments[].{field}");
    }
}
