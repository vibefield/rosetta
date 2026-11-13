//! Pure native metadata for resolving shared transcript prefixes.

#[derive(Debug, Clone)]
pub struct TranscriptReference {
    pub source_id: String,
    pub end_offset: u64,
    pub end_ordinal: u64,
}

#[derive(Debug, Clone)]
pub struct TranscriptHeader {
    pub source_id: String,
    pub session_id: String,
    pub base: Option<TranscriptReference>,
    pub start_ordinal: u64,
    pub paginated: bool,
}

#[derive(Debug, Clone)]
pub struct TranscriptSelection {
    /// Native path, absolute or relative to the runtime root.
    pub path: String,
}
