//! Recorded video, classified from its container header alone. No codec, no
//! decode, no filesystem and no allocation: every read is a checked slice of
//! the caller's bytes.
//!
//! Acceptance is the `ftyp` box an ISO base media file starts with. A major
//! brand of `qt  ` is QuickTime; otherwise a major or compatible brand in
//! [`MP4_BRANDS`] is MP4. Anything else is not a video here: HEIC and AVIF
//! (image brands), WebM (no `ftyp`), a `.mov` written without one. The brand
//! list cannot tell an MP4 that carries only audio from one with a picture:
//! an `M4A ` file that lists `mp42` among its compatible brands reads as MP4.
//! Callers accept a file only when this type equals the one it was declared
//! with, which is what keeps audio declared as audio out.
//!
//! The display size is a layout hint, never part of acceptance. It comes
//! from the first track header (`moov` > `trak` > `tkhd`) that records a
//! width and a height above zero, found by a bounded walk of box headers,
//! and is read through that header's matrix: unrotated or turned 180 degrees
//! it is width by height, turned 90 or 270 degrees it is height by width,
//! and under any other matrix (a scale, a mirror, a skew) there is no size,
//! because the header alone does not prove what a player would show. A file
//! whose `moov` is cut, missing or malformed still classifies, with no size.
//! Sample descriptions and edit lists are not read.

use rosetta_core::timeline::Dimensions;

use crate::MediaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoProbe {
    pub media_type: &'static str,
    /// The display size, when the header proves one.
    pub dimensions: Option<Dimensions>,
}

/// Brands that mark an ISO base media file as MP4 video.
pub const MP4_BRANDS: [[u8; 4]; 10] = [
    *b"isom", *b"iso2", *b"iso3", *b"iso4", *b"iso5", *b"iso6", *b"mp41", *b"mp42", *b"avc1",
    *b"M4V ",
];

/// Smallest `ftyp` box: size, type, major brand and minor version.
const FTYP_MIN_BYTES: usize = 16;
/// Largest `ftyp` box read; real ones list a handful of brands.
const FTYP_MAX_BYTES: usize = 4096;

/// Top-level boxes read while looking for `moov`.
const TOP_LEVEL_BOXES: usize = 64;
/// Children of `moov` read while looking for tracks.
const MOVIE_BOXES: usize = 256;
/// Children of one `trak` read while looking for its `tkhd`.
const TRACK_BOXES: usize = 64;
/// A version 0 `tkhd` body: version and flags, 20 bytes of times and ids,
/// then 60 bytes that end with width and height.
const TKHD_V0_BYTES: u64 = 84;
/// A version 1 `tkhd` body: the same, with 64-bit times and duration.
const TKHD_V1_BYTES: u64 = 96;
/// One in the matrix's 16.16 fixed point, and minus one.
const ONE: u32 = 0x0001_0000;
const MINUS_ONE: u32 = 0xffff_0000;

/// The container type the leading `ftyp` box declares, or `None` when the
/// bytes are not a video this crate accepts.
pub fn video_media_type(bytes: &[u8]) -> Option<&'static str> {
    let size = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    if bytes.get(4..8)? != b"ftyp" {
        return None;
    }
    if !(FTYP_MIN_BYTES..=FTYP_MAX_BYTES).contains(&size)
        || !size.is_multiple_of(4)
        || size > bytes.len()
    {
        return None;
    }
    let major = bytes.get(8..12)?;
    if major == b"qt  " {
        return Some("video/quicktime");
    }
    let is_mp4 = |brand: &[u8]| MP4_BRANDS.iter().any(|known| known == brand);
    if is_mp4(major) || bytes.get(16..size)?.chunks_exact(4).any(is_mp4) {
        return Some("video/mp4");
    }
    None
}

/// Classify recorded video bytes. Accepts exactly what [`video_media_type`]
/// accepts. The size is present only when a track header proves one, and
/// neither its width nor its height is ever zero.
pub fn probe_video(bytes: &[u8]) -> Result<VideoProbe, MediaError> {
    let media_type = video_media_type(bytes).ok_or(MediaError::Unsupported)?;
    Ok(VideoProbe {
        media_type,
        dimensions: track_dimensions(bytes),
    })
}

/// The display size of the first track of the first `moov` whose header
/// records a width and a height above zero. An audio track records zero for
/// both and is passed over. The first sized track decides: when its matrix
/// proves no display size, a later track is not read in its place.
fn track_dimensions(bytes: &[u8]) -> Option<Dimensions> {
    let file = Span {
        start: 0,
        end: bytes.len() as u64,
    };
    let movie = Boxes::new(bytes, file, TOP_LEVEL_BOXES).find(|found| &found.kind == b"moov")?;
    Boxes::new(bytes, movie.body, MOVIE_BOXES)
        .filter(|found| &found.kind == b"trak")
        .find_map(|track| {
            let header =
                Boxes::new(bytes, track.body, TRACK_BOXES).find(|found| &found.kind == b"tkhd")?;
            header_dimensions(bytes, header.body)
        })
        .flatten()
}

/// Width and height are the final eight bytes of a `tkhd` body, each 16.16
/// fixed point, and the 36 bytes before them are the track's matrix; the two
/// versions differ only in the time fields before that. A body that is not
/// exactly its version's layout is not read, and neither is a value whose
/// whole part is zero: both are `None`, a track with no size.
///
/// A sized track answers `Some`, holding the size as displayed or `None`
/// when its matrix is not one whose effect on the size is known.
fn header_dimensions(bytes: &[u8], body: Span) -> Option<Option<Dimensions>> {
    let layout = match *slice(bytes, body.start, 1)?.first()? {
        0 => TKHD_V0_BYTES,
        1 => TKHD_V1_BYTES,
        _ => return None,
    };
    if body.end.checked_sub(body.start)? != layout {
        return None;
    }
    let width = be_u32(bytes, body.end.checked_sub(8)?)? >> 16;
    let height = be_u32(bytes, body.end.checked_sub(4)?)? >> 16;
    if width == 0 || height == 0 {
        return None;
    }
    // The matrix is nine cells, row by row: `a b u`, `c d v`, `x y w`. Only
    // the four that turn and scale the picture matter to its size.
    let cell = |index: u64| be_u32(bytes, body.end.checked_sub(44)?.checked_add(4 * index)?);
    let (a, b, c, d) = (cell(0)?, cell(1)?, cell(3)?, cell(4)?);
    Some(match (a, b, c, d) {
        // As stored, or turned 180 degrees.
        (ONE, 0, 0, ONE) | (MINUS_ONE, 0, 0, MINUS_ONE) => Some(Dimensions { width, height }),
        // Turned 90 or 270 degrees: the stored rows become columns.
        (0, b, c, 0) if b != 0 && c != 0 => Some(Dimensions {
            width: height,
            height: width,
        }),
        _ => None,
    })
}

/// Byte offsets into the probed slice; `start <= end`.
#[derive(Clone, Copy)]
struct Span {
    start: u64,
    end: u64,
}

struct BoxHeader {
    kind: [u8; 4],
    /// The box's bytes after its size and type, wholly inside its parent.
    body: Span,
}

/// The boxes laid end to end in one span, at most `left` of them. The walk
/// ends for good at the first box that is malformed or runs past the span.
struct Boxes<'a> {
    bytes: &'a [u8],
    at: u64,
    end: u64,
    left: usize,
}

impl<'a> Boxes<'a> {
    fn new(bytes: &'a [u8], span: Span, limit: usize) -> Self {
        Self {
            bytes,
            at: span.start,
            end: span.end,
            left: limit,
        }
    }

    fn read(&mut self) -> Option<BoxHeader> {
        self.left = self.left.checked_sub(1)?;
        if self.end.checked_sub(self.at)? < 8 {
            return None;
        }
        let kind: [u8; 4] = slice(self.bytes, self.at.checked_add(4)?, 4)?
            .try_into()
            .ok()?;
        // A size of 1 names a 64-bit size after the type; 0 runs to the end
        // of the enclosing span.
        let (header, size) = match be_u32(self.bytes, self.at)? {
            0 => (8, self.end.checked_sub(self.at)?),
            1 => (16, be_u64(self.bytes, self.at.checked_add(8)?)?),
            size => (8, u64::from(size)),
        };
        let start = self.at.checked_add(header)?;
        let end = self.at.checked_add(size)?;
        if start > end || end > self.end {
            return None;
        }
        self.at = end;
        Some(BoxHeader {
            kind,
            body: Span { start, end },
        })
    }
}

impl Iterator for Boxes<'_> {
    type Item = BoxHeader;

    fn next(&mut self) -> Option<BoxHeader> {
        let found = self.read();
        if found.is_none() {
            self.left = 0;
        }
        found
    }
}

fn slice(bytes: &[u8], at: u64, len: usize) -> Option<&[u8]> {
    let start = usize::try_from(at).ok()?;
    bytes.get(start..start.checked_add(len)?)
}

fn be_u32(bytes: &[u8], at: u64) -> Option<u32> {
    Some(u32::from_be_bytes(slice(bytes, at, 4)?.try_into().ok()?))
}

fn be_u64(bytes: &[u8], at: u64) -> Option<u64> {
    Some(u64::from_be_bytes(slice(bytes, at, 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `ftyp` box with these brands, followed by `rest`.
    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]], rest: &[u8]) -> Vec<u8> {
        let size = 16 + 4 * compatible.len();
        let mut bytes = (size as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(major);
        bytes.extend_from_slice(&[0, 0, 2, 0]);
        for brand in compatible {
            bytes.extend_from_slice(*brand);
        }
        bytes.extend_from_slice(rest);
        bytes
    }

    fn cases() -> Vec<(&'static str, Vec<u8>, Option<&'static str>)> {
        let moov = b"\0\0\0\x08moov";
        let mut not_a_multiple = ftyp(b"isom", &[b"iso2"], moov);
        not_a_multiple[..4].copy_from_slice(&18_u32.to_be_bytes());
        let mut past_the_slice = ftyp(b"isom", &[b"iso2"], b"");
        past_the_slice[..4].copy_from_slice(&24_u32.to_be_bytes());
        let mut too_small = ftyp(b"isom", &[], moov);
        too_small[..4].copy_from_slice(&12_u32.to_be_bytes());
        let mut too_large = ftyp(b"isom", &[], &[0; 8192]);
        too_large[..4].copy_from_slice(&4100_u32.to_be_bytes());
        let mut largest = ftyp(b"heic", &[], &[0; 4096]);
        largest[..4].copy_from_slice(&4096_u32.to_be_bytes());
        largest[4092..4096].copy_from_slice(b"mp42");
        let mut extended_size = ftyp(b"isom", &[b"iso2"], moov);
        extended_size[..4].copy_from_slice(&1_u32.to_be_bytes());
        let mut not_first = b"\0\0\0\x08free".to_vec();
        not_first.extend_from_slice(&ftyp(b"isom", &[b"iso2"], moov));
        let mut webm = vec![0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01];
        webm.extend_from_slice(b"\x42\x82\x84webm");
        vec![
            (
                "isom, as ffmpeg writes it",
                ftyp(b"isom", &[b"isom", b"iso2", b"avc1", b"mp41"], moov),
                Some("video/mp4"),
            ),
            (
                "mp42 major",
                ftyp(b"mp42", &[b"mp42", b"isom"], moov),
                Some("video/mp4"),
            ),
            (
                "mp42 major with no compatible brands",
                ftyp(b"mp42", &[], b""),
                Some("video/mp4"),
            ),
            (
                "M4V major",
                ftyp(b"M4V ", &[b"M4V ", b"M4A ", b"mp42", b"isom"], moov),
                Some("video/mp4"),
            ),
            (
                "compatible-only match",
                ftyp(b"3gp4", &[b"3gp4", b"isom"], moov),
                Some("video/mp4"),
            ),
            (
                "compatible match in the last brand of the largest box",
                largest,
                Some("video/mp4"),
            ),
            (
                "quicktime",
                ftyp(b"qt  ", &[b"qt  "], b"\0\0\0\x08wide"),
                Some("video/quicktime"),
            ),
            (
                "quicktime wins over an MP4 compatible brand",
                ftyp(b"qt  ", &[b"isom"], moov),
                Some("video/quicktime"),
            ),
            ("heic", ftyp(b"heic", &[b"mif1", b"heic"], moov), None),
            (
                "avif",
                ftyp(b"avif", &[b"avif", b"mif1", b"miaf", b"MA1B"], moov),
                None,
            ),
            // An M4A whose brands name no MP4 brand. One that lists `mp42`
            // or `isom` is indistinguishable by brand; see the module doc.
            ("m4a", ftyp(b"M4A ", &[b"M4A "], moov), None),
            ("webm", webm, None),
            (
                "a mov with no ftyp",
                b"\0\0\0\x08wide\0\0\0\x08mdat".to_vec(),
                None,
            ),
            (
                "a quicktime brand compatible only",
                ftyp(b"mqt ", &[b"qt  "], moov),
                None,
            ),
            ("empty", Vec::new(), None),
            ("three bytes", b"\0\0\0".to_vec(), None),
            ("a size and nothing else", b"\0\0\0\x10".to_vec(), None),
            (
                "a header cut before the major brand",
                b"\0\0\0\x10ftyp".to_vec(),
                None,
            ),
            (
                "a header cut inside the minor version",
                b"\0\0\0\x10ftypisom\0\0".to_vec(),
                None,
            ),
            ("size not a multiple of 4", not_a_multiple, None),
            ("size past the slice", past_the_slice, None),
            ("size below the smallest box", too_small, None),
            ("size above the largest box read", too_large, None),
            ("a 64-bit box size", extended_size, None),
            ("ftyp is not the first box", not_first, None),
            ("png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(), None),
        ]
    }

    #[test]
    fn the_container_type_is_read_from_the_leading_ftyp_box() {
        for (name, bytes, expected) in cases() {
            assert_eq!(video_media_type(&bytes), expected, "{name}");
        }
    }

    #[test]
    fn the_probe_accepts_exactly_what_the_type_check_accepts() {
        for (name, bytes, expected) in cases() {
            match expected {
                Some(media_type) => assert_eq!(
                    probe_video(&bytes),
                    Ok(VideoProbe {
                        media_type,
                        dimensions: None
                    }),
                    "{name}"
                ),
                None => assert_eq!(probe_video(&bytes), Err(MediaError::Unsupported), "{name}"),
            }
        }
    }

    #[test]
    fn every_prefix_of_a_video_is_classified_without_a_panic() {
        let video = ftyp(
            b"isom",
            &[b"isom", b"iso2", b"avc1", b"mp41"],
            b"\0\0\0\x08moov",
        );
        for end in 0..=video.len() {
            let expected = (end >= 32).then_some("video/mp4");
            assert_eq!(video_media_type(&video[..end]), expected, "{end} bytes");
        }
        // Every declared size over a short buffer is safe too.
        let mut bytes = video;
        for size in 0..=64_u32 {
            bytes[..4].copy_from_slice(&size.to_be_bytes());
            let _ = video_media_type(&bytes);
            let _ = probe_video(&bytes);
        }
        bytes[..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(video_media_type(&bytes), None);
    }
}
