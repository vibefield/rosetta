//! Raw bytes observed at an explicitly presented document locator. The parent
//! source/generation/offset ties this evidence to its publication record.

use serde::{Deserialize, Serialize};

use super::worker::AsyncCommand;
use super::{Ledger, MAX_FRAME_BYTES, PrebuiltFrames, SourceDef};
use crate::CaptureError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentCaptureStatus {
    Captured,
    Missing,
    TooLarge,
    Unsupported,
    ChangedDuringRead,
}

/// What a media observation was asked to capture: the type and size the
/// transcript recorded for the file. Its presence marks the frame as media;
/// a frame without one is an HTML document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaClaim {
    pub media_type: String,
    pub expected_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedDocument {
    pub path: String,
    pub captured_at: i64,
    pub status: DocumentCaptureStatus,
    pub recovered_from: Option<String>,
    pub media: Option<MediaClaim>,
    pub bytes: Vec<u8>,
}

impl CapturedDocument {
    /// The one frame contract. A checksum-valid frame that `decode` rejects
    /// ends the walk of its whole segment, so `encode` refuses exactly the
    /// frames `decode` rejects and the writer can never emit one.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.path.is_empty() || self.path.len() > 16384 || self.path.contains('\0') {
            return Err("captured document path is empty, over-long or holds a NUL");
        }
        if self
            .recovered_from
            .as_ref()
            .is_some_and(|from| from.is_empty() || from.len() > 32768 || from.contains('\0'))
        {
            return Err("captured document recovery source is empty, over-long or holds a NUL");
        }
        if self.status != DocumentCaptureStatus::Captured {
            if !self.bytes.is_empty() {
                return Err("an uncaptured document carries bytes");
            }
            if self.recovered_from.is_some() {
                return Err("an uncaptured document names a recovery source");
            }
        }
        if let Some(media) = &self.media {
            if media.expected_len == 0 {
                return Err("a media claim records no size");
            }
            if media.media_type.is_empty()
                || media.media_type.len() > 255
                || media.media_type.contains('\0')
            {
                return Err("a media claim's type is empty, over-long or holds a NUL");
            }
            if self.status == DocumentCaptureStatus::Captured
                && self.bytes.len() as u64 != media.expected_len
            {
                return Err("captured media is not the size its claim recorded");
            }
        }
        Ok(())
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>, CaptureError> {
        self.validate()
            .map_err(|reason| CaptureError::InvalidConfig(reason.to_string()))?;
        // A frame with no media claim is written exactly as before the claim
        // existed, so HTML frames stay byte-identical.
        let metadata = if let Some(media) = &self.media {
            serde_json::to_vec(&(
                &self.path,
                self.captured_at,
                self.status,
                &self.recovered_from,
                media,
            ))
        } else if self.recovered_from.is_some() {
            serde_json::to_vec(&(
                &self.path,
                self.captured_at,
                self.status,
                &self.recovered_from,
            ))
        } else {
            serde_json::to_vec(&(&self.path, self.captured_at, self.status))
        }
        .map_err(|e| CaptureError::InvalidConfig(e.to_string()))?;
        let size = 4 + metadata.len() + self.bytes.len();
        if size > MAX_FRAME_BYTES - 20 {
            return Err(CaptureError::LimitExceeded(
                "captured document exceeds ledger frame".into(),
            ));
        }
        let mut encoded = Vec::with_capacity(size);
        encoded.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
        encoded.extend_from_slice(&metadata);
        encoded.extend_from_slice(&self.bytes);
        Ok(encoded)
    }

    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        let length = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
        let end = 4_usize.checked_add(length)?;
        // Old document frames remain readable without rewriting the raw ledger.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Metadata {
            Media(
                String,
                i64,
                DocumentCaptureStatus,
                Option<String>,
                MediaClaim,
            ),
            Current(String, i64, DocumentCaptureStatus, Option<String>),
            Legacy(String, i64, DocumentCaptureStatus),
        }
        let (path, captured_at, status, recovered_from, media) =
            match serde_json::from_slice(bytes.get(4..end)?).ok()? {
                Metadata::Media(path, at, status, from, media) => {
                    (path, at, status, from, Some(media))
                }
                Metadata::Current(path, at, status, from) => (path, at, status, from, None),
                Metadata::Legacy(path, at, status) => (path, at, status, None, None),
            };
        let document = Self {
            path,
            captured_at,
            status,
            recovered_from,
            media,
            bytes: bytes.get(end..)?.to_vec(),
        };
        document.validate().ok()?;
        Some(document)
    }
}

impl Ledger {
    pub fn append_document(
        &mut self,
        source: &SourceDef,
        generation: u64,
        start_offset: u64,
        document: CapturedDocument,
    ) -> Result<(), CaptureError> {
        if self.async_backend.is_some() {
            return self.send_async(AsyncCommand::Document {
                source: source.clone(),
                generation,
                start_offset,
                document,
            });
        }
        let key = self.define(source)?;
        let mut frames = PrebuiltFrames::new(key, generation);
        frames.push_document(start_offset, &document)?;
        self.write_frames(key, frames)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::ledger::{LedgerEntry, scan};

    fn source() -> SourceDef {
        SourceDef {
            ledger_only: false,
            relative_path: "media.jsonl".to_string(),
            stream_id: "transcript".to_string(),
            session_id: "s-canonical".to_string(),
            native_session_id: "s-native".to_string(),
            agent_version: Some("1.2.3".to_string()),
        }
    }

    /// A document frame body written with no contract check: what a writer
    /// without `validate` would have produced.
    fn unchecked_frame(document: &CapturedDocument) -> Vec<u8> {
        let metadata = match (&document.media, &document.recovered_from) {
            (Some(media), from) => serde_json::to_vec(&(
                &document.path,
                document.captured_at,
                document.status,
                from,
                media,
            )),
            (None, Some(from)) => serde_json::to_vec(&(
                &document.path,
                document.captured_at,
                document.status,
                Some(from),
            )),
            (None, None) => {
                serde_json::to_vec(&(&document.path, document.captured_at, document.status))
            }
        }
        .unwrap();
        let mut frame = (metadata.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&metadata);
        frame.extend_from_slice(&document.bytes);
        frame
    }

    fn media_document(status: DocumentCaptureStatus, bytes: &[u8]) -> CapturedDocument {
        CapturedDocument {
            path: "/work/out/clip.mp4".into(),
            captured_at: 1_790_000_000_123,
            status,
            recovered_from: None,
            media: Some(MediaClaim {
                media_type: "video/mp4".into(),
                expected_len: 11,
            }),
            bytes: bytes.to_vec(),
        }
    }

    const STATUSES: [DocumentCaptureStatus; 5] = [
        DocumentCaptureStatus::Captured,
        DocumentCaptureStatus::Missing,
        DocumentCaptureStatus::TooLarge,
        DocumentCaptureStatus::Unsupported,
        DocumentCaptureStatus::ChangedDuringRead,
    ];

    #[test]
    fn media_frames_round_trip_for_every_status() {
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let source = source();
        let mut written = Vec::new();
        for (index, status) in STATUSES.into_iter().enumerate() {
            let bytes: &[u8] = if status == DocumentCaptureStatus::Captured {
                b"eleven byte"
            } else {
                b""
            };
            let document = media_document(status, bytes);
            assert_eq!(
                CapturedDocument::decode(&document.encode().unwrap()),
                Some(document.clone()),
                "{status:?}"
            );
            ledger
                .append_document(&source, 1, index as u64, document.clone())
                .unwrap();
            written.push((index as u64, document));
        }
        // A carried capture keeps its claim and names where it came from.
        let mut carried = media_document(DocumentCaptureStatus::Captured, b"eleven byte");
        carried.recovered_from = Some("ledger:1:0".into());
        assert_eq!(
            CapturedDocument::decode(&carried.encode().unwrap()),
            Some(carried.clone())
        );
        ledger
            .append_document(&source, 2, 0, carried.clone())
            .unwrap();
        ledger.flush().unwrap();
        assert_eq!(ledger.dir(), dir.path());

        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 0);
        let documents: Vec<_> = scanned
            .entries
            .iter()
            .filter_map(|entry| match entry {
                LedgerEntry::Document {
                    generation,
                    start_offset,
                    document,
                    ..
                } => Some((*generation, *start_offset, document.clone())),
                _ => None,
            })
            .collect();
        let mut expected: Vec<_> = written
            .into_iter()
            .map(|(offset, document)| (1, offset, document))
            .collect();
        expected.push((2, 0, carried));
        assert_eq!(documents, expected);
    }

    #[test]
    fn frames_without_a_media_claim_keep_their_exact_bytes() {
        // The three-field form, as written before recovery provenance existed.
        let plain = CapturedDocument {
            path: "/tmp/review.html".into(),
            captured_at: 99,
            status: DocumentCaptureStatus::Captured,
            recovered_from: None,
            media: None,
            bytes: b"<h1>Review</h1>".to_vec(),
        };
        let mut expected = vec![0, 0, 0, 34];
        expected.extend_from_slice(br#"["/tmp/review.html",99,"captured"]"#);
        expected.extend_from_slice(b"<h1>Review</h1>");
        assert_eq!(plain.encode().unwrap(), expected);
        assert_eq!(CapturedDocument::decode(&expected), Some(plain.clone()));

        // The four-field form.
        let recovered = CapturedDocument {
            recovered_from: Some("/archive/review.html".into()),
            ..plain.clone()
        };
        let mut expected = vec![0, 0, 0, 57];
        expected.extend_from_slice(br#"["/tmp/review.html",99,"captured","/archive/review.html"]"#);
        expected.extend_from_slice(b"<h1>Review</h1>");
        assert_eq!(recovered.encode().unwrap(), expected);
        assert_eq!(CapturedDocument::decode(&expected), Some(recovered));

        // An uncaptured observation has no body at all.
        let missing = CapturedDocument {
            status: DocumentCaptureStatus::Missing,
            bytes: Vec::new(),
            ..plain
        };
        let mut expected = vec![0, 0, 0, 33];
        expected.extend_from_slice(br#"["/tmp/review.html",99,"missing"]"#);
        assert_eq!(missing.encode().unwrap(), expected);

        // The media form is the only one with a fifth element.
        let media = media_document(DocumentCaptureStatus::Missing, b"");
        let mut expected = vec![0, 0, 0, 96];
        expected.extend_from_slice(
            br#"["/work/out/clip.mp4",1790000000123,"missing",null,{"media_type":"video/mp4","expected_len":11}]"#,
        );
        assert_eq!(media.encode().unwrap(), expected);
    }

    /// Every way a document can break the frame contract, by name.
    fn invalid_documents() -> Vec<(&'static str, CapturedDocument)> {
        type Break = fn(&mut CapturedDocument);
        let html = CapturedDocument {
            path: "/tmp/review.html".into(),
            captured_at: 99,
            status: DocumentCaptureStatus::Captured,
            recovered_from: Some("/archive/review.html".into()),
            media: None,
            bytes: b"<h1>Review</h1>".to_vec(),
        };
        let media = media_document(DocumentCaptureStatus::Captured, b"eleven byte");
        let html_breaks: [(&str, Break); 9] = [
            ("empty path", |d| d.path.clear()),
            ("over-long path", |d| d.path = "p".repeat(16385)),
            ("nul in path", |d| d.path = "/tmp/\0.html".into()),
            ("empty recovery source", |d| {
                d.recovered_from = Some(String::new())
            }),
            ("over-long recovery source", |d| {
                d.recovered_from = Some("r".repeat(32769))
            }),
            ("nul in recovery source", |d| {
                d.recovered_from = Some("/archive/\0".into())
            }),
            ("uncaptured with bytes", |d| {
                d.recovered_from = None;
                d.status = DocumentCaptureStatus::Unsupported;
            }),
            ("uncaptured with a recovery source", |d| {
                d.bytes.clear();
                d.status = DocumentCaptureStatus::Missing;
            }),
            ("too large with bytes", |d| {
                d.recovered_from = None;
                d.status = DocumentCaptureStatus::TooLarge;
            }),
        ];
        let media_breaks: [(&str, Break); 9] = [
            ("media with no recorded size", |d| {
                d.media.as_mut().unwrap().expected_len = 0
            }),
            ("media with an empty type", |d| {
                d.media.as_mut().unwrap().media_type.clear()
            }),
            ("media with an over-long type", |d| {
                d.media.as_mut().unwrap().media_type = "t".repeat(256)
            }),
            ("media with a nul in its type", |d| {
                d.media.as_mut().unwrap().media_type = "video/\0".into()
            }),
            ("captured media shorter than its claim", |d| {
                d.bytes.truncate(10)
            }),
            ("captured media longer than its claim", |d| {
                d.bytes.push(b'!')
            }),
            ("captured media with no bytes", |d| d.bytes.clear()),
            ("uncaptured media with bytes", |d| {
                d.status = DocumentCaptureStatus::ChangedDuringRead
            }),
            ("uncaptured media with a recovery source", |d| {
                d.bytes.clear();
                d.status = DocumentCaptureStatus::Missing;
                d.recovered_from = Some("ledger:1:0".into());
            }),
        ];
        let mut broken = Vec::new();
        for (base, breaks) in [(&html, &html_breaks[..]), (&media, &media_breaks[..])] {
            for (name, break_it) in breaks {
                let mut document = base.clone();
                break_it(&mut document);
                broken.push((*name, document));
            }
        }
        broken
    }

    #[test]
    fn encode_refuses_exactly_the_frames_decode_rejects() {
        for (name, document) in invalid_documents() {
            assert!(document.validate().is_err(), "{name} must not validate");
            assert!(
                matches!(document.encode(), Err(CaptureError::InvalidConfig(_))),
                "{name} must not encode"
            );
            assert_eq!(
                CapturedDocument::decode(&unchecked_frame(&document)),
                None,
                "{name} must not decode"
            );
        }

        // The writer refuses it too, and writes no frame the walk would stop at.
        let dir = TempDir::new().unwrap();
        let mut ledger = Ledger::open(dir.path()).unwrap();
        let source = source();
        ledger
            .append_raw(&source, 1, [(0, b"before".as_slice())])
            .unwrap();
        let (_, refused) = invalid_documents().pop().unwrap();
        assert!(matches!(
            ledger.append_document(&source, 1, 0, refused),
            Err(CaptureError::InvalidConfig(_))
        ));
        ledger
            .append_raw(&source, 1, [(7, b"after".as_slice())])
            .unwrap();
        ledger.flush().unwrap();
        let scanned = scan(dir.path()).unwrap();
        assert_eq!(scanned.torn_segments, 0);
        assert_eq!(scanned.entries.len(), 2);
    }

    #[test]
    fn every_frame_that_encodes_decodes_to_the_same_document() {
        let longest_path = format!("/{}", "p".repeat(16383));
        let longest_from = "r".repeat(32768);
        let longest_type = "t".repeat(255);
        let mut frames = Vec::new();
        for status in STATUSES {
            let captured = status == DocumentCaptureStatus::Captured;
            for media in [
                None,
                Some(MediaClaim {
                    media_type: "video/quicktime".into(),
                    expected_len: 3,
                }),
                Some(MediaClaim {
                    media_type: longest_type.clone(),
                    expected_len: 3,
                }),
                Some(MediaClaim {
                    media_type: "image/png".into(),
                    expected_len: u64::MAX,
                }),
            ] {
                for recovered_from in [
                    None,
                    Some("ledger:4:128".to_string()),
                    Some(longest_from.clone()),
                ] {
                    for path in ["/a", "/tmp/\u{1F3AC} clip \"quoted\".mov", &longest_path] {
                        for bytes in [&b""[..], b"abc", b"\xff\xfe\0\0"] {
                            frames.push(CapturedDocument {
                                path: path.to_string(),
                                captured_at: if captured { i64::MIN } else { i64::MAX },
                                status,
                                recovered_from: recovered_from.clone(),
                                media: media.clone(),
                                bytes: bytes.to_vec(),
                            });
                        }
                    }
                }
            }
        }
        let (mut encoded, mut refused) = (0, 0);
        for document in frames {
            match document.encode() {
                Ok(frame) => {
                    encoded += 1;
                    assert_eq!(document.validate(), Ok(()));
                    assert_eq!(
                        CapturedDocument::decode(&frame),
                        Some(document.clone()),
                        "{document:?}"
                    );
                }
                Err(CaptureError::InvalidConfig(_)) => {
                    refused += 1;
                    assert!(document.validate().is_err());
                    assert_eq!(CapturedDocument::decode(&unchecked_frame(&document)), None);
                }
                Err(error) => panic!("unexpected {error}"),
            }
        }
        // The table exercises both halves of the contract, not one.
        assert!(encoded > 50 && refused > 50, "{encoded} / {refused}");
    }

    #[test]
    fn a_captured_video_at_the_size_limit_fits_one_frame() {
        let len = 64 * 1024 * 1024;
        let document = CapturedDocument {
            path: "/work/out/long.mov".into(),
            captured_at: 1,
            status: DocumentCaptureStatus::Captured,
            recovered_from: Some("ledger:1:0".into()),
            media: Some(MediaClaim {
                media_type: "video/quicktime".into(),
                expected_len: len as u64,
            }),
            bytes: vec![0x5a; len],
        };
        let frame = document.encode().unwrap();
        assert!(frame.len() > len && frame.len() < MAX_FRAME_BYTES - 20);
        let decoded = CapturedDocument::decode(&frame).unwrap();
        assert!(decoded == document);
    }
}
