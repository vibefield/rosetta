//! Frozen-input search campaigns.
//!
//! Organic data is useful corroboration but may never become a fixture. This
//! crate makes the local lane auditable without retaining content: a campaign
//! begins with a permission-read-only checksummed ledger and checksummed query suite;
//! every run verifies them before and after, opens SQLite read-only, and emits
//! only timings, counts, labels, and digests. Query text, snippets, and row ids
//! never enter the report.

mod compare;
mod fingerprint;
mod model;
mod runner;
mod stats;

#[cfg(feature = "tantivy-prototype")]
mod tantivy_proto;

pub use compare::{compare_reports, comparison_line};
pub use fingerprint::{freeze_campaign, read_json, verify_campaign, write_json_new};
pub use model::{
    CampaignManifest, ComparisonReport, QueryCase, QueryFilterSpec, QuerySuite, RunReport,
};
pub use runner::run_campaign;
pub use runner::summary_line;
#[cfg(feature = "tantivy-prototype")]
pub use tantivy_proto::{TantivyRunOptions, TantivyRunReport, run_tantivy_prototype};

pub const CAMPAIGN_FORMAT: u32 = 1;
/// Version 2 binds reports to the backend-neutral product query grammar and
/// deterministic FTS tie order.
pub const RUN_FORMAT: u32 = 2;
pub const SUITE_FORMAT: u32 = 2;
