//! Optional, transient observation of cold work. Callbacks run at record-batch
//! boundaries, never per normalized op, and cannot publish canonical rows.

use crate::SessionFile;

pub trait ColdProgressObserver: std::fmt::Debug + Send + Sync {
    /// Framed payload processed in this batch, excluding audit/prefix rereads.
    /// This is work performed, not a claim that its session is browseable.
    fn source_bytes(&self, file: &SessionFile, bytes: u64);

    /// Input reading closed; cross-file reconciliation and ordinal preparation
    /// may still take substantial time before the runtime can be published.
    fn reading_finished(&self);
}
