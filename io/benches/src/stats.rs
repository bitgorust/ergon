//! Percentile summaries used by the analysis binary.

/// A summary of a sample, values in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    /// Sample size.
    pub n: usize,
    /// Mean.
    pub mean: f64,
    /// Median.
    pub p50: i64,
    /// 90th percentile.
    pub p90: i64,
    /// 99th percentile.
    pub p99: i64,
    /// 99.9th percentile.
    pub p999: i64,
    /// 99.99th percentile (needs about a million samples to mean much).
    pub p9999: i64,
    /// Minimum.
    pub min: i64,
    /// Maximum.
    pub max: i64,
}

/// Nearest-rank percentile of a sorted slice (`q` in 0..=1).
#[must_use]
pub fn percentile(sorted: &[i64], q: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Summarises `v` (sorted in place). `None` when empty.
#[must_use]
pub fn summarize(v: &mut [i64]) -> Option<Summary> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let sum: f64 = v.iter().map(|&x| x as f64).sum();
    Some(Summary {
        n: v.len(),
        mean: sum / v.len() as f64,
        p50: percentile(v, 0.50),
        p90: percentile(v, 0.90),
        p99: percentile(v, 0.99),
        p999: percentile(v, 0.999),
        p9999: percentile(v, 0.9999),
        min: v[0],
        max: v[v.len() - 1],
    })
}

/// Formats nanoseconds as microseconds with two decimals.
#[must_use]
pub fn us(ns: i64) -> String {
    format!("{:.2}", ns as f64 / 1000.0)
}

/// A markdown table row: `label | n | mean | p50 | p90 | p99 | p99.9 | p99.99 | max` (µs).
#[must_use]
pub fn row(label: &str, s: &Summary) -> String {
    format!(
        "| {label} | {} | {:.2} | {} | {} | {} | {} | {} | {} |",
        s.n,
        s.mean / 1000.0,
        us(s.p50),
        us(s.p90),
        us(s.p99),
        us(s.p999),
        us(s.p9999),
        us(s.max)
    )
}

/// Header matching [`row`].
pub const HEADER: &str = "| | n | mean µs | p50 µs | p90 µs | p99 µs | p99.9 µs | p99.99 µs | max µs |\n|---|---|---|---|---|---|---|---|---|";
