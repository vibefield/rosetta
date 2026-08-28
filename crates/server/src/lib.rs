//! rosetta-server — the L3 service runtime (docs/ARCHITECTURE.md §8):
//! [`hub`] (the pub/sub fan-out), [`http`] (the axum REST surface over
//! canonical queries, the search backend, and the hub's mirror), and [`ws`] (WebSocket
//! subscriptions speaking `rosetta-protocol` frames). Async lives only in
//! this layer; the ingest engine below stays synchronous.

pub mod health;
pub mod http;
pub mod hub;
pub mod ws;

pub use health::{DaemonHealth, RuntimeTick};
pub use http::{AppState, router};
pub use hub::{AbsorbReport, Hub, HubConfig, HubItem, Subscription};
