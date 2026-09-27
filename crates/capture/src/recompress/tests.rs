use super::*;
use crate::{Ledger, scan_ledger, sniff_segment};

fn plain_segment(dir: &Path) -> std::path::PathBuf {
    let mut ledger = Ledger::open(dir).unwrap().with_codec(SegmentCodec::Plain);
    ledger
        .append_meta(1, &"bounded compression payload ".repeat(24_000))
        .unwrap();
    ledger.flush().unwrap();
    drop(ledger);
    dir.join("00000000.rledger")
}

#[test]
fn cancellation_at_every_checkpoint_keeps_the_original_and_allows_retry() {
    let fixture = tempfile::TempDir::new().unwrap();
    let original_path = plain_segment(fixture.path());
    let original = std::fs::read(&original_path).unwrap();
    let expected = scan_ledger(fixture.path()).unwrap().entries;
    let mut checkpoints = 0;
    recompress_segment_controlled(&original_path, SegmentCodec::default(), false, &mut || {
        checkpoints += 1;
        Ok(())
    })
    .unwrap();
    assert!(checkpoints > 20, "large frames must yield within each pass");

    let trial = tempfile::TempDir::new().unwrap();
    let path = trial.path().join("00000000.rledger");
    for cancel_at in 1..=checkpoints {
        std::fs::write(&path, &original).unwrap();
        let mut calls = 0;
        let result =
            recompress_segment_controlled(&path, SegmentCodec::default(), false, &mut || {
                calls += 1;
                if calls == cancel_at {
                    Err(io::Error::other("test cancellation"))
                } else {
                    Ok(())
                }
            });
        assert!(
            result.is_err(),
            "checkpoint {cancel_at} must be cancellable"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("rledger.compact").exists());
    }
    recompress_segment(&path, SegmentCodec::default(), false).unwrap();
    assert_eq!(scan_ledger(trial.path()).unwrap().entries, expected);
}

#[test]
fn immutable_snapshot_excludes_active_and_future_segments() {
    let dir = tempfile::TempDir::new().unwrap();
    let closed = plain_segment(dir.path());
    let mut writer = Ledger::open(dir.path())
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(1);
    let candidates = writer.closed_segment_paths().unwrap();
    assert_eq!(candidates.as_slice(), std::slice::from_ref(&closed));
    writer.append_meta(1, "new active segment").unwrap();
    assert_eq!(writer.closed_segment_paths().unwrap(), candidates);
    writer.checkpoint().unwrap();
    writer.append_meta(1, "next active segment").unwrap();
    assert_eq!(
        candidates.as_slice(),
        std::slice::from_ref(&closed),
        "the handed-off snapshot cannot grow"
    );
    assert_eq!(writer.closed_segment_paths().unwrap().len(), 2);
    recompress_segment(&closed, SegmentCodec::default(), false).unwrap();
    writer.append_meta(1, "append still works").unwrap();
    writer.flush().unwrap();
    drop(writer);
    assert_eq!(
        sniff_segment(&closed).unwrap(),
        Some(SegmentCodec::default())
    );
    assert_eq!(
        sniff_segment(&dir.path().join("00000002.rledger")).unwrap(),
        Some(SegmentCodec::Plain)
    );
    let result = scan_ledger(dir.path()).unwrap();
    assert_eq!(result.entries.len(), 4);
    assert_eq!(result.torn_segments, 0);
}

#[test]
#[cfg(unix)]
fn reader_open_before_atomic_replacement_keeps_identical_frames() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = plain_segment(dir.path());
    let mut old_reader = SegmentReader::open(&path).unwrap().unwrap();
    recompress_segment(&path, SegmentCodec::default(), false).unwrap();
    let mut new_reader = SegmentReader::open(&path).unwrap().unwrap();
    let old = ledger::frame_prefix(&mut old_reader).unwrap();
    let new = ledger::frame_prefix(&mut new_reader).unwrap();
    assert!(old.clean && new.clean);
    assert_eq!(old.bytes, new.bytes);
    assert_eq!(old.digest, new.digest);
}

#[test]
fn a_source_that_grows_during_compression_is_not_replaced() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = plain_segment(dir.path());
    let original = std::fs::read(&path).unwrap();
    let mut changed = false;
    let result = recompress_segment_controlled(&path, SegmentCodec::default(), false, &mut || {
        if !changed && path.with_extension("rledger.compact").exists() {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)?
                .write_all(b"new tail")?;
            changed = true;
        }
        Ok(())
    });
    assert!(changed && result.is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        [original.as_slice(), b"new tail"].concat()
    );
    assert!(!path.with_extension("rledger.compact").exists());
}
