use super::*;
use rosetta_core::assets::ImageVariant;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_core::timeline::{AssetRef, AttachmentKind, AttachmentRef, AttachmentSource};
use rosetta_core::{IngestOp, ProjectionAsset};
use rosetta_store::{Batch, Store};
use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::sync::Mutex;

/// The codec pool is process-wide: tests that count its jobs or hold its
/// slots take this first.
static CODEC: Mutex<()> = Mutex::new(());

fn png(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let pixels = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 251) as u8 ^ seed, (y % 241) as u8, seed, 255])
    });
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

/// A PNG whose header is sound but whose pixel data is not.
fn corrupt_png(seed: u8) -> Vec<u8> {
    let mut bytes = png(40, 30, seed);
    let at = bytes.windows(4).position(|w| w == b"IDAT").unwrap() + 6;
    for byte in &mut bytes[at..at + 8] {
        *byte ^= 0x5a;
    }
    bytes
}

fn session(id: &str, updated_at: i64) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt".into(),
        project_id: Some("p".into()),
        cwd: Some("/w/p".into()),
        parent_session_id: None,
        delegation: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: updated_at,
        updated_at,
        activity: None,
        stream_state: StreamState::Indexed,
    }
}

fn message(session_id: &str, ordinal: i64, attachments: Vec<AttachmentRef>) -> MessageRow {
    MessageRow {
        timeline_id: format!("{session_id}-{ordinal}"),
        session_id: session_id.into(),
        ordinal,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(1_000 + ordinal),
        model: None,
        effort: None,
        content: Some("look".into()),
        tool: None,
        attachments,
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    }
}

fn attachment(asset: &AssetRef, bytes: &[u8]) -> AttachmentRef {
    let mut value = AttachmentRef::new(
        &asset.digest[..12],
        AttachmentKind::Image,
        AttachmentSource::Asset {
            asset: asset.clone(),
        },
    );
    value.media_type = Some(asset.media_type.clone());
    value.byte_len = Some(asset.byte_len);
    value.dimensions = rosetta_media::probe(bytes).ok().map(|p| p.dimensions);
    value
}

/// A file projection whose sessions (id, updated_at) each hold one message
/// naming their images as recorded-original members.
fn projection(dir: &Path, sessions: &[(&str, i64, Vec<Vec<u8>>)]) -> PathBuf {
    let db = dir.join("projection.sqlite");
    let mut store = Store::open(&db).unwrap();
    let mut ops = vec![
        IngestOp::Runtime(RuntimeRow {
            id: "rt".into(),
            kind: "claude-code".into(),
            data_root: "~/.claude".into(),
            enabled: true,
            account: None,
            config: None,
            skills: None,
            versions_seen: None,
            discovered_at: 1,
            label: None,
            provenance: Default::default(),
        }),
        IngestOp::Project(ProjectRow {
            id: "p".into(),
            name: "p".into(),
            root_paths: vec!["/w/p".into()],
            git_repo: None,
        }),
    ];
    for (id, updated_at, _) in sessions {
        ops.push(IngestOp::SessionMeta(session(id, *updated_at)));
    }
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();
    for (id, _, images) in sessions {
        let mut ops = Vec::new();
        let mut attachments = Vec::new();
        for bytes in images {
            let asset = ProjectionAsset::new("image/png", bytes.clone()).unwrap();
            attachments.push(attachment(&asset.reference(), bytes));
            ops.push(IngestOp::ProjectionAsset { asset });
        }
        ops.extend(IngestOp::project_message(message(id, 0, attachments)));
        store
            .apply(&Batch {
                ops,
                ..Batch::default()
            })
            .unwrap();
    }
    drop(store);
    db
}

fn member(bytes: &[u8]) -> String {
    format!("{}.png", blake3::hash(bytes).to_hex())
}

fn prepared(bytes: &[u8], variant: ImageVariant) -> Vec<u8> {
    let prepared = rosetta_media::prepare(bytes).unwrap();
    if variant == ImageVariant::Full {
        return prepared.asset.bytes.clone();
    }
    prepared
        .asset
        .image_variants
        .iter()
        .find(|v| v.variant == variant)
        .unwrap()
        .bytes
        .clone()
}

/// Never paces, never gates; errs after `allowed` checkpoints if given.
struct Ungated {
    allowed: Option<usize>,
}
impl Checkpoint for Ungated {
    fn checkpoint(&mut self) -> io::Result<()> {
        match &mut self.allowed {
            Some(0) => Err(io::Error::other("stopped")),
            Some(left) => {
                *left -= 1;
                Ok(())
            }
            None => Ok(()),
        }
    }
    fn idle(&mut self) {}
}

fn run(db: &Path, allowed: Option<usize>) -> (&'static str, Vec<Value>) {
    run_with(db, allowed, &|| false)
}

fn run_with(
    db: &Path,
    allowed: Option<usize>,
    stopped: &dyn Fn() -> bool,
) -> (&'static str, Vec<Value>) {
    let mut events = Vec::new();
    let state = sweep(db, &mut Ungated { allowed }, stopped, &mut |e| {
        events.push(e)
    });
    (state, events)
}

fn tally(event: &Value) -> [u64; 8] {
    [
        "bases", "complete", "made", "busy", "failed", "missing", "files", "bytes",
    ]
    .map(|key| event[key].as_u64().unwrap())
}

#[test]
fn the_sweep_makes_every_base_and_a_warm_start_reads_none() {
    let _codec = CODEC.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (small, tall, wide) = (png(300, 200, 1), png(200, 900, 2), png(2000, 300, 3));
    let db = projection(
        dir.path(),
        &[
            ("older", 10_000, vec![small.clone(), tall.clone()]),
            ("newer", 20_000, vec![wide.clone(), small.clone()]),
        ],
    );
    let root = rosetta_store::assets::asset_root(&db);
    let old_temp = root.join(".image-left-by-a-crash");
    fs::write(&old_temp, b"partial").unwrap();
    fs::File::options()
        .write(true)
        .open(&old_temp)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    let projection_bytes = fs::read(&db).unwrap();

    // Newest session first; one base per image however often it occurs.
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut newer = vec![member(&wide), member(&small)];
    newer.sort();
    let expected: Vec<_> = newer.into_iter().chain([member(&tall)]).collect();
    let order: Vec<_> = image_work(&conn, &|| false)
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|w| w.name)
        .collect();
    assert_eq!(order, expected);
    drop(conn);

    let (state, events) = run(&db, None);
    assert_eq!(state, "finished");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["event"], "image-renditions");
    assert_eq!(events[0]["state"], "started");
    assert_eq!(events[0]["cpuBudgetPercent"], 50);
    assert_eq!(tally(&events[0])[..2], [3, 0]);
    assert_eq!(events[1]["state"], "finished");
    let [bases, complete, made, busy, failed, missing, files, bytes] = tally(&events[1]);
    // Each image at most 1600 px gets rail, tile2x, preview, full (a link)
    // and tile; the wide one has no Full until its first 1:1 view.
    assert_eq!(
        (bases, complete, made, busy, failed, missing),
        (3, 0, 3, 0, 0, 0)
    );
    assert_eq!(files, 5 + 5 + 4);
    assert!(bytes > 0);
    assert!(!old_temp.exists(), "stale tempfiles go first");
    for source in [&small, &tall, &wide] {
        let digest = blake3::hash(source).to_hex().to_string();
        for variant in [
            ImageVariant::Rail,
            ImageVariant::Tile,
            ImageVariant::Tile2x,
            ImageVariant::Preview,
        ] {
            assert_eq!(
                fs::read(root.join(variant.file_name(&digest))).unwrap(),
                prepared(source, variant),
                "{variant:?}"
            );
        }
    }
    let wide_digest = blake3::hash(&wide).to_hex().to_string();
    assert!(
        !root
            .join(ImageVariant::Full.file_name(&wide_digest))
            .exists()
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        projection_bytes,
        "the sweep never writes SQLite"
    );

    // A warm start finds every base complete by stat alone: with the bases
    // gone, any read would count them missing.
    for source in [&small, &tall, &wide] {
        fs::remove_file(root.join(member(source))).unwrap();
    }
    let (state, events) = run(&db, None);
    assert_eq!(state, "finished");
    assert_eq!(tally(&events[1])[..6], [3, 3, 0, 0, 0, 0]);
}

#[test]
fn missing_and_undecodable_bases_are_counted_and_skipped() {
    let _codec = CODEC.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let (good, gone, broken) = (png(64, 64, 4), png(64, 64, 5), corrupt_png(6));
    let db = projection(
        dir.path(),
        &[("s", 1, vec![good.clone(), gone.clone(), broken.clone()])],
    );
    let root = rosetta_store::assets::asset_root(&db);
    fs::remove_file(root.join(member(&gone))).unwrap();
    let (state, events) = run(&db, None);
    assert_eq!(state, "finished");
    assert_eq!(tally(&events[1])[..6], [3, 0, 1, 0, 1, 1]);
    let left: BTreeSet<_> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(&blake3::hash(&broken).to_hex().to_string()))
        .collect();
    assert_eq!(
        left,
        [member(&broken)].into(),
        "a failed base keeps no cache"
    );
    // The next start retries what failed, and only that.
    let (_, events) = run(&db, None);
    assert_eq!(tally(&events[1])[..6], [3, 1, 0, 0, 1, 1]);
}

#[test]
fn a_stop_ends_the_sweep_between_images() {
    let _codec = CODEC.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db = projection(
        dir.path(),
        &[("s", 1, vec![png(50, 50, 7), png(50, 50, 8), png(50, 50, 9)])],
    );
    // The gate, then one image.
    let (state, events) = run(&db, Some(2));
    assert_eq!(state, "stopped");
    assert_eq!(events[1]["state"], "stopped");
    assert_eq!(tally(&events[1])[2], 1);
    // Stopped at the gate: no work list, no events.
    let (state, events) = run(&db, Some(0));
    assert_eq!((state, events.len()), ("stopped", 0));
}

#[test]
fn a_stop_after_the_gate_ends_the_work_list_scan() {
    let _codec = CODEC.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let images = vec![png(50, 50, 13), png(50, 50, 14)];
    let db = projection(dir.path(), &[("s", 1, images.clone())]);
    let root = rosetta_store::assets::asset_root(&db);
    // The gate opens, then stop arrives: the scan and the stat pass end at
    // their next page or base, and nothing is announced or made.
    for allowed_polls in [0, 1, 2] {
        let polls = std::cell::Cell::new(0);
        let stopped = || {
            polls.set(polls.get() + 1);
            polls.get() > allowed_polls
        };
        let (state, events) = run_with(&db, None, &stopped);
        assert_eq!((state, events.len()), ("stopped", 0), "{allowed_polls}");
    }
    for bytes in &images {
        let digest = blake3::hash(bytes).to_hex().to_string();
        assert!(!root.join(ImageVariant::Tile.file_name(&digest)).exists());
    }
}

#[cfg(unix)]
#[test]
fn an_unwritable_cache_defers_the_run() {
    use std::os::unix::fs::PermissionsExt;
    let _codec = CODEC.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db = projection(
        dir.path(),
        &[("s", 1, vec![png(50, 40, 10), png(50, 40, 11)])],
    );
    let root = rosetta_store::assets::asset_root(&db);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(root.join("probe"), b"x").is_ok() {
        // Running as root: permissions do not bind.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let (state, events) = run(&db, None);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(state, "deferred");
    assert_eq!(events[1]["state"], "deferred");
    assert!(events[1]["error"].as_str().is_some());
    assert_eq!(tally(&events[1])[2], 0);
}

#[test]
fn drop_returns_promptly_while_search_is_not_ready() {
    let dir = tempfile::tempdir().unwrap();
    let db = projection(dir.path(), &[("s", 1, vec![png(30, 30, 12)])]);
    let (asked_tx, asked_rx) = std::sync::mpsc::channel();
    let worker = BackgroundRenditions::start(db.clone(), move || {
        let _ = asked_tx.send(());
        false
    })
    .unwrap();
    asked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = Instant::now();
    drop(worker);
    assert!(started.elapsed() < Duration::from_secs(2));
    let digest = blake3::hash(&png(30, 30, 12)).to_hex().to_string();
    let root = rosetta_store::assets::asset_root(&db);
    assert!(!root.join(ImageVariant::Tile.file_name(&digest)).exists());
}

#[test]
fn a_waiting_admission_sees_stop() {
    let _codec = CODEC.lock().unwrap_or_else(|p| p.into_inner());
    // Hold every slot the background lane could use.
    let held = rosetta_media::admit(Lane::Foreground);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let waiter = thread::spawn(move || {
        background_slot(&mut Ungated { allowed: None }, &|| {
            flag.load(Ordering::Acquire)
        })
        .is_some()
    });
    thread::sleep(Duration::from_millis(100));
    stop.store(true, Ordering::Release);
    waiter.thread().unpark();
    let started = Instant::now();
    assert!(!waiter.join().unwrap(), "no slot after stop");
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(held);
}

#[test]
fn the_hidden_flag_holds_the_sweep() {
    assert!(crate::parse_flags(&[]).unwrap().image_renditions);
    let off = ["--image-renditions".to_string(), "off".into()];
    assert!(!crate::parse_flags(&off).unwrap().image_renditions);
    let on = ["--image-renditions".to_string(), "on".into()];
    assert!(crate::parse_flags(&on).unwrap().image_renditions);
    let bad = ["--image-renditions".to_string(), "maybe".into()];
    assert!(crate::parse_flags(&bad).is_err());

    let dir = tempfile::tempdir().unwrap();
    let db = projection(dir.path(), &[]);
    let conn = Connection::open(&db).unwrap();
    let search = Fts5SidecarSearch::new(&db);
    let running = AtomicBool::new(false);
    assert!(start_after_catch_up(false, &conn, &search, &running).is_none());
    assert!(start_after_catch_up(true, &conn, &search, &AtomicBool::new(true)).is_none());
    let memory = Connection::open_in_memory().unwrap();
    assert!(start_after_catch_up(true, &memory, &search, &running).is_none());
    assert!(start_after_catch_up(true, &conn, &search, &running).is_some());
}
