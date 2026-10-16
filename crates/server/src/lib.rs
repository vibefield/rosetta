//! rosetta-server — the L3 service runtime (docs/ARCHITECTURE.md §8):
//! [`hub`] (the pub/sub fan-out), [`http`] (the axum REST surface over
//! canonical queries, the search backend, and the hub's mirror), [`ws`] (WebSocket
//! subscriptions speaking `rosetta-protocol` frames), and [`auth`] with
//! [`grant`] (the bearer, per-session grants and the Origin rule —
//! docs/AUTH.md). Async lives only in this layer; the ingest engine below
//! stays synchronous.
//!
//! @atlas layer edge · domain api
//! @atlas rule server-deps "server → {core, protocol, store}" {
//!   only this -> crate:rosetta-{core, protocol, store}
//!   see "docs/ARCHITECTURE.md#8-workspace-layout"
//! }
//! @atlas rule search-seam "server searches only through the backend-neutral seam" {
//!   deny this -> sym:rosetta_search::{Fts5SidecarSearch, Fts5Search}
//!   except mod:rosetta_server::http -> sym:rosetta_search::Fts5SidecarSearch "AppState::new builds the FTS5 default for tests; the daemon injects its backend via with_search"
//!   see "docs/ARCHITECTURE.md#6-storage"
//! }

pub mod auth;
pub mod board;
pub mod consent;
pub mod enrolled;
pub mod grant;
pub mod health;
pub mod http;
pub mod hub;
pub mod ws;

pub use auth::{AuthPolicy, GrantEvent, GrantObserver, Refusal};
pub use board::ProgressBoard;
pub use consent::{BoostLedger, ConsentGate, GrantError};
pub use enrolled::EnrolledRuntimes;
// `grant::GrantError` stays in its module: `consent::GrantError` (a consent
// grant) owns the short name at the root.
pub use grant::GrantClaims;
// The roll-call row travels with the gate that publishes it, so the daemon
// binary needs no protocol dependency of its own.
pub use health::{DaemonHealth, IngestCounterSource, IngestCounters, RuntimeIdentity, RuntimeTick};
pub use http::{AppState, ProjectionReader, RawCommandReader, router};
pub use hub::{AbsorbReport, Hub, HubConfig, HubItem, Subscription};
pub use rosetta_protocol::{
    BackfillState, DiscoveryRow, IngestPhase, PROTOCOL_VERSION, RuntimeProvenance,
};
