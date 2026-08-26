//! rosetta-server — the L3 service runtime (docs/ARCHITECTURE.md §8). The
//! axum REST/WS surface is not shipped yet; what lives here now is [`hub`],
//! the pub/sub fan-out those services will serve subscriptions from.

pub mod hub;

pub use hub::{AbsorbReport, Hub, HubConfig, HubItem, Subscription};
