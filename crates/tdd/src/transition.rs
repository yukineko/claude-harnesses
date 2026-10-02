//! Test-transition classification: given a task's RED (pre) and GREEN (post)
//! proof results, decide which of the four RED→GREEN transitions occurred and
//! whether it is a *valid* test-first (Fail→Pass) oracle.
//!
//! The only transition that proves test-first worked is `FailToPass`: the test
//! failed before the implementation and passes after it. Every other transition
//! (a test that never failed, one that still fails, or one that regressed) is
//! not a valid oracle.

use harness_core::verdict::Determination;
use serde_json::{json, Value};

/// Which RED→GREEN transition a task's proofs represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Failed at RED, passes at GREEN — the only valid test-first oracle.
    FailToPass,
    /// Failed at RED and still fails at GREEN.
    FailToFail,
    /// Already passed at RED and still passes (never a real RED).
    PassToPass,
    /// Passed at RED but fails at GREEN (a regression).
    PassToFail,
}

impl Transition {
    /// True ONLY for `FailToPass` — the sole transition that proves test-first.
    pub fn is_valid_oracle(&self) -> bool {
        matches!(self, Transition::FailToPass)
    }

    /// snake_case string used in JSON output.
    pub fn as_snake(&self) -> &'static str {
        match self {
            Transition::FailToPass => "fail_to_pass",
            Transition::FailToFail => "fail_to_fail",
            Transition::PassToPass => "pass_to_pass",
            Transition::PassToFail => "pass_to_fail",
        }
    }
}

/// Classify a transition from the RED (`pre_passed`) and GREEN (`post_passed`)
/// pass/fail results.
pub fn classify(pre_passed: bool, post_passed: bool) -> Transition {
    match (pre_passed, post_passed) {
        (false, true) => Transition::FailToPass,
        (false, false) => Transition::FailToFail,
        (true, true) => Transition::PassToPass,
        (true, false) => Transition::PassToFail,
    }
}

/// `tdd oracle` exit code for a valid Fail→Pass oracle.
pub const ORACLE_EXIT_VALID: i32 = 0;
/// `tdd oracle` exit code for a complete-or-missing proof pair that is not a
/// valid Fail→Pass oracle (`fail_to_fail` / `pass_to_pass` / `pass_to_fail` /
/// `unknown`).
pub const ORACLE_EXIT_INVALID: i32 = 1;
/// `tdd oracle` exit code when a proof artifact exists but could not be read,
/// so the transition could not be determined. Distinct from
/// [`ORACLE_EXIT_INVALID`] so a human can tell "looked and found it invalid"
/// from "could not look"; both are non-zero, and `condukt state check-oracle`
/// (`crates/condukt/src/oracle.rs` `verdict_from_oracle_output`) maps every
/// non-zero exit to `valid_fp_oracle=false, fallback=false` (reject).
pub const ORACLE_EXIT_UNDETERMINED: i32 = 2;

/// Build the `tdd oracle` JSON report and its exit code from the RED/GREEN
/// `passed` reads.
///
/// - `Known(None)` = the artifact is missing/corrupt: the transition is
///   `"unknown"` and the oracle is not valid (exit [`ORACLE_EXIT_INVALID`]).
/// - Either side `Undetermined` (the artifact could not be read): the
///   transition is `"undetermined"`, `valid_fp_oracle` is `false`, the
///   unreadable side's `has_*` is `null` (neither "present" nor "absent" was
///   observed), the reasons are listed under `"undetermined"`, and the exit
///   code is [`ORACLE_EXIT_UNDETERMINED`].
///
/// Pure, so it is fully unit-testable.
pub fn oracle_report(
    pre_passed: Determination<Option<bool>>,
    post_passed: Determination<Option<bool>>,
) -> (Value, i32) {
    match (pre_passed, post_passed) {
        (Determination::Known(pre), Determination::Known(post)) => {
            let (transition, valid) = match (pre, post) {
                (Some(pre), Some(post)) => {
                    let t = classify(pre, post);
                    (t.as_snake(), t.is_valid_oracle())
                }
                _ => ("unknown", false),
            };
            let report = json!({
                "transition": transition,
                "valid_fp_oracle": valid,
                "has_red": pre.is_some(),
                "has_green": post.is_some(),
            });
            let code = if valid {
                ORACLE_EXIT_VALID
            } else {
                ORACLE_EXIT_INVALID
            };
            (report, code)
        }
        (pre, post) => {
            let side = |d: &Determination<Option<bool>>| -> (Value, Option<String>) {
                match d {
                    Determination::Known(v) => (Value::Bool(v.is_some()), None),
                    Determination::Undetermined(why) => {
                        (Value::Null, Some(why.as_str().to_string()))
                    }
                }
            };
            let (has_red, red_why) = side(&pre);
            let (has_green, green_why) = side(&post);
            let report = json!({
                "transition": "undetermined",
                "valid_fp_oracle": false,
                "has_red": has_red,
                "has_green": has_green,
                "undetermined": {
                    "red": red_why,
                    "green": green_why,
                },
            });
            (report, ORACLE_EXIT_UNDETERMINED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_all_four_transitions() {
        assert_eq!(classify(false, true), Transition::FailToPass);
        assert_eq!(classify(false, false), Transition::FailToFail);
        assert_eq!(classify(true, true), Transition::PassToPass);
        assert_eq!(classify(true, false), Transition::PassToFail);
    }

    #[test]
    fn only_fail_to_pass_is_a_valid_oracle() {
        assert!(Transition::FailToPass.is_valid_oracle());
        assert!(!Transition::FailToFail.is_valid_oracle());
        assert!(!Transition::PassToPass.is_valid_oracle());
        assert!(!Transition::PassToFail.is_valid_oracle());
    }

    #[test]
    fn snake_case_names_are_stable() {
        assert_eq!(Transition::FailToPass.as_snake(), "fail_to_pass");
        assert_eq!(Transition::FailToFail.as_snake(), "fail_to_fail");
        assert_eq!(Transition::PassToPass.as_snake(), "pass_to_pass");
        assert_eq!(Transition::PassToFail.as_snake(), "pass_to_fail");
    }

    #[test]
    fn oracle_report_valid_fp() {
        let (r, _) = oracle_report(
            Determination::known(Some(false)),
            Determination::known(Some(true)),
        );
        assert_eq!(r["transition"], "fail_to_pass");
        assert_eq!(r["valid_fp_oracle"], true);
        assert_eq!(r["has_red"], true);
        assert_eq!(r["has_green"], true);
    }

    #[test]
    fn oracle_report_non_fp_transitions_are_invalid() {
        for (pre, post, name) in [
            (false, false, "fail_to_fail"),
            (true, true, "pass_to_pass"),
            (true, false, "pass_to_fail"),
        ] {
            let (r, _) = oracle_report(
                Determination::known(Some(pre)),
                Determination::known(Some(post)),
            );
            assert_eq!(r["transition"], name);
            assert_eq!(r["valid_fp_oracle"], false);
        }
    }

    #[test]
    fn oracle_report_missing_artifacts_are_unknown_and_invalid() {
        for (pre, post, has_red, has_green) in [
            (None, None, false, false),
            (Some(false), None, true, false),
            (None, Some(true), false, true),
        ] {
            let (r, _) = oracle_report(Determination::known(pre), Determination::known(post));
            assert_eq!(r["transition"], "unknown");
            assert_eq!(r["valid_fp_oracle"], false);
            assert_eq!(r["has_red"], has_red);
            assert_eq!(r["has_green"], has_green);
        }
    }
}
