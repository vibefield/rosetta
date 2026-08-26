use crate::model::LatencySummary;

/// Nearest-rank percentile: sort ascending and select `ceil(p * n) - 1`.
/// This is deliberately simple and frozen so p99 does not move when a stats
/// dependency changes interpolation rules.
fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    let rank = sorted.len().saturating_mul(numerator).div_ceil(denominator);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

pub(crate) fn summarize(values: &[u64]) -> LatencySummary {
    assert!(!values.is_empty(), "a latency summary needs samples");
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let total: u128 = sorted.iter().map(|&value| u128::from(value)).sum();
    LatencySummary {
        samples: sorted.len() as u64,
        min_nanos: sorted[0],
        mean_nanos: u64::try_from(total / sorted.len() as u128).unwrap_or(u64::MAX),
        p50_nanos: percentile(&sorted, 50, 100),
        p95_nanos: percentile(&sorted, 95, 100),
        p99_nanos: percentile(&sorted, 99, 100),
        max_nanos: sorted[sorted.len() - 1],
    }
}

#[cfg(test)]
mod tests {
    use super::summarize;

    #[test]
    fn nearest_rank_percentiles_are_frozen() {
        let values: Vec<u64> = (1..=100).collect();
        let summary = summarize(&values);
        assert_eq!(summary.samples, 100);
        assert_eq!(summary.mean_nanos, 50);
        assert_eq!(summary.p50_nanos, 50);
        assert_eq!(summary.p95_nanos, 95);
        assert_eq!(summary.p99_nanos, 99);
        assert_eq!(summary.max_nanos, 100);
    }
}
