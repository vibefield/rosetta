//! The display size of recorded video, read from hand-assembled boxes. No
//! real file, encoder or filesystem is involved.

use rosetta_core::timeline::Dimensions;

use crate::{MediaError, VideoProbe, probe_video, video_media_type};

/// A box with a 32-bit size.
fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = ((8 + body.len()) as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(body);
    bytes
}

/// A box whose 32-bit size is 1, with the real size in 64 bits after the type.
fn large(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = 1_u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(&((16 + body.len()) as u64).to_be_bytes());
    bytes.extend_from_slice(body);
    bytes
}

/// A box whose size is 0: it runs to the end of whatever holds it.
fn open(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = 0_u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(body);
    bytes
}

/// A box header that claims `size` bytes, followed by `body`.
fn claiming(size: u32, kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = size.to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(body);
    bytes
}

/// A 64-bit box header that claims `size` bytes, followed by `body`.
fn claiming_large(size: u64, kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = 1_u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(body);
    bytes
}

fn join(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
    let mut body = major.to_vec();
    body.extend_from_slice(&[0, 0, 2, 0]);
    for brand in compatible {
        body.extend_from_slice(*brand);
    }
    boxed(b"ftyp", &body)
}

/// The `ftyp` box ffmpeg writes, as both local videos carry it.
fn isom() -> Vec<u8> {
    ftyp(b"isom", &[b"isom", b"iso2", b"avc1", b"mp41"])
}

/// Whole pixels as 16.16 fixed point.
fn px(pixels: u32) -> u32 {
    pixels << 16
}

/// One and minus one in the matrix's 16.16 fixed point.
const ONE: u32 = 0x0001_0000;
const NEG: u32 = 0xffff_0000;
/// The matrix cells `[a, b, c, d]` of a track shown as stored, and of the
/// three turns as ffmpeg 9 writes them for `-display_rotation 90`, `180` and
/// `270` (read once from clips it made; it writes no translation).
const UPRIGHT: [u32; 4] = [ONE, 0, 0, ONE];
const TURN_90: [u32; 4] = [0, NEG, ONE, 0];
const TURN_180: [u32; 4] = [NEG, 0, 0, NEG];
const TURN_270: [u32; 4] = [0, ONE, NEG, 0];

/// A `tkhd` body with an identity matrix.
fn tkhd_body(version: u8, width: u32, height: u32) -> Vec<u8> {
    tkhd_body_with(version, UPRIGHT, [0, 0], width, height)
}

/// A `tkhd` body whose matrix is `a b 0 / c d 0 / x y 1`. Every field before
/// the size is non-zero where the format allows, so a read at the wrong
/// offset cannot land on a plausible value.
fn tkhd_body_with(
    version: u8,
    [a, b, c, d]: [u32; 4],
    [x, y]: [u32; 2],
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut body = vec![version, 0, 0, 3];
    if version == 1 {
        body.extend_from_slice(&0x0000_0000_e1b2_c3d4_u64.to_be_bytes()); // creation
        body.extend_from_slice(&0x0000_0000_e1b2_c3d5_u64.to_be_bytes()); // modification
        body.extend_from_slice(&7_u32.to_be_bytes()); // track id
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&0x0000_0001_0002_0003_u64.to_be_bytes()); // duration
    } else {
        body.extend_from_slice(&0xe1b2_c3d4_u32.to_be_bytes());
        body.extend_from_slice(&0xe1b2_c3d5_u32.to_be_bytes());
        body.extend_from_slice(&7_u32.to_be_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&0x0001_0203_u32.to_be_bytes());
    }
    body.extend_from_slice(&[0; 8]);
    body.extend_from_slice(&[0, 0, 0, 0, 1, 0, 0, 0]); // layer, group, volume
    for cell in [a, b, 0, c, d, 0, x, y, 0x4000_0000] {
        body.extend_from_slice(&cell.to_be_bytes());
    }
    body.extend_from_slice(&width.to_be_bytes());
    body.extend_from_slice(&height.to_be_bytes());
    body
}

fn tkhd(version: u8, width: u32, height: u32) -> Vec<u8> {
    boxed(b"tkhd", &tkhd_body(version, width, height))
}

/// A track: its header, then a media box the probe never opens.
fn trak(header: &[u8]) -> Vec<u8> {
    boxed(b"trak", &join(&[header, &boxed(b"mdia", &[0xab; 24])]))
}

fn video_track(version: u8, width: u32, height: u32) -> Vec<u8> {
    trak(&tkhd(version, px(width), px(height)))
}

/// A video track stored `width` by `height` under this matrix.
fn turned_track(version: u8, matrix: [u32; 4], width: u32, height: u32) -> Vec<u8> {
    let body = tkhd_body_with(version, matrix, [0, 0], px(width), px(height));
    trak(&boxed(b"tkhd", &body))
}

/// An audio track records no size.
fn audio_track() -> Vec<u8> {
    trak(&tkhd(0, 0, 0))
}

fn mvhd() -> Vec<u8> {
    boxed(b"mvhd", &[0x11; 100])
}

fn moov(children: &[&[u8]]) -> Vec<u8> {
    boxed(b"moov", &join(children))
}

fn size(width: u32, height: u32) -> Option<Dimensions> {
    Some(Dimensions { width, height })
}

/// The size of bytes that are a video, checked against the probe's two
/// promises: the type is the type check's, and a size is never zero.
fn probed(bytes: &[u8]) -> Option<Dimensions> {
    let probe = probe_video(bytes).expect("the bytes are a video");
    assert_eq!(Some(probe.media_type), video_media_type(bytes));
    if let Some(found) = probe.dimensions {
        assert!(found.width > 0 && found.height > 0, "{found:?}");
    }
    probe.dimensions
}

#[test]
fn the_size_is_read_when_moov_follows_ftyp() {
    let video = join(&[
        &isom(),
        &moov(&[&mvhd(), &video_track(0, 1920, 1080)]),
        &boxed(b"mdat", &[0xcd; 512]),
    ]);
    assert_eq!(
        probe_video(&video),
        Ok(VideoProbe {
            media_type: "video/mp4",
            dimensions: size(1920, 1080),
        })
    );
}

#[test]
fn the_size_is_read_when_moov_follows_a_large_mdat() {
    let video = join(&[
        &isom(),
        &boxed(b"free", &[]),
        &boxed(b"mdat", &vec![0xcd; 1024 * 1024]),
        &moov(&[&mvhd(), &video_track(0, 1280, 720)]),
    ]);
    assert_eq!(probed(&video), size(1280, 720));
}

#[test]
fn a_64_bit_box_size_is_followed_at_every_depth() {
    let track = video_track(0, 640, 360);
    for (name, video) in [
        (
            "mdat before moov",
            join(&[&isom(), &large(b"mdat", &[0xcd; 300]), &moov(&[&track])]),
        ),
        (
            "moov",
            join(&[&isom(), &large(b"moov", &join(&[&mvhd(), &track]))]),
        ),
        (
            "trak",
            join(&[
                &isom(),
                &moov(&[&large(b"trak", &tkhd(0, px(640), px(360)))]),
            ]),
        ),
        (
            "tkhd",
            join(&[
                &isom(),
                &moov(&[&trak(&large(b"tkhd", &tkhd_body(0, px(640), px(360))))]),
            ]),
        ),
        (
            "a sibling before trak",
            join(&[&isom(), &moov(&[&large(b"udta", &[0x22; 40]), &track])]),
        ),
    ] {
        assert_eq!(probed(&video), size(640, 360), "{name}");
    }
}

#[test]
fn a_zero_size_box_runs_to_the_end_of_what_holds_it() {
    let track = video_track(0, 1920, 1080);
    // The last top-level box, holding the tracks.
    let last_moov = join(&[
        &isom(),
        &boxed(b"mdat", &[0xcd; 64]),
        &open(b"moov", &join(&[&mvhd(), &track])),
    ]);
    assert_eq!(probed(&last_moov), size(1920, 1080));
    // The last top-level box, after the tracks.
    let last_mdat = join(&[&isom(), &moov(&[&track]), &open(b"mdat", &[0xcd; 64])]);
    assert_eq!(probed(&last_mdat), size(1920, 1080));
    // It swallows everything after it, a moov included.
    let swallowed = join(&[&isom(), &open(b"mdat", &moov(&[&track]))]);
    assert_eq!(probed(&swallowed), None);
    // Inside moov it ends with moov, not with the file: the track header is
    // the open track's whole body, and the mdat after moov is not part of it.
    let open_track = join(&[
        &isom(),
        &moov(&[&mvhd(), &open(b"trak", &tkhd(1, px(800), px(600)))]),
        &boxed(b"mdat", &[0xcd; 64]),
    ]);
    assert_eq!(probed(&open_track), size(800, 600));
    let open_header = join(&[
        &isom(),
        &moov(&[&boxed(
            b"trak",
            &open(b"tkhd", &tkhd_body(0, px(800), px(600))),
        )]),
        &boxed(b"mdat", &[0xcd; 64]),
    ]);
    assert_eq!(probed(&open_header), size(800, 600));
    // An open box that is not the last one hides its later siblings.
    let hidden = join(&[
        &isom(),
        &moov(&[&open(b"udta", &[0x22; 12]), &track]),
        &boxed(b"mdat", &[0xcd; 64]),
    ]);
    assert_eq!(probed(&hidden), None);
}

#[test]
fn a_quicktime_file_reports_its_size() {
    let video = join(&[
        &ftyp(b"qt  ", &[b"qt  "]),
        &boxed(b"wide", &[]),
        &boxed(b"mdat", &[0xcd; 128]),
        &moov(&[&mvhd(), &video_track(0, 3024, 1964)]),
    ]);
    assert_eq!(
        probe_video(&video),
        Ok(VideoProbe {
            media_type: "video/quicktime",
            dimensions: size(3024, 1964),
        })
    );
}

#[test]
fn both_track_header_versions_are_read() {
    assert_eq!(tkhd_body(0, 0, 0).len(), 84);
    assert_eq!(tkhd_body(1, 0, 0).len(), 96);
    for (version, width, height) in [(0, 1920, 1080), (1, 1080, 1920), (0, 1, 1), (1, 65535, 2)] {
        let video = join(&[&isom(), &moov(&[&video_track(version, width, height)])]);
        assert_eq!(probed(&video), size(width, height), "version {version}");
    }
    // The fraction of a 16.16 value is dropped.
    let fractional = join(&[
        &isom(),
        &moov(&[&trak(&tkhd(0, px(1920) | 0x8000, px(1080) | 0xffff))]),
    ]);
    assert_eq!(probed(&fractional), size(1920, 1080));
}

#[test]
fn an_upright_or_half_turned_track_keeps_its_stored_size() {
    for version in [0, 1] {
        for (name, matrix) in [("upright", UPRIGHT), ("180 degrees", TURN_180)] {
            let video = join(&[
                &isom(),
                &moov(&[&mvhd(), &turned_track(version, matrix, 1920, 1080)]),
            ]);
            assert_eq!(probed(&video), size(1920, 1080), "{name}, v{version}");
        }
    }
    // A translation moves the picture and leaves its size alone: a half turn
    // about the frame's centre carries one.
    let about_the_centre = tkhd_body_with(0, TURN_180, [px(640), px(360)], px(640), px(360));
    let video = join(&[&isom(), &moov(&[&trak(&boxed(b"tkhd", &about_the_centre))])]);
    assert_eq!(probed(&video), size(640, 360));
}

#[test]
fn a_quarter_turned_track_reports_its_size_as_displayed() {
    for version in [0, 1] {
        for (name, matrix) in [
            ("90 degrees", TURN_90),
            ("270 degrees", TURN_270),
            // Not a pure turn, but rows still become columns.
            ("a transpose", [0, ONE, ONE, 0]),
            ("a turn and a scale", [0, 2 * ONE, NEG, 0]),
            ("the smallest off-diagonal cells", [0, 1, 1, 0]),
        ] {
            let video = join(&[
                &isom(),
                &moov(&[&mvhd(), &turned_track(version, matrix, 1920, 1080)]),
            ]);
            assert_eq!(probed(&video), size(1080, 1920), "{name}, v{version}");
        }
    }
    // A portrait phone recording as a camera app writes it: stored
    // landscape, turned 90 degrees and moved back into view.
    let phone = tkhd_body_with(0, [0, ONE, NEG, 0], [px(1080), 0], px(1920), px(1080));
    let video = join(&[
        &ftyp(b"qt  ", &[b"qt  "]),
        &boxed(b"wide", &[]),
        &boxed(b"mdat", &[0xcd; 64]),
        &moov(&[&mvhd(), &audio_track(), &trak(&boxed(b"tkhd", &phone))]),
    ]);
    assert_eq!(
        probe_video(&video),
        Ok(VideoProbe {
            media_type: "video/quicktime",
            dimensions: size(1080, 1920),
        })
    );
    // The fraction is dropped before the swap, and a side that is zero once
    // dropped still means no size.
    let fractional = tkhd_body_with(1, TURN_90, [0, 0], px(1920) | 0x8000, px(1080) | 0xffff);
    let video = join(&[&isom(), &moov(&[&trak(&boxed(b"tkhd", &fractional))])]);
    assert_eq!(probed(&video), size(1080, 1920));
    let no_height = tkhd_body_with(0, TURN_90, [0, 0], px(1920), 0xffff);
    let video = join(&[&isom(), &moov(&[&trak(&boxed(b"tkhd", &no_height))])]);
    assert_eq!(probed(&video), None);
}

#[test]
fn a_track_under_any_other_matrix_has_no_size() {
    let cases: [(&str, [u32; 4]); 16] = [
        ("a horizontal mirror", [NEG, 0, 0, ONE]),
        ("a vertical mirror", [ONE, 0, 0, NEG]),
        ("a scale by two", [2 * ONE, 0, 0, 2 * ONE]),
        ("a scale by a half", [ONE / 2, 0, 0, ONE / 2]),
        ("a stretch", [ONE, 0, 0, 2 * ONE]),
        ("a hair off the identity", [ONE + 1, 0, 0, ONE]),
        ("a hair off a half turn", [NEG, 0, 0, NEG + 1]),
        ("a shear along x", [ONE, 0, ONE / 4, ONE]),
        ("a shear along y", [ONE, ONE / 4, 0, ONE]),
        ("a 45 degree turn", [0xb505, 0xb505, 0xffff_4afb, 0xb505]),
        ("all zero", [0, 0, 0, 0]),
        ("a quarter turn with no b", [0, 0, ONE, 0]),
        ("a quarter turn with no c", [0, NEG, 0, 0]),
        ("a quarter turn with a diagonal cell", [ONE, NEG, ONE, 0]),
        (
            "a quarter turn with the other diagonal cell",
            [0, NEG, ONE, 1],
        ),
        ("every cell set", [ONE, ONE, ONE, ONE]),
    ];
    for version in [0, 1] {
        for (name, matrix) in cases {
            let odd = turned_track(version, matrix, 1920, 1080);
            let alone = join(&[&isom(), &moov(&[&mvhd(), &odd])]);
            assert_eq!(probed(&alone), None, "{name}, v{version}");
            // The first sized track decides. A later one is not read in its
            // place, or a thumbnail track's size would stand for the film's.
            let followed = join(&[&isom(), &moov(&[&odd, &video_track(0, 1280, 720)])]);
            assert_eq!(probed(&followed), None, "{name}, v{version}, then video");
            // Acceptance never looks at the matrix.
            assert_eq!(video_media_type(&alone), Some("video/mp4"), "{name}");
        }
    }
    // A track with no size is still passed over, whatever its matrix says.
    let unsized_odd = trak(&boxed(
        b"tkhd",
        &tkhd_body_with(0, [2 * ONE, 0, 0, 2 * ONE], [0, 0], 0, 0),
    ));
    let video = join(&[
        &isom(),
        &moov(&[&unsized_odd, &turned_track(0, TURN_270, 1280, 720)]),
    ]);
    assert_eq!(probed(&video), size(720, 1280));
}

#[test]
fn a_track_with_no_size_before_the_video_track_is_passed_over() {
    let video = join(&[
        &isom(),
        &moov(&[
            &mvhd(),
            &audio_track(),
            &video_track(0, 1920, 1080),
            &video_track(0, 320, 240),
        ]),
    ]);
    assert_eq!(probed(&video), size(1920, 1080), "the first sized track");
}

#[test]
fn a_file_with_no_video_track_has_no_size() {
    let track = video_track(0, 1920, 1080);
    let header = tkhd(0, px(1920), px(1080));
    for (name, rest) in [
        ("no moov", boxed(b"mdat", &[0xcd; 64])),
        ("nothing after ftyp", Vec::new()),
        ("an empty moov", moov(&[])),
        ("a moov with no track", moov(&[&mvhd()])),
        (
            "audio tracks only",
            moov(&[&mvhd(), &audio_track(), &audio_track()]),
        ),
        (
            "a track with no header",
            moov(&[&boxed(b"trak", &boxed(b"mdia", &[0xab; 24]))]),
        ),
        ("a header outside any track", moov(&[&mvhd(), &header])),
        ("a track outside moov", track.clone()),
        (
            "a track below moov's children",
            moov(&[&boxed(b"udta", &track)]),
        ),
        (
            "a header below a track's children",
            moov(&[&boxed(b"trak", &boxed(b"mdia", &header))]),
        ),
        // The first moov decides; a file has one.
        (
            "a sized track in a second moov",
            join(&[&moov(&[&audio_track()]), &moov(&[&track])]),
        ),
    ] {
        assert_eq!(probed(&join(&[&isom(), &rest])), None, "{name}");
    }
}

#[test]
fn a_track_header_that_proves_no_size_is_never_reported() {
    let mut second_version = tkhd_body(0, px(1920), px(1080));
    second_version[0] = 2;
    let mut mislabeled = tkhd_body(0, px(1920), px(1080));
    mislabeled[0] = 1;
    let mut padded = tkhd_body(0, px(1920), px(1080));
    padded.extend_from_slice(&[0; 4]);
    let short = tkhd_body(1, px(1920), px(1080))[..92].to_vec();
    let headers: Vec<(&str, Vec<u8>)> = vec![
        ("zero width and height", tkhd(0, 0, 0)),
        ("zero width", tkhd(0, 0, px(1080))),
        ("zero height", tkhd(1, px(1920), 0)),
        ("a width below one pixel", tkhd(0, 0x8000, px(1080))),
        ("a height below one pixel", tkhd(1, px(1920), 0xffff)),
        ("an empty header", boxed(b"tkhd", &[])),
        ("version and flags only", boxed(b"tkhd", &[0, 0, 0, 3])),
        (
            "eight bytes that read as a size",
            boxed(
                b"tkhd",
                &join(&[&px(240).to_be_bytes(), &px(160).to_be_bytes()]),
            ),
        ),
        ("an unknown version", boxed(b"tkhd", &second_version)),
        (
            "version 1 with the version 0 length",
            boxed(b"tkhd", &mislabeled),
        ),
        ("bytes after the height", boxed(b"tkhd", &padded)),
        ("a header cut short", boxed(b"tkhd", &short)),
    ];
    for (name, header) in &headers {
        let alone = join(&[&isom(), &moov(&[&mvhd(), &trak(header)])]);
        assert_eq!(probed(&alone), None, "{name}");
        // Such a track does not hide a sized one after it.
        let followed = join(&[&isom(), &moov(&[&trak(header), &video_track(0, 1280, 720)])]);
        assert_eq!(probed(&followed), size(1280, 720), "{name}, then video");
    }
}

#[test]
fn every_prefix_of_a_video_is_probed_without_a_panic() {
    let head = isom();
    let movie = moov(&[&mvhd(), &audio_track(), &video_track(1, 1920, 1080)]);
    let video = join(&[&head, &movie, &boxed(b"mdat", &[0xcd; 64])]);
    for end in 0..=video.len() {
        let prefix = &video[..end];
        match video_media_type(prefix) {
            None => {
                assert!(end < head.len(), "{end} bytes");
                assert_eq!(probe_video(prefix), Err(MediaError::Unsupported));
            }
            Some(media_type) => {
                // The size appears with the last byte of moov, never before.
                let whole_moov = end >= head.len() + movie.len();
                let dimensions = whole_moov.then_some(Dimensions {
                    width: 1920,
                    height: 1080,
                });
                assert_eq!(
                    probe_video(prefix),
                    Ok(VideoProbe {
                        media_type,
                        dimensions
                    }),
                    "{end} bytes"
                );
            }
        }
    }

    // The same with moov last and open-ended: a cut can only lose the size.
    let open_ended = join(&[
        &head,
        &large(b"mdat", &[0xcd; 64]),
        &open(
            b"moov",
            &join(&[&mvhd(), &audio_track(), &video_track(0, 1920, 1080)]),
        ),
    ]);
    for end in head.len()..open_ended.len() {
        assert_eq!(probed(&open_ended[..end]), None, "{end} bytes");
    }
    assert_eq!(probed(&open_ended), size(1920, 1080));
}

#[test]
fn every_one_byte_change_to_a_video_is_probed_without_a_panic() {
    let video = join(&[
        &isom(),
        &large(b"free", &[0; 4]),
        &moov(&[&mvhd(), &audio_track(), &video_track(1, 1920, 1080)]),
        &open(b"mdat", &[0xcd; 16]),
    ]);
    assert_eq!(probed(&video), size(1920, 1080));
    let mut bytes = video.clone();
    for at in 0..video.len() {
        for value in 0..=u8::MAX {
            bytes[at] = value;
            match probe_video(&bytes) {
                Ok(probe) => {
                    assert_eq!(Some(probe.media_type), video_media_type(&bytes));
                    if let Some(found) = probe.dimensions {
                        assert!(
                            found.width > 0 && found.height > 0,
                            "byte {at} = {value}: {found:?}"
                        );
                    }
                }
                Err(error) => {
                    assert_eq!(error, MediaError::Unsupported);
                    assert_eq!(video_media_type(&bytes), None, "byte {at} = {value}");
                }
            }
        }
        bytes[at] = video[at];
    }
}

#[test]
fn a_box_size_that_overflows_or_lies_ends_the_walk() {
    let track = video_track(0, 1920, 1080);
    let movie = moov(&[&track]);
    let head = isom();
    let at = head.len() as u64;
    let cases: Vec<(String, Vec<u8>)> = [
        ("the largest 64-bit size", u64::MAX),
        (
            "a 64-bit size that wraps to the next box",
            u64::MAX - at + 17,
        ),
        ("a 64-bit size that ends at the last offset", u64::MAX - at),
        ("a 64-bit size past the slice", 1 << 40),
        (
            "a 64-bit size one byte past the slice",
            16 + 8 + movie.len() as u64 + 1,
        ),
    ]
    .into_iter()
    .map(|(name, claimed)| (name.to_string(), claiming_large(claimed, b"free", &[0; 8])))
    .chain((0..16).map(|claimed| {
        (
            format!("a 64-bit size of {claimed}, inside its own header"),
            claiming_large(claimed, b"free", &[0; 8]),
        )
    }))
    .chain((2..8).map(|claimed| {
        (
            format!("a 32-bit size of {claimed}, inside its own header"),
            claiming(claimed, b"free", &[0; 8]),
        )
    }))
    .chain([
        (
            "the largest 32-bit size".to_string(),
            claiming(u32::MAX, b"free", &[0; 8]),
        ),
        (
            "a 32-bit size one byte past the slice".to_string(),
            claiming(8 + 8 + movie.len() as u32 + 1, b"free", &[0; 8]),
        ),
    ])
    .collect();
    for (name, bad) in &cases {
        // The moov after the bad box is whole, and is not reached.
        let video = join(&[&head, bad, &movie]);
        assert_eq!(probed(&video), None, "{name}");
    }
    // A box that fits exactly is still followed.
    let exact = join(&[&head, &claiming_large(24, b"free", &[0; 8]), &movie]);
    assert_eq!(probed(&exact), size(1920, 1080));

    // The same inside moov and inside a track: a child may not run past its
    // parent, even when the bytes it claims exist in the file.
    let tail = boxed(b"mdat", &[0xcd; 256]);
    let header = tkhd(0, px(1920), px(1080));
    for (name, children) in [
        (
            "a moov child one byte past moov",
            join(&[
                &claiming(8 + 4 + track.len() as u32 + 1, b"udta", &[0; 4]),
                &track,
            ]),
        ),
        (
            "a moov child with the largest 64-bit size",
            join(&[&claiming_large(u64::MAX, b"udta", &[0; 4]), &track]),
        ),
        (
            "a moov child smaller than its header",
            join(&[&claiming(4, b"udta", &[0; 4]), &track]),
        ),
        (
            "a track one byte past moov",
            claiming(8 + header.len() as u32 + 1, b"trak", &header),
        ),
        (
            "a track header one byte past its track",
            boxed(
                b"trak",
                &claiming(8 + 84 + 1, b"tkhd", &tkhd_body(0, px(1920), px(1080))),
            ),
        ),
        (
            "a track child with a size that wraps",
            boxed(
                b"trak",
                &join(&[&claiming_large(u64::MAX - 8, b"edts", &[0; 4]), &header]),
            ),
        ),
    ] {
        let video = join(&[&head, &boxed(b"moov", &children), &tail]);
        assert_eq!(probed(&video), None, "{name}");
    }
}

#[test]
fn the_walk_stops_at_its_box_count() {
    let track = video_track(0, 1920, 1080);
    let filler = boxed(b"free", &[]);
    let fill = |count: usize| filler.repeat(count);

    // 64 top-level boxes, ftyp included.
    let at_the_cap = join(&[&isom(), &fill(62), &moov(&[&track])]);
    assert_eq!(probed(&at_the_cap), size(1920, 1080));
    let past_the_cap = join(&[&isom(), &fill(63), &moov(&[&track])]);
    assert_eq!(probed(&past_the_cap), None);

    // 256 children of moov.
    let at_the_cap = join(&[&isom(), &moov(&[&fill(255), &track])]);
    assert_eq!(probed(&at_the_cap), size(1920, 1080));
    let past_the_cap = join(&[&isom(), &moov(&[&fill(256), &track])]);
    assert_eq!(probed(&past_the_cap), None);
    // Tracks with no size count against it like any other child.
    let audio = audio_track();
    let many_tracks = join(&[&isom(), &moov(&[&audio.repeat(255), &track])]);
    assert_eq!(probed(&many_tracks), size(1920, 1080));
    let too_many_tracks = join(&[&isom(), &moov(&[&audio.repeat(256), &track])]);
    assert_eq!(probed(&too_many_tracks), None);

    // 64 children of one track; the next track starts a fresh count.
    let header = tkhd(0, px(1920), px(1080));
    let deep = boxed(b"trak", &join(&[&fill(63), &header]));
    assert_eq!(probed(&join(&[&isom(), &moov(&[&deep])])), size(1920, 1080));
    let too_deep = boxed(b"trak", &join(&[&fill(64), &header]));
    assert_eq!(probed(&join(&[&isom(), &moov(&[&too_deep])])), None);
    let next = video_track(0, 1280, 720);
    assert_eq!(
        probed(&join(&[&isom(), &moov(&[&too_deep, &next])])),
        size(1280, 720)
    );
}

#[test]
fn input_that_is_not_a_video_is_unsupported_whatever_follows() {
    let movie = moov(&[&mvhd(), &video_track(0, 1920, 1080)]);
    let mut webm = vec![0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01];
    webm.extend_from_slice(b"\x42\x82\x84webm");
    for (name, bytes) in [
        ("empty", Vec::new()),
        ("png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec()),
        ("webm", webm),
        ("text", b"not a video".to_vec()),
        ("moov with no ftyp", movie.clone()),
        (
            "a mov with no ftyp",
            join(&[&boxed(b"wide", &[]), &boxed(b"mdat", &[0xcd; 8]), &movie]),
        ),
        ("heic", join(&[&ftyp(b"heic", &[b"mif1", b"heic"]), &movie])),
        ("m4a", join(&[&ftyp(b"M4A ", &[b"M4A "]), &movie])),
        (
            "ftyp after another box",
            join(&[&boxed(b"free", &[]), &isom(), &movie]),
        ),
        ("a cut ftyp", isom()[..20].to_vec()),
    ] {
        assert_eq!(video_media_type(&bytes), None, "{name}");
        assert_eq!(probe_video(&bytes), Err(MediaError::Unsupported), "{name}");
    }
}
