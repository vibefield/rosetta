//! The implemented cold-run controls. Work phase and execution state are
//! separate: pausing never erases the metadata-ready stamp or progress.

use serde::{Deserialize, Serialize};

use crate::Ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum IngestionState {
    Running,
    Pausing,
    Paused,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum IngestionAction {
    Pause,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum IngestionPhase {
    Discovering,
    Restoring,
    Capturing,
    Finalizing,
    Publishing,
}

/// One auto-started cold run. The same resource is returned by the control
/// routes and `daemon.ingest.run`. Ordinary progress stays in that existing
/// ingest block; it is not persisted in the control receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Ingestion {
    pub id: String,
    /// Stable logical-index identity; pin it when creating a command.
    pub index_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub revision: u64,
    pub state: IngestionState,
    pub phase: Option<IngestionPhase>,
    pub created_at: Ms,
    pub updated_at: Ms,
    /// Changes whenever recovery resets the RAM progress baseline.
    pub progress_epoch: String,
    pub runtime_ids: Vec<String>,
    pub allowed_actions: Vec<IngestionAction>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct IngestionControlRequest {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct IngestionConflict {
    pub ingestion_id: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub current_revision: u64,
}
