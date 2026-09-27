//! The shared encoder against the framing it replaced, kept here verbatim
//! (`Ledger::write_owned_batch`, `finish_raw_batch`, `raw_frame` and the seal
//! body of `complete_generation` as of 6a56afd, writing into a buffer).

use super::*;

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

mod legacy {
    use crate::ledger::{CHECKSUM_BYTES, TAG_GENERATION_COMPLETE, TAG_RAW, TAG_RAW_BATCH};

    const BATCH_BODY_BYTES: usize = 1024 * 1024;
    const BATCH_HEADER_BYTES: usize = 16;
    const RECORD_HEADER_BYTES: usize = 12;

    fn frame(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
        let mut head = [0_u8; 5];
        head[0] = tag;
        head[1..5].copy_from_slice(&(body.len() as u32).to_be_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(body);
        let digest = hasher.finalize();
        out.extend_from_slice(&head);
        out.extend_from_slice(body);
        out.extend_from_slice(&digest.as_bytes()[..CHECKSUM_BYTES]);
    }

    pub(super) fn raw_frame(
        out: &mut Vec<u8>,
        file_id: u32,
        generation: u64,
        start_offset: u64,
        payload: &[u8],
    ) {
        const FIXED_BYTES: usize = 20;
        let body_len = FIXED_BYTES + payload.len();
        let mut head = [0_u8; 5];
        head[0] = TAG_RAW;
        head[1..5].copy_from_slice(&(body_len as u32).to_be_bytes());
        let mut fixed = [0_u8; FIXED_BYTES];
        fixed[0..4].copy_from_slice(&file_id.to_be_bytes());
        fixed[4..12].copy_from_slice(&generation.to_be_bytes());
        fixed[12..20].copy_from_slice(&start_offset.to_be_bytes());
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        hasher.update(&fixed);
        hasher.update(payload);
        let digest = hasher.finalize();
        out.extend_from_slice(&head);
        out.extend_from_slice(&fixed);
        out.extend_from_slice(payload);
        out.extend_from_slice(&digest.as_bytes()[..CHECKSUM_BYTES]);
    }

    fn finish_raw_batch(
        out: &mut Vec<u8>,
        file_id: u32,
        generation: u64,
        body: &mut Vec<u8>,
        count: &mut u32,
    ) {
        match *count {
            0 => return,
            1 => {
                let offset = u64::from_be_bytes(body[16..24].try_into().expect("record offset"));
                raw_frame(out, file_id, generation, offset, &body[28..]);
            }
            _ => {
                body[12..16].copy_from_slice(&count.to_be_bytes());
                frame(out, TAG_RAW_BATCH, body);
            }
        }
        body.clear();
        *count = 0;
    }

    pub(super) fn owned_batch(
        out: &mut Vec<u8>,
        file_id: u32,
        generation: u64,
        records: Vec<(u64, Vec<u8>)>,
    ) {
        if records.len() <= 1 {
            for (offset, payload) in records {
                raw_frame(out, file_id, generation, offset, &payload);
            }
            return;
        }
        let mut body = Vec::new();
        let mut count = 0_u32;
        for (offset, payload) in records {
            if payload.len() > BATCH_BODY_BYTES - BATCH_HEADER_BYTES - RECORD_HEADER_BYTES {
                finish_raw_batch(out, file_id, generation, &mut body, &mut count);
                raw_frame(out, file_id, generation, offset, &payload);
                continue;
            }
            if !body.is_empty()
                && body.len() + RECORD_HEADER_BYTES + payload.len() > BATCH_BODY_BYTES
            {
                finish_raw_batch(out, file_id, generation, &mut body, &mut count);
            }
            if body.is_empty() {
                body.extend_from_slice(&file_id.to_be_bytes());
                body.extend_from_slice(&generation.to_be_bytes());
                body.extend_from_slice(&0_u32.to_be_bytes());
            }
            body.extend_from_slice(&offset.to_be_bytes());
            body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(&payload);
            count += 1;
        }
        finish_raw_batch(out, file_id, generation, &mut body, &mut count);
    }

    pub(super) fn seal(
        out: &mut Vec<u8>,
        file_id: u32,
        generation: u64,
        normalizer_version: u32,
        normalized_clean: bool,
    ) {
        let mut body = Vec::with_capacity(17);
        body.extend_from_slice(&file_id.to_be_bytes());
        body.extend_from_slice(&generation.to_be_bytes());
        body.extend_from_slice(&normalizer_version.to_be_bytes());
        body.push(u8::from(normalized_clean));
        frame(out, TAG_GENERATION_COMPLETE, &body);
    }
}

/// xorshift64*: deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// Sizes where framing decisions flip: empty, tiny, the largest payload a
/// batch still holds (1 MiB − 28 B) and one byte past it, and just over
/// 1 MiB — among mostly ordinary transcript lines. (32 MiB records are the
/// fixed cases below; drawing them here would hash gigabytes per run.)
fn payload_len(rng: &mut Rng) -> usize {
    match rng.below(16) {
        0 => 0,
        1 => 1,
        2 => MIB - 28,
        3 => MIB - 27,
        4 => MIB + rng.below(4 * KIB),
        5 => MIB / 2 + rng.below(MIB / 2),
        _ => rng.below(8 * KIB),
    }
}

fn records(rng: &mut Rng, count: usize) -> Vec<(u64, Vec<u8>)> {
    let mut offset = rng.below(1 << 20) as u64;
    (0..count)
        .map(|_| {
            let len = payload_len(rng);
            let fill = (rng.next() & 0xff) as u8;
            let record = (offset, vec![fill; len]);
            offset += len as u64 + 1;
            record
        })
        .collect()
}

fn legacy_frames(
    key: u32,
    generation: u64,
    records: Vec<(u64, Vec<u8>)>,
    seal: Option<(u32, bool)>,
) -> Vec<u8> {
    let mut out = Vec::new();
    legacy::owned_batch(&mut out, key, generation, records);
    if let Some((normalizer_version, clean)) = seal {
        legacy::seal(&mut out, key, generation, normalizer_version, clean);
    }
    out
}

fn prebuilt(
    key: u32,
    generation: u64,
    records: Vec<(u64, Vec<u8>)>,
    seal: Option<(u32, bool)>,
) -> PrebuiltFrames {
    let mut frames = PrebuiltFrames::new(key, generation);
    frames.push_records(records).unwrap();
    if let Some((normalizer_version, clean)) = seal {
        frames.push_seal(normalizer_version, clean);
    }
    frames
}

/// How many frames the legacy bytes hold, walked by their heads.
fn frame_count(mut bytes: &[u8]) -> u64 {
    let mut count = 0;
    while !bytes.is_empty() {
        let body_len = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
        bytes = &bytes[5 + body_len + CHECKSUM_BYTES..];
        count += 1;
    }
    count
}

#[test]
fn prebuilt_frames_equal_the_legacy_framing_for_random_batches() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..64 {
        let count = match round % 8 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 1 + rng.below(24),
        };
        let batch = records(&mut rng, count);
        let key = rng.next() as u32;
        let generation = rng.next() >> 1;
        let seal = (round % 3 == 0).then(|| (rng.next() as u32, round % 2 == 0));
        let expected = legacy_frames(key, generation, batch.clone(), seal);
        let frames = prebuilt(key, generation, batch, seal);
        assert_eq!(frames.bytes(), expected.as_slice(), "round {round}");
        assert_eq!(frames.frame_bytes(), expected.len() as u64);
        assert_eq!(frames.frames(), frame_count(&expected), "round {round}");
        assert_eq!(frames.seals(), seal.is_some());
        assert_eq!(frames.is_empty(), expected.is_empty());
        assert_eq!((frames.key(), frames.generation()), (key, generation));
    }
}

#[test]
fn batch_boundaries_match_at_every_flip() {
    // Batches that close exactly at 1 MiB of body, one byte past it, and a
    // lone small record between two large ones (a demoted count-1 batch).
    let exact_fit = MIB - 16 - 12;
    let cases: Vec<Vec<usize>> = vec![
        vec![exact_fit, 0],
        vec![exact_fit - 13, 1, 0],
        vec![(MIB - 16) / 2 - 12, (MIB - 16) / 2 - 12, 0],
        vec![(MIB - 16) / 2 - 12, (MIB - 16) / 2 - 11, 5],
        vec![MIB, 3, MIB],
        vec![10, MIB - 27, 10, 10],
        vec![0, 0, 0],
        vec![32 * MIB, 32 * MIB],
    ];
    for (index, sizes) in cases.into_iter().enumerate() {
        let batch: Vec<(u64, Vec<u8>)> = sizes
            .iter()
            .enumerate()
            .map(|(at, len)| (at as u64 * 7, vec![at as u8; *len]))
            .collect();
        let expected = legacy_frames(3, 9, batch.clone(), Some((4, true)));
        let frames = prebuilt(3, 9, batch, Some((4, true)));
        assert_eq!(frames.bytes(), expected.as_slice(), "case {index}");
        assert_eq!(frames.frames(), frame_count(&expected), "case {index}");
    }
}

#[test]
fn single_records_and_seals_equal_the_legacy_frames() {
    for len in [0, 1, 100, MIB - 28, MIB + 1, 32 * MIB] {
        let payload = vec![0x5a; len];
        let mut expected = Vec::new();
        legacy::raw_frame(&mut expected, 7, 2, 41, &payload);
        let mut frames = PrebuiltFrames::new(7, 2);
        frames.push_record(41, &payload).unwrap();
        assert_eq!(frames.bytes(), expected.as_slice(), "{len}");
    }
    let mut expected = Vec::new();
    legacy::seal(&mut expected, 7, 2, 11, false);
    let mut frames = PrebuiltFrames::new(7, 2);
    frames.push_seal(11, false);
    assert_eq!(frames.bytes(), expected.as_slice());
    assert!(frames.seals());
}

#[test]
fn a_record_past_the_frame_limit_fails_the_whole_batch() {
    let mut frames = PrebuiltFrames::new(1, 1);
    let error = frames
        .push_records(vec![
            (0, b"small".to_vec()),
            (6, vec![0; MAX_FRAME_BYTES - RAW_FIXED_BYTES + 1]),
        ])
        .unwrap_err();
    assert!(matches!(error, CaptureError::LimitExceeded(_)), "{error}");
}

#[test]
#[should_panic(expected = "a sealed generation takes no more frames")]
fn nothing_follows_a_seal() {
    let mut frames = PrebuiltFrames::new(1, 1);
    frames.push_seal(1, true);
    frames.push_record(0, b"late").unwrap();
}
