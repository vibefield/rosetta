use std::collections::BTreeMap;

use crate::RUN_FORMAT;
use crate::model::{ComparisonReport, LaneComparison, LatencySummary, RunReport};

fn percent(before: u64, after: u64) -> f64 {
    if before == 0 {
        0.0
    } else {
        (after as f64 / before as f64 - 1.0) * 100.0
    }
}

fn ensure_compatible(before: &RunReport, after: &RunReport) -> Result<(), String> {
    if before.format != RUN_FORMAT || after.format != RUN_FORMAT {
        return Err(format!("both reports must use run format {RUN_FORMAT}"));
    }
    if before.campaign_id != after.campaign_id
        || before.ledger != after.ledger
        || before.query_suite != after.query_suite
    {
        return Err("reports do not share one frozen ledger and query suite".into());
    }
    if before.machine != after.machine || before.os != after.os || before.arch != after.arch {
        return Err("reports were not captured on the same declared machine".into());
    }
    if before.config != after.config {
        return Err("reports used different warmup or measured cycle counts".into());
    }
    if before.database.schema_version != after.database.schema_version {
        return Err("reports used different canonical schema versions".into());
    }
    if before.database.counts != after.database.counts {
        return Err("reports used projections with different logical row counts".into());
    }
    Ok(())
}

fn same_results(
    before: &crate::model::LaneReport,
    after: &crate::model::LaneReport,
) -> Result<(), String> {
    if before.cases.len() != after.cases.len() {
        return Err(format!("lane `{}` has a different case count", before.lane));
    }
    for (left, right) in before.cases.iter().zip(&after.cases) {
        if left.label != right.label
            || left.class != right.class
            || left.query_blake3 != right.query_blake3
            || left.filter_blake3 != right.filter_blake3
            || left.weight != right.weight
            || left.limit != right.limit
            || left.hits != right.hits
            || left.result_blake3 != right.result_blake3
        {
            return Err(format!(
                "ranked results/configuration differ for lane `{}`, case `{}`",
                before.lane, left.label
            ));
        }
    }
    Ok(())
}

pub fn compare_reports(before: &RunReport, after: &RunReport) -> Result<ComparisonReport, String> {
    ensure_compatible(before, after)?;
    let after_lanes: BTreeMap<&str, _> = after
        .lanes
        .iter()
        .map(|lane| (lane.lane.as_str(), lane))
        .collect();
    if before.lanes.len() != after_lanes.len() {
        return Err("reports have different lane counts".into());
    }

    let mut lanes = Vec::new();
    for left in &before.lanes {
        let right = after_lanes
            .get(left.lane.as_str())
            .ok_or_else(|| format!("after report is missing lane `{}`", left.lane))?;
        same_results(left, right)?;
        lanes.push(LaneComparison {
            lane: left.lane.clone(),
            before: left.latency.clone(),
            after: right.latency.clone(),
            p50_change_percent: percent(left.latency.p50_nanos, right.latency.p50_nanos),
            p95_change_percent: percent(left.latency.p95_nanos, right.latency.p95_nanos),
            p99_change_percent: percent(left.latency.p99_nanos, right.latency.p99_nanos),
        });
    }
    Ok(ComparisonReport {
        campaign_id: before.campaign_id.clone(),
        before_implementation: before.implementation.clone(),
        after_implementation: after.implementation.clone(),
        machine: before.machine.clone(),
        lanes,
    })
}

pub fn comparison_line(lane: &str, before: &LatencySummary, after: &LatencySummary) -> String {
    format!(
        "{lane:<24} p50 {:+7.2}%  p95 {:+7.2}%  p99 {:+7.2}%",
        percent(before.p50_nanos, after.p50_nanos),
        percent(before.p95_nanos, after.p95_nanos),
        percent(before.p99_nanos, after.p99_nanos),
    )
}
