//! Synchronous control boundary. The daemon owns durability and its writer;
//! HTTP neither starts a competing worker nor touches ingestion artifacts.

pub use rosetta_protocol::{ApiError, ErrorCode, IngestPhase};
pub use rosetta_protocol::{
    Ingestion, IngestionAction, IngestionConflict, IngestionControlRequest, IngestionPhase,
    IngestionState,
};

#[derive(Debug)]
pub struct IngestionReply {
    pub run: Ingestion,
    pub accepted: bool,
}

pub trait IngestionControl: Send + Sync {
    fn snapshot(&self) -> Ingestion;
    fn observe_phase(&self, phase: IngestPhase);
    fn command(
        &self,
        id: &str,
        index_id: &str,
        key: &str,
        action: IngestionAction,
        request: IngestionControlRequest,
    ) -> Result<IngestionReply, ApiError>;
}

/// A control failure uses the ordinary error envelope, with optional
/// revision details so callers can deliberately reconcile stale intent.
pub fn control_error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
        details: None,
    }
}
