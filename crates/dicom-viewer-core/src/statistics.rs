/// Returns the nearest-rank percentile after sorting samples by the total `f64` order.
///
/// The percentile is clamped to `[0, 1]`. An empty sample returns `0.0`.
#[must_use]
pub fn nearest_rank_percentile(samples: &[f64], percentile: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (percentile.clamp(0.0, 1.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}
