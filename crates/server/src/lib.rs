//! rosetta-server — the L3 service runtime (docs/ARCHITECTURE.md §8):
//! [`hub`] (the pub/sub fan-out), [`http`] (the axum REST surface over
//! canonical queries, the search backend, and the hub's mirror), and [`ws`] (WebSocket
//! subscriptions speaking `rosetta-protocol` frames). Async lives only in
//! this layer; the ingest engine below stays synchronous.

pub mod board;
pub mod consent;
pub mod health;
pub mod http;
pub mod hub;
pub mod ws;

pub use board::ProgressBoard;
pub use consent::{BoostLedger, ConsentGate, GrantError};
// The roll-call row travels with the gate that publishes it, so the daemon
// binary needs no protocol dependency of its own.
pub use health::{DaemonHealth, IngestCounterSource, IngestCounters, RuntimeTick};
pub use http::{AppState, ProjectionReader, router};
pub use hub::{AbsorbReport, Hub, HubConfig, HubItem, Subscription};
pub use rosetta_protocol::DiscoveryRow;
