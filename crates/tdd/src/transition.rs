//! Test-transition classification: given a task's RED (pre) and GREEN (post)
//! proof results, decide which of the four RED→GREEN transitions occurred and
//! whether it is a *valid* test-first (Fail→Pass) oracle.
//!
//! The only transition that proves test-first worked is `FailToPass`: the test
//! failed before the implementation and passes after it. Every other transition
//! (a test that never failed, one that still fails, or one that regressed) is
//! not a valid oracle.
//!
//! "The test" means ONE test command: a RED recorded with command A and a
//! GREEN recorded with an unrelated command B prove nothing about A going
//! Fail→Pass. `tdd oracle` therefore goes through [`oracle_report_same_cmd`],
//! which only accepts a `FailToPass` whose RED and GREEN proofs recorded the
//! same `cmd` (backlog dc37a4da).

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

/// `tdd oracle`'s full verdict: [`oracle_report`] over the `passed` reads,
/// plus the requirement that the RED and GREEN proofs were recorded with the
/// SAME test command (`red_cmd` / `green_cmd`, from `proof::read_cmd`).
///
/// - Either `cmd` read `Undetermined` (artifact unreadable): transition
///   `"undetermined"`, `valid_fp_oracle: false`, exit
///   [`ORACLE_EXIT_UNDETERMINED`] — "could not look", never a valid oracle.
/// - A `fail_to_pass` whose commands are not both present and equal
///   (different commands, or a proof with no recorded `cmd`, which cannot show
///   sameness): transition `"command_mismatch"`, `valid_fp_oracle: false`,
///   exit [`ORACLE_EXIT_INVALID`]. It is deliberately NOT `"unknown"`:
///   `condukt state check-oracle` degrades `"unknown"` to its fallback, while
///   any other non-valid transition is a reject.
/// - Otherwise the [`oracle_report`] verdict stands. The recorded commands are
///   added as `red_cmd` / `green_cmd` (null when absent or unreadable).
pub fn oracle_report_same_cmd(
    pre_passed: Determination<Option<bool>>,
    post_passed: Determination<Option<bool>>,
    red_cmd: Determination<Option<String>>,
    green_cmd: Determination<Option<String>>,
) -> (Value, i32) {
    let (mut report, code) = oracle_report(pre_passed, post_passed);
    let (red_cmd, green_cmd) = match (red_cmd, green_cmd) {
        (Determination::Known(r), Determination::Known(g)) => (r, g),
        (r, g) => {
            let why = |d: &Determination<Option<String>>| -> Option<String> {
                match d {
                    Determination::Known(_) => None,
                    Determination::Undetermined(why) => Some(why.as_str().to_string()),
                }
            };
            let (red_why, green_why) = (why(&r), why(&g));
            if code != ORACLE_EXIT_UNDETERMINED {
                report["undetermined"] = json!({ "red": red_why, "green": green_why });
            }
            report["transition"] = json!("undetermined");
            report["valid_fp_oracle"] = json!(false);
            report["red_cmd"] = Value::Null;
            report["green_cmd"] = Value::Null;
            return (report, ORACLE_EXIT_UNDETERMINED);
        }
    };
    let same = matches!((&red_cmd, &green_cmd), (Some(r), Some(g)) if r == g);
    report["red_cmd"] = json!(red_cmd);
    report["green_cmd"] = json!(green_cmd);
    if code == ORACLE_EXIT_VALID && !same {
        report["transition"] = json!("command_mismatch");
        report["valid_fp_oracle"] = json!(false);
        return (report, ORACLE_EXIT_INVALID);
    }
    (report, code)
}

/// One recorded proof file (one slot of one kind): its `cmd` and `passed`
/// reads, each with the same tri-state as `proof::read_cmd` / `read_passed`.
pub struct SlotProof {
    pub cmd: Determination<Option<String>>,
    pub passed: Determination<Option<bool>>,
}

fn read_known<T: Clone>(d: &Determination<T>) -> Result<T, String> {
    match d {
        Determination::Known(v) => Ok(v.clone()),
        Determination::Undetermined(w) => Err(w.as_str().to_string()),
    }
}

/// `tdd oracle`'s verdict over ALL slots of a task (backlog 519f089e: one
/// slot per test command, so a second command no longer erases the first).
///
/// * Either list `Undetermined`, or any proof in it with an unreadable `cmd` /
///   `passed`: transition `"undetermined"`, exit [`ORACLE_EXIT_UNDETERMINED`].
/// * At most one RED proof and at most one GREEN proof: exactly
///   [`oracle_report_same_cmd`] (the single-slot verdict, unchanged — including
///   `"command_mismatch"` for a RED of command A and a GREEN of command B).
/// * Otherwise the proofs are grouped by recorded `cmd`. The oracle is valid
///   (exit 0) ONLY when EVERY command has a RED and a GREEN proof and goes
///   Fail->Pass; a command with only one side, a proof with no recorded `cmd`,
///   or two proofs of one kind for one command makes it invalid (exit 1). A RED
///   of command A is never paired with a GREEN of command B.
///   The report lists every command under `"slots"`; the top-level
///   `"transition"` is `"fail_to_pass"` when valid, else that of the first
///   non-valid slot.
///
/// This never compares trees (user ruling 2026-11-17): that is `condukt merge`'s
/// job, from the identity each proof records.
pub fn oracle_report_slots(
    reds: Determination<Vec<SlotProof>>,
    greens: Determination<Vec<SlotProof>>,
) -> (Value, i32) {
    let (reds, greens) = match (reds, greens) {
        (Determination::Known(r), Determination::Known(g)) => (r, g),
        (r, g) => {
            let why = |d: &Determination<Vec<SlotProof>>| match d {
                Determination::Known(_) => None,
                Determination::Undetermined(w) => Some(w.as_str().to_string()),
            };
            let report = json!({
                "transition": "undetermined",
                "valid_fp_oracle": false,
                "has_red": Value::Null,
                "has_green": Value::Null,
                "undetermined": { "red": why(&r), "green": why(&g) },
            });
            return (report, ORACLE_EXIT_UNDETERMINED);
        }
    };
    // Resolve every proof to concrete (cmd, passed) values. A proof whose cmd
    // or passed could not be read is collected as a reason and the oracle
    // answers "undetermined" below — no value is invented for it.
    type Resolved = Vec<(Option<String>, Option<bool>)>;
    let mut why: Vec<String> = Vec::new();
    let mut resolve = |kind: &str, list: &[SlotProof]| -> Resolved {
        let mut out = Vec::new();
        for p in list {
            match (read_known(&p.cmd), read_known(&p.passed)) {
                (Ok(cmd), Ok(passed)) => out.push((cmd, passed)),
                (cmd, passed) => {
                    if let Err(e) = cmd {
                        why.push(format!("{kind} cmd: {e}"));
                    }
                    if let Err(e) = passed {
                        why.push(format!("{kind} passed: {e}"));
                    }
                }
            }
        }
        out
    };
    let reds_r = resolve("red", &reds);
    let greens_r = resolve("green", &greens);
    if !why.is_empty() {
        let report = json!({
            "transition": "undetermined",
            "valid_fp_oracle": false,
            "has_red": !reds.is_empty(),
            "has_green": !greens.is_empty(),
            "undetermined": why,
        });
        return (report, ORACLE_EXIT_UNDETERMINED);
    }
    if reds_r.len() <= 1 && greens_r.len() <= 1 {
        let one = |l: &Resolved| -> (Determination<Option<bool>>, Determination<Option<String>>) {
            match l.first() {
                Some((cmd, passed)) => (
                    Determination::known(*passed),
                    Determination::known(cmd.clone()),
                ),
                None => (Determination::known(None), Determination::known(None)),
            }
        };
        let (pre, red_cmd) = one(&reds_r);
        let (post, green_cmd) = one(&greens_r);
        return oracle_report_same_cmd(pre, post, red_cmd, green_cmd);
    }
    // Group by command (None = a proof that records no command).
    type Passes = Vec<Option<bool>>;
    let mut groups: std::collections::BTreeMap<Option<String>, (Passes, Passes)> =
        std::collections::BTreeMap::new();
    for (cmd, passed) in &reds_r {
        groups.entry(cmd.clone()).or_default().0.push(*passed);
    }
    for (cmd, passed) in &greens_r {
        groups.entry(cmd.clone()).or_default().1.push(*passed);
    }
    let mut slots = Vec::new();
    let mut first_bad: Option<String> = None;
    for (cmd, (r, g)) in &groups {
        let (transition, valid) = if cmd.is_none() {
            ("command_mismatch".to_string(), false)
        } else if r.len() > 1 || g.len() > 1 {
            ("duplicate_slot".to_string(), false)
        } else {
            let (rep, _) = oracle_report(
                Determination::known(r.first().copied().flatten()),
                Determination::known(g.first().copied().flatten()),
            );
            (
                rep["transition"].as_str().unwrap_or("unknown").to_string(),
                rep["valid_fp_oracle"].as_bool() == Some(true),
            )
        };
        if !valid && first_bad.is_none() {
            first_bad = Some(transition.clone());
        }
        slots.push(json!({
            "cmd": cmd,
            "transition": transition,
            "valid_fp_oracle": valid,
            "has_red": !r.is_empty(),
            "has_green": !g.is_empty(),
        }));
    }
    let valid = first_bad.is_none() && !slots.is_empty();
    let report = json!({
        "transition": first_bad.unwrap_or_else(|| "fail_to_pass".to_string()),
        "valid_fp_oracle": valid,
        "has_red": !reds.is_empty(),
        "has_green": !greens.is_empty(),
        "slots": slots,
    });
    (
        report,
        if valid {
            ORACLE_EXIT_VALID
        } else {
            ORACLE_EXIT_INVALID
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(cmd: Option<&str>, passed: Option<bool>) -> SlotProof {
        SlotProof {
            cmd: Determination::known(cmd.map(str::to_string)),
            passed: Determination::known(passed),
        }
    }

    #[test]
    fn slots_all_fail_to_pass_is_valid() {
        let (r, code) = oracle_report_slots(
            Determination::known(vec![sp(Some("A"), Some(false)), sp(Some("B"), Some(false))]),
            Determination::known(vec![sp(Some("B"), Some(true)), sp(Some("A"), Some(true))]),
        );
        assert_eq!(code, ORACLE_EXIT_VALID, "{r}");
        assert_eq!(r["valid_fp_oracle"], true);
        assert_eq!(r["slots"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn slots_unpaired_command_is_invalid() {
        // RED A, RED B, GREEN A: B never went green. RED A paired with a
        // GREEN of another command is never valid.
        let (r, code) = oracle_report_slots(
            Determination::known(vec![sp(Some("A"), Some(false)), sp(Some("B"), Some(false))]),
            Determination::known(vec![sp(Some("A"), Some(true))]),
        );
        assert_eq!(code, ORACLE_EXIT_INVALID, "{r}");
        assert_eq!(r["valid_fp_oracle"], false);
        let (r, code) = oracle_report_slots(
            Determination::known(vec![sp(Some("A"), Some(false)), sp(Some("B"), Some(false))]),
            Determination::known(vec![sp(Some("C"), Some(true)), sp(Some("D"), Some(true))]),
        );
        assert_eq!(code, ORACLE_EXIT_INVALID, "{r}");
    }

    #[test]
    fn slots_one_failing_green_makes_it_invalid() {
        let (_, code) = oracle_report_slots(
            Determination::known(vec![sp(Some("A"), Some(false)), sp(Some("B"), Some(false))]),
            Determination::known(vec![sp(Some("A"), Some(true)), sp(Some("B"), Some(false))]),
        );
        assert_eq!(code, ORACLE_EXIT_INVALID);
    }

    #[test]
    fn slots_without_a_recorded_cmd_are_never_valid() {
        let (r, code) = oracle_report_slots(
            Determination::known(vec![sp(Some("A"), Some(false)), sp(None, Some(false))]),
            Determination::known(vec![sp(Some("A"), Some(true)), sp(None, Some(true))]),
        );
        assert_eq!(code, ORACLE_EXIT_INVALID, "{r}");
    }

    #[test]
    fn slots_single_pair_keeps_the_single_slot_verdict() {
        let (r, code) = oracle_report_slots(
            Determination::known(vec![sp(Some("A"), Some(false))]),
            Determination::known(vec![sp(Some("B"), Some(true))]),
        );
        assert_eq!(r["transition"], "command_mismatch");
        assert_eq!(code, ORACLE_EXIT_INVALID);
        let (r, code) =
            oracle_report_slots(Determination::known(vec![]), Determination::known(vec![]));
        assert_eq!(r["transition"], "unknown");
        assert_eq!(code, ORACLE_EXIT_INVALID);
    }

    #[test]
    fn slots_unreadable_is_undetermined_exit_2() {
        let (r, code) = oracle_report_slots(
            Determination::undetermined("cannot list"),
            Determination::known(vec![]),
        );
        assert_eq!(code, ORACLE_EXIT_UNDETERMINED, "{r}");
        assert_eq!(r["valid_fp_oracle"], false);
        let bad = SlotProof {
            cmd: Determination::undetermined("io"),
            passed: Determination::known(Some(false)),
        };
        let (_, code) = oracle_report_slots(
            Determination::known(vec![bad, sp(Some("B"), Some(false))]),
            Determination::known(vec![]),
        );
        assert_eq!(code, ORACLE_EXIT_UNDETERMINED);
    }

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

    #[test]
    fn oracle_report_undetermined_side_names_the_read_error() {
        let (r, code) = oracle_report(
            Determination::known(Some(false)),
            Determination::undetermined("green proof unreadable: EISDIR"),
        );
        assert_eq!(code, ORACLE_EXIT_UNDETERMINED, "{r}");
        assert_eq!(r["transition"], "undetermined");
        assert_eq!(r["valid_fp_oracle"], false);
        assert!(r["undetermined"]["red"].is_null(), "{r}");
        assert!(
            r["undetermined"]["green"]
                .as_str()
                .is_some_and(|s| s.contains("green proof unreadable: EISDIR")),
            "{r}"
        );
    }

    fn cmd(c: &str) -> Determination<Option<String>> {
        Determination::known(Some(c.to_string()))
    }

    #[test]
    fn same_cmd_fail_to_pass_is_valid() {
        let (r, code) = oracle_report_same_cmd(
            Determination::known(Some(false)),
            Determination::known(Some(true)),
            cmd("cargo test"),
            cmd("cargo test"),
        );
        assert_eq!(code, ORACLE_EXIT_VALID, "{r}");
        assert_eq!(r["transition"], "fail_to_pass");
        assert_eq!(r["valid_fp_oracle"], true);
        assert_eq!(r["red_cmd"], "cargo test");
        assert_eq!(r["green_cmd"], "cargo test");
    }

    #[test]
    fn different_cmd_fail_to_pass_is_command_mismatch() {
        let (r, code) = oracle_report_same_cmd(
            Determination::known(Some(false)),
            Determination::known(Some(true)),
            cmd("cargo test a"),
            cmd("cargo test b"),
        );
        assert_eq!(code, ORACLE_EXIT_INVALID, "{r}");
        assert_eq!(r["transition"], "command_mismatch");
        assert_eq!(r["valid_fp_oracle"], false);
    }

    #[test]
    fn absent_cmd_on_either_side_is_command_mismatch() {
        for (red, green) in [
            (Determination::known(None), cmd("x")),
            (cmd("x"), Determination::known(None)),
            (Determination::known(None), Determination::known(None)),
        ] {
            let (r, code) = oracle_report_same_cmd(
                Determination::known(Some(false)),
                Determination::known(Some(true)),
                red,
                green,
            );
            assert_eq!(code, ORACLE_EXIT_INVALID, "{r}");
            assert_eq!(r["transition"], "command_mismatch");
            assert_eq!(r["valid_fp_oracle"], false);
        }
    }

    #[test]
    fn non_fp_transitions_keep_their_name_regardless_of_cmd() {
        let (r, code) = oracle_report_same_cmd(
            Determination::known(Some(false)),
            Determination::known(Some(false)),
            cmd("a"),
            cmd("b"),
        );
        assert_eq!(code, ORACLE_EXIT_INVALID);
        assert_eq!(r["transition"], "fail_to_fail");
        let (r, code) = oracle_report_same_cmd(
            Determination::known(None),
            Determination::known(None),
            Determination::known(None),
            Determination::known(None),
        );
        assert_eq!(code, ORACLE_EXIT_INVALID);
        assert_eq!(r["transition"], "unknown");
    }

    #[test]
    fn undetermined_cmd_read_is_undetermined_exit_2() {
        let und = || Determination::<Option<String>>::undetermined("read blocked");
        for (red, green) in [(und(), cmd("x")), (cmd("x"), und()), (und(), und())] {
            let (r, code) = oracle_report_same_cmd(
                Determination::known(Some(false)),
                Determination::known(Some(true)),
                red,
                green,
            );
            assert_eq!(code, ORACLE_EXIT_UNDETERMINED, "{r}");
            assert_eq!(r["transition"], "undetermined");
            assert_eq!(r["valid_fp_oracle"], false);
            assert!(r["undetermined"].is_object(), "{r}");
        }
    }
}
