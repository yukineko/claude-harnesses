//! Pure, deterministic SWE-bench resolution scorer.
//!
//! Grading a candidate is a two-part rule, both parts required:
//!   * every `FAIL_TO_PASS` test id must **pass** (the fix works), and
//!   * every `PASS_TO_PASS` test id must **still pass** (no regression).
//!
//! Design invariant: this module is *pure* — no network, no clock, no env, no
//! I/O. Grading is a function of the per-instance test-result map and the
//! instance's two named test-id lists, nothing else. That keeps scoring
//! hermetic and reproducible.
//!
//! Fail-closed: a test id that is absent from the result map counts as **not
//! passing**. A missing result is never charitably treated as a pass.

use std::collections::BTreeMap;

/// Decide whether a candidate **resolves** an instance.
///
/// Returns `true` iff every id in `fail_to_pass` passes AND every id in
/// `pass_to_pass` passes, according to `results` (a map of test-id ->
/// `true` for pass / `false` for fail).
///
/// A test id missing from `results` is treated as **not passing**
/// (fail-closed), so an incomplete result map can never over-credit a
/// candidate.
///
/// Pure: no I/O, no network, no clock.
pub fn is_resolved(
    results: &BTreeMap<String, bool>,
    fail_to_pass: &[String],
    pass_to_pass: &[String],
) -> bool {
    let all_pass = |ids: &[String]| -> bool {
        ids.iter()
            .all(|id| results.get(id).copied().unwrap_or(false))
    };
    all_pass(fail_to_pass) && all_pass(pass_to_pass)
}

/// Aggregate resolution rate over a slice of per-instance verdicts.
///
/// `resolution_rate = resolved_count / total`. The empty case returns `0.0`
/// (no panic, no divide-by-zero).
///
/// Pure: no I/O, no network, no clock.
pub fn aggregate_resolution_rate(verdicts: &[bool]) -> f64 {
    let total = verdicts.len();
    if total == 0 {
        return 0.0;
    }
    let resolved = verdicts.iter().filter(|&&v| v).count();
    resolved as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    fn results(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn fully_resolved() {
        let f2p = ids(&["test_fix_a", "test_fix_b"]);
        let p2p = ids(&["test_guard_x", "test_guard_y"]);
        let r = results(&[
            ("test_fix_a", true),
            ("test_fix_b", true),
            ("test_guard_x", true),
            ("test_guard_y", true),
        ]);
        assert!(is_resolved(&r, &f2p, &p2p));
    }

    #[test]
    fn fail_to_pass_regression_is_not_resolved() {
        let f2p = ids(&["test_fix_a", "test_fix_b"]);
        let p2p = ids(&["test_guard_x"]);
        let r = results(&[
            ("test_fix_a", true),
            ("test_fix_b", false), // one FAIL_TO_PASS did not flip
            ("test_guard_x", true),
        ]);
        assert!(!is_resolved(&r, &f2p, &p2p));
    }

    #[test]
    fn pass_to_pass_regression_is_not_resolved() {
        let f2p = ids(&["test_fix_a"]);
        let p2p = ids(&["test_guard_x", "test_guard_y"]);
        let r = results(&[
            ("test_fix_a", true),
            ("test_guard_x", true),
            ("test_guard_y", false), // a previously-green test regressed
        ]);
        assert!(!is_resolved(&r, &f2p, &p2p));
    }

    #[test]
    fn missing_test_id_is_fail_closed() {
        let f2p = ids(&["test_fix_a", "test_absent"]);
        let p2p = ids(&[]);
        let r = results(&[("test_fix_a", true)]); // test_absent has no result
        assert!(!is_resolved(&r, &f2p, &p2p));
    }

    #[test]
    fn empty_run_rate_is_zero_no_panic() {
        assert_eq!(aggregate_resolution_rate(&[]), 0.0);
    }

    #[test]
    fn aggregate_rate_counts_resolved_over_total() {
        let verdicts = [true, false, true, true]; // 3 of 4
        assert_eq!(aggregate_resolution_rate(&verdicts), 0.75);
        assert_eq!(aggregate_resolution_rate(&[false, false]), 0.0);
        assert_eq!(aggregate_resolution_rate(&[true, true]), 1.0);
    }
}
