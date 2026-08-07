//! Shared recency weighting for search ranking.
//!
//! Relevance-ranked search over a long-lived corpus surfaces the strongest
//! textual match regardless of age, which buries recent work behind years of
//! archives. Multiplying a relevance score by [`decay`] biases ties toward
//! fresh documents while leaving strong old matches reachable.

use std::time::{SystemTime, UNIX_EPOCH};

/// Age at which the decaying portion of the weight halves.
const HALF_LIFE_DAYS: f64 = 30.0;

/// Weight retained by an arbitrarily old document.
///
/// Recency is a tiebreaker, not a veto: without a floor an old-but-exact match
/// loses to a recent-but-weak one, which is the failure mode in the opposite
/// direction.
const FLOOR: f64 = 0.35;

const NANOS_PER_DAY: f64 = 86_400_000_000_000.0;

/// Nanoseconds since the Unix epoch, matching how `mtime_ns` columns are
/// stored across the derived caches.
#[must_use]
pub fn now_unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// Multiplier in `[FLOOR, 1.0]` for a document last modified at `mtime_ns`.
///
/// Returns `1.0` for missing or future timestamps so an unknown mtime never
/// penalizes a result.
#[must_use]
pub fn decay(mtime_ns: i64, now_ns: i64) -> f64 {
    if mtime_ns <= 0 || now_ns <= mtime_ns {
        return 1.0;
    }
    let age_days = (now_ns - mtime_ns) as f64 / NANOS_PER_DAY;
    FLOOR + (1.0 - FLOOR) * 0.5_f64.powf(age_days / HALF_LIFE_DAYS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000_000_000;

    #[test]
    fn fresh_documents_keep_full_weight() {
        let now = 1_000 * DAY;
        assert!((decay(now, now) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn weight_decreases_monotonically_with_age() {
        let now = 1_000 * DAY;
        let weights: Vec<f64> = [0, 7, 30, 90, 365]
            .iter()
            .map(|days| decay(now - days * DAY, now))
            .collect();
        for pair in weights.windows(2) {
            assert!(
                pair[0] > pair[1],
                "expected decreasing weights: {weights:?}"
            );
        }
    }

    #[test]
    fn half_life_lands_midway_between_floor_and_one() {
        let now = 1_000 * DAY;
        let expected = FLOOR + (1.0 - FLOOR) * 0.5;
        assert!((decay(now - 30 * DAY, now) - expected).abs() < 1e-9);
    }

    #[test]
    fn ancient_documents_retain_the_floor() {
        let now = 100_000 * DAY;
        let weight = decay(DAY, now);
        assert!(weight >= FLOOR, "{weight} fell below the floor");
        assert!((weight - FLOOR).abs() < 1e-6);
    }

    #[test]
    fn unknown_and_future_timestamps_are_unpenalized() {
        let now = 1_000 * DAY;
        assert!((decay(0, now) - 1.0).abs() < 1e-9);
        assert!((decay(now + DAY, now) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn recency_cannot_outweigh_a_large_relevance_gap() {
        let now = 1_000 * DAY;
        let strong_old = 3.0 * decay(now - 3_650 * DAY, now);
        let weak_fresh = 1.0 * decay(now, now);
        assert!(strong_old > weak_fresh);
    }
}
