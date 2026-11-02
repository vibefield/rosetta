//! Measurement only: the `walk:` line, the `walk-plan` event and the lane
//! profile of one runtime's capture walk.

use rosetta_pipeline::{LaneProfile, Samples, WalkProfile, WalkSummary};
use serde_json::{Value, json};

use crate::bench;

/// Print the runtime's `walk:` line, and with `--json --profile` its
/// `walk-plan` event: per source in global order `[path_rank, lane,
/// tree_index, read_len, age_ms_from_newest]`, structure only — no path, id
/// or content.
pub(super) fn announce(
    plan: &WalkSummary,
    runtime_id: &str,
    kind: &str,
    fell_back: bool,
    stage_profiles: bool,
) {
    println!("{}", plan.line(runtime_id, kind, fell_back));
    if stage_profiles {
        let sources: Vec<Value> = plan
            .sources
            .iter()
            .map(|source| {
                json!([
                    source.path_rank,
                    source.lane.to_string(),
                    source.tree,
                    source.read_len,
                    source.age_ms
                ])
            })
            .collect();
        bench::emit(&json!({
            "event": "walk-plan",
            "runtime": runtime_id,
            "agent": kind,
            "mode": plan.mode,
            "digest": plan.digest_hex16(),
            "sources": sources,
        }));
    }
}

fn millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

fn distribution(samples: &mut Samples) -> Value {
    json!({
        "count": samples.len(),
        "p50Ms": millis(samples.percentile_nanos(50)),
        "p99Ms": millis(samples.percentile_nanos(99)),
        "maxMs": millis(samples.max_nanos()),
    })
}

fn lane(profile: &LaneProfile) -> Value {
    let mut held = profile.held.clone();
    let mut service = profile.service_delay.clone();
    json!({
        "lane": profile.lane.to_string(),
        "units": profile.units,
        "sources": profile.sources,
        "bytes": profile.bytes,
        "headWaitMs": millis(profile.head_wait_nanos),
        "heldMs": distribution(&mut held),
        "endMs": millis(profile.end_nanos),
        "inlineMs": millis(profile.inline_nanos),
        "serviceDelayMs": distribution(&mut service),
        "peakUnemittedHeadBytes": profile.peak_unemitted_head_bytes,
    })
}

/// The `parallel-stream-prepare` profile's `lanes` array: `null` for a path
/// walk.
pub(super) fn lanes(walk: Option<&WalkProfile>) -> Value {
    walk.map_or(Value::Null, |walk| {
        Value::Array(walk.lanes.iter().map(lane).collect())
    })
}

/// Its scalars: `(laneSwitches, bigLaneChainMs)`, `null` for a path walk.
pub(super) fn scalars(walk: Option<&WalkProfile>) -> (Value, Value) {
    walk.map_or((Value::Null, Value::Null), |walk| {
        (
            json!(walk.lane_switches),
            json!(millis(walk.big_lane_chain_nanos)),
        )
    })
}
