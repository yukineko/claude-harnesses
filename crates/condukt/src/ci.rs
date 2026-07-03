//! Deterministic CI-conclusion parsing — turns raw `gh` CI-status output
//! (either `gh pr checks` plain text or `gh run list --json ...` /
//! `gh pr checks --json ...` JSON) into a small, decision-ready enum.
//!
//! Design invariants preserved here (mirrors [`crate::pr`]):
//!
//! 1. **Pure, no side effects**: [`parse_ci_checks`] takes the already-captured
//!    `gh` stdout as a `&str` and returns a [`CiConclusion`]. It never spawns a
//!    process, reads the clock, or touches the filesystem, so it is fully
//!    unit-testable without `gh` installed.
//!
//! 2. **Deterministic**: the same input string always yields the same
//!    [`CiConclusion`] — no hidden state, no randomness.
//!
//! 3. **Fail-soft**: unparsable / empty input never panics. It degrades to the
//!    conservative [`CiConclusion::Unknown`] rather than guessing `Success`.
//!
//! 4. **Priority order**: when a status set carries mixed signals (e.g. one
//!    check failed while another is still queued), the verdict is `Failure` >
//!    `Pending` > `Success` — a single failing check always wins, and a
//!    still-running check is never mistaken for a clean pass.

// Not yet wired into a CLI subcommand — consumed by the CI-conclusion
// integration in a follow-up task. Keep the public surface warning-free until
// then rather than gating unit tests behind a caller that doesn't exist yet.
#![allow(dead_code)]

use serde_json::Value;

/// The deterministic verdict [`parse_ci_checks`] maps `gh` CI-status output to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiConclusion {
    /// Every observed check reported a success-class status
    /// (`success`, `pass`, `completed` with no failure/pending signal).
    Success,
    /// At least one check reported a failure-class status
    /// (`failure`, `fail`, `cancelled`, `error`, `timed_out`).
    Failure,
    /// At least one check is still running/queued and none has failed
    /// (`pending`, `queued`, `in_progress`, `waiting`, `requested`).
    Pending,
    /// The input was empty or carried no recognizable status tokens at all.
    /// A conservative fallback — never treated as `Success`.
    Unknown,
}

/// Parse `gh` CI-status output (JSON or `gh pr checks` plain text) into a
/// [`CiConclusion`]. Pure: no IO, no clock, no panic.
///
/// Tries JSON first (covers `gh run list --json conclusion,status` and
/// `gh pr checks --json state,bucket,...`, whether the top level is an array
/// of check objects or a single object). Falls back to scanning the raw text
/// line-by-line (covers `gh pr checks`'s tab-separated plain-text output).
pub fn parse_ci_checks(output: &str) -> CiConclusion {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return CiConclusion::Unknown;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        let mut statuses = Vec::new();
        collect_status_strings(&value, &mut statuses);
        if !statuses.is_empty() {
            return classify(&statuses);
        }
        // Valid JSON but no recognizable status keys — fall through to the
        // text scan below rather than guessing.
    }
    let statuses: Vec<String> = trimmed
        .lines()
        .map(|line| line.to_lowercase())
        .filter(|line| !line.trim().is_empty())
        .collect();
    if statuses.is_empty() {
        return CiConclusion::Unknown;
    }
    classify(&statuses)
}

/// Recursively collect lowercased status-bearing strings from a `gh --json`
/// value: any `conclusion` / `state` / `bucket` / `status` string field, at
/// any depth of arrays/objects gh might nest the check list under.
fn collect_status_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_status_strings(item, out);
            }
        }
        Value::Object(map) => {
            for key in ["conclusion", "state", "bucket", "status"] {
                if let Some(Value::String(s)) = map.get(key) {
                    out.push(s.to_lowercase());
                }
            }
            // Some shapes nest the check list under a wrapper key (e.g.
            // `{"statusCheckRollup": [...]}`); recurse into all values so
            // those are still found.
            for v in map.values() {
                if v.is_array() || v.is_object() {
                    collect_status_strings(v, out);
                }
            }
        }
        _ => {}
    }
}

/// Deterministic verdict from a set of already-lowercased status strings, in
/// `Failure` > `Pending` > `Success` priority order.
fn classify(statuses: &[String]) -> CiConclusion {
    let has_failure = statuses.iter().any(|s| {
        s.contains("fail")
            || s.contains("error")
            || s.contains("cancelled")
            || s.contains("canceled")
            || s.contains("timed_out")
            || s.contains("timeout")
            || s.contains("action_required")
    });
    if has_failure {
        return CiConclusion::Failure;
    }
    let has_pending = statuses.iter().any(|s| {
        s.contains("pending")
            || s.contains("queued")
            || s.contains("in_progress")
            || s.contains("waiting")
            || s.contains("requested")
    });
    if has_pending {
        return CiConclusion::Pending;
    }
    let has_success = statuses
        .iter()
        .any(|s| s.contains("success") || s.contains("pass") || s.contains("completed"));
    if has_success {
        return CiConclusion::Success;
    }
    CiConclusion::Unknown
}

/// The deterministic next action [`decide_ci_action`] maps a [`CiConclusion`]
/// (plus whether `gh` could actually be queried) onto. This is the control-flow
/// verdict the orchestrator acts on — no LLM, no IO, no clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiVerdict {
    /// CI is still running/queued — keep polling, do not merge yet.
    Wait,
    /// CI reported a failure — re-enter the worker to fix it.
    Reenter,
    /// CI is green — safe to merge.
    Merge,
    /// Fail-soft: `gh` was unavailable or the CI status could not be
    /// determined, so we fall back to local-only behavior (mirrors
    /// [`crate::pr::PrOutcome::DegradedLocalOnly`]). Carries a human-readable
    /// reason. Never panics, never guesses `Merge`.
    DegradedLocalOnly { reason: String },
}

/// Deterministic state machine: map a [`CiConclusion`] onto the next
/// [`CiVerdict`]. Pure — no IO, no clock, no panic, no LLM.
///
/// `gh_available` reflects whether the `gh` CI-status query could be run and
/// captured at all (gh present + authed + command succeeded). When it is
/// `false`, the `conclusion` is meaningless and we degrade to
/// [`CiVerdict::DegradedLocalOnly`] regardless of its value.
///
/// Mapping (when `gh_available`):
/// - [`CiConclusion::Pending`] ⇒ [`CiVerdict::Wait`]
/// - [`CiConclusion::Failure`] ⇒ [`CiVerdict::Reenter`]
/// - [`CiConclusion::Success`] ⇒ [`CiVerdict::Merge`]
/// - [`CiConclusion::Unknown`] ⇒ [`CiVerdict::DegradedLocalOnly`] (conservative:
///   we never treat an unrecognized status as a green light to merge).
pub fn decide_ci_action(conclusion: CiConclusion, gh_available: bool) -> CiVerdict {
    if !gh_available {
        return CiVerdict::DegradedLocalOnly {
            reason: "gh CI status unavailable (gh absent, unauthed, or query failed)".to_string(),
        };
    }
    match conclusion {
        CiConclusion::Pending => CiVerdict::Wait,
        CiConclusion::Failure => CiVerdict::Reenter,
        CiConclusion::Success => CiVerdict::Merge,
        CiConclusion::Unknown => CiVerdict::DegradedLocalOnly {
            reason: "CI conclusion could not be determined from gh output".to_string(),
        },
    }
}

/// Build the argv for `gh pr checks <pr> --json name,state,bucket` — the
/// CI-status query the poll subcommand feeds through the injected runner into
/// [`parse_ci_checks`]. Pure, deterministic, no side effects.
///
/// `pr` is the PR number or URL. The `--json` projection is chosen so the output
/// is the JSON shape [`parse_ci_checks`] already recognizes (`state`/`bucket`
/// fields), independent of `gh`'s human-readable plain-text formatting.
pub fn build_ci_checks_args(pr: &str) -> Vec<String> {
    vec![
        "pr".to_string(),
        "checks".to_string(),
        pr.to_string(),
        "--json".to_string(),
        "name,state,bucket".to_string(),
    ]
}

/// Fetch CI status through an injected command runner and map it to a
/// [`CiVerdict`]. Pure with respect to `gh`: the runner is injected exactly like
/// [`crate::pr::detect_gh`], so this is fully unit-testable without `gh`.
///
/// `run(argv)` returns:
/// - `Some((_success, output))` — the process spawned; we parse `output` with
///   [`parse_ci_checks`] and decide with `gh_available = true`. The exit status
///   is intentionally ignored: `gh pr checks` signals CI *state* (fail/pending)
///   through non-zero exit codes while still emitting the parseable status, so a
///   non-zero exit is NOT an availability failure. Unrecognizable output (e.g. an
///   auth-error blob) parses to [`CiConclusion::Unknown`] and therefore degrades.
/// - `None` — the binary could not be spawned (gh absent); we decide with
///   `gh_available = false`, which yields [`CiVerdict::DegradedLocalOnly`].
///
/// Never panics, never guesses `Merge` — fail-soft by construction.
pub fn fetch_and_decide<R: Fn(&[&str]) -> Option<(bool, String)>>(
    run: R,
    argv: &[&str],
) -> CiVerdict {
    match run(argv) {
        Some((_success, output)) => decide_ci_action(parse_ci_checks(&output), true),
        None => decide_ci_action(CiConclusion::Unknown, false),
    }
}

/// The single merge gate: a worktree merge is authorized ONLY when CI is green.
/// Every other verdict (`Wait`, `Reenter`, `DegradedLocalOnly`) returns `false`,
/// so the caller can never merge on a non-green path.
pub fn should_merge(verdict: &CiVerdict) -> bool {
    matches!(verdict, CiVerdict::Merge)
}

/// Poll CI via the injected runner, decide the verdict, and invoke `on_merge`
/// EXCLUSIVELY when the verdict is [`CiVerdict::Merge`]. Returns the verdict.
///
/// This is the pure heart of the poll subcommand's merge gate: `run` is the sole
/// gh dependency (injected) and `on_merge` is the sole side-effecting escape
/// hatch, wired so it fires on — and only on — the green path. `Wait`, `Reenter`,
/// and `DegradedLocalOnly` never call `on_merge`.
pub fn poll_and_maybe_merge<R, M>(run: R, argv: &[&str], on_merge: M) -> CiVerdict
where
    R: Fn(&[&str]) -> Option<(bool, String)>,
    M: FnOnce(),
{
    let verdict = fetch_and_decide(run, argv);
    if should_merge(&verdict) {
        on_merge();
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON_ALL_SUCCESS: &str = r#"[
        {"name":"build","status":"completed","conclusion":"success"},
        {"name":"test","status":"completed","conclusion":"success"}
    ]"#;

    const JSON_ONE_FAILURE: &str = r#"[
        {"name":"build","status":"completed","conclusion":"success"},
        {"name":"test","status":"completed","conclusion":"failure"}
    ]"#;

    const JSON_ONE_PENDING: &str = r#"[
        {"name":"build","status":"completed","conclusion":"success"},
        {"name":"test","status":"in_progress","conclusion":null}
    ]"#;

    const TEXT_ALL_SUCCESS: &str =
        "build\tpass\t12s\thttps://example/1\ntest\tpass\t34s\thttps://example/2\n";

    const TEXT_ONE_FAILURE: &str =
        "build\tpass\t12s\thttps://example/1\ntest\tfail\t34s\thttps://example/2\n";

    const TEXT_ONE_PENDING: &str =
        "build\tpass\t12s\thttps://example/1\ntest\tpending\t0s\thttps://example/2\n";

    #[test]
    fn json_all_success_yields_success() {
        assert_eq!(parse_ci_checks(JSON_ALL_SUCCESS), CiConclusion::Success);
    }

    #[test]
    fn json_one_failure_yields_failure_even_with_success_present() {
        assert_eq!(parse_ci_checks(JSON_ONE_FAILURE), CiConclusion::Failure);
    }

    #[test]
    fn json_one_pending_yields_pending_when_no_failure() {
        assert_eq!(parse_ci_checks(JSON_ONE_PENDING), CiConclusion::Pending);
    }

    #[test]
    fn text_all_success_yields_success() {
        assert_eq!(parse_ci_checks(TEXT_ALL_SUCCESS), CiConclusion::Success);
    }

    #[test]
    fn text_one_failure_yields_failure() {
        assert_eq!(parse_ci_checks(TEXT_ONE_FAILURE), CiConclusion::Failure);
    }

    #[test]
    fn text_one_pending_yields_pending() {
        assert_eq!(parse_ci_checks(TEXT_ONE_PENDING), CiConclusion::Pending);
    }

    /// Fail-soft: empty / unparsable input never panics and never guesses
    /// `Success` — it degrades to `Unknown`.
    #[test]
    fn empty_input_is_unknown_not_success() {
        assert_eq!(parse_ci_checks(""), CiConclusion::Unknown);
        assert_eq!(parse_ci_checks("   \n  \n"), CiConclusion::Unknown);
    }

    /// Determinism proof: the same input parsed repeatedly (including across
    /// the JSON and text code paths) always yields the exact same verdict.
    #[test]
    fn parse_is_deterministic_across_repeated_calls() {
        for input in [
            JSON_ALL_SUCCESS,
            JSON_ONE_FAILURE,
            JSON_ONE_PENDING,
            TEXT_ALL_SUCCESS,
            TEXT_ONE_FAILURE,
            TEXT_ONE_PENDING,
        ] {
            let first = parse_ci_checks(input);
            for _ in 0..5 {
                assert_eq!(
                    parse_ci_checks(input),
                    first,
                    "parse_ci_checks must be deterministic for input: {input:?}"
                );
            }
        }
    }

    #[test]
    fn pending_yields_wait() {
        assert_eq!(
            decide_ci_action(CiConclusion::Pending, true),
            CiVerdict::Wait
        );
    }

    #[test]
    fn failure_yields_reenter() {
        assert_eq!(
            decide_ci_action(CiConclusion::Failure, true),
            CiVerdict::Reenter
        );
    }

    #[test]
    fn success_yields_merge() {
        assert_eq!(
            decide_ci_action(CiConclusion::Success, true),
            CiVerdict::Merge
        );
    }

    /// Fail-soft: gh unavailable ⇒ DegradedLocalOnly regardless of conclusion,
    /// and never a `Merge`.
    #[test]
    fn gh_unavailable_yields_degraded_local_only() {
        for conclusion in [
            CiConclusion::Success,
            CiConclusion::Failure,
            CiConclusion::Pending,
            CiConclusion::Unknown,
        ] {
            let verdict = decide_ci_action(conclusion, false);
            assert!(
                matches!(verdict, CiVerdict::DegradedLocalOnly { .. }),
                "gh-unavailable must degrade, got {verdict:?} for {conclusion:?}"
            );
            assert_ne!(verdict, CiVerdict::Merge);
        }
    }

    /// Conservative fallback: an Unknown conclusion (even with gh available)
    /// never greenlights a merge — it degrades.
    #[test]
    fn unknown_conclusion_degrades_not_merges() {
        assert!(matches!(
            decide_ci_action(CiConclusion::Unknown, true),
            CiVerdict::DegradedLocalOnly { .. }
        ));
    }

    /// Determinism proof: same (conclusion, gh_available) input always maps to
    /// the same verdict.
    #[test]
    fn decide_ci_action_is_deterministic() {
        for gh_available in [true, false] {
            for conclusion in [
                CiConclusion::Success,
                CiConclusion::Failure,
                CiConclusion::Pending,
                CiConclusion::Unknown,
            ] {
                let first = decide_ci_action(conclusion, gh_available);
                for _ in 0..5 {
                    assert_eq!(
                        decide_ci_action(conclusion, gh_available),
                        first,
                        "decide_ci_action must be deterministic for ({conclusion:?}, {gh_available})"
                    );
                }
            }
        }
    }

    /// The gh-checks argv is exact and deterministic, and projects the JSON
    /// shape `parse_ci_checks` recognizes.
    #[test]
    fn build_ci_checks_args_is_deterministic() {
        assert_eq!(
            build_ci_checks_args("42"),
            vec![
                "pr".to_string(),
                "checks".to_string(),
                "42".to_string(),
                "--json".to_string(),
                "name,state,bucket".to_string(),
            ]
        );
    }

    /// A stub runner returning all-green JSON drives `fetch_and_decide` to
    /// `Merge` — the only verdict that authorizes a merge.
    #[test]
    fn fetch_and_decide_green_yields_merge() {
        let run = |_argv: &[&str]| Some((true, JSON_ALL_SUCCESS.to_string()));
        assert_eq!(
            fetch_and_decide(run, &["pr", "checks", "1"]),
            CiVerdict::Merge
        );
    }

    /// A failing check drives `Reenter` — even though `gh pr checks` would exit
    /// non-zero, the parseable output still classifies the failure.
    #[test]
    fn fetch_and_decide_failure_yields_reenter() {
        let run = |_argv: &[&str]| Some((false, JSON_ONE_FAILURE.to_string()));
        assert_eq!(
            fetch_and_decide(run, &["pr", "checks", "1"]),
            CiVerdict::Reenter
        );
    }

    /// A still-running check drives `Wait` (gh would exit non-zero here too).
    #[test]
    fn fetch_and_decide_pending_yields_wait() {
        let run = |_argv: &[&str]| Some((false, JSON_ONE_PENDING.to_string()));
        assert_eq!(
            fetch_and_decide(run, &["pr", "checks", "1"]),
            CiVerdict::Wait
        );
    }

    /// Fail-soft: gh absent (runner returns None) degrades — never `Merge`.
    #[test]
    fn fetch_and_decide_gh_absent_degrades() {
        let run = |_argv: &[&str]| None;
        let verdict = fetch_and_decide(run, &["pr", "checks", "1"]);
        assert!(matches!(verdict, CiVerdict::DegradedLocalOnly { .. }));
        assert_ne!(verdict, CiVerdict::Merge);
    }

    /// Fail-soft: gh spawned but emitted a blob carrying no recognizable status
    /// token — parses to Unknown and degrades rather than guessing `Merge`.
    #[test]
    fn fetch_and_decide_unrecognized_output_degrades() {
        let run = |_argv: &[&str]| Some((false, "no recognizable checks here".to_string()));
        let verdict = fetch_and_decide(run, &["pr", "checks", "1"]);
        assert!(matches!(verdict, CiVerdict::DegradedLocalOnly { .. }));
        assert_ne!(verdict, CiVerdict::Merge);
    }

    /// `should_merge` is true for exactly one verdict.
    #[test]
    fn should_merge_only_for_merge() {
        assert!(should_merge(&CiVerdict::Merge));
        assert!(!should_merge(&CiVerdict::Wait));
        assert!(!should_merge(&CiVerdict::Reenter));
        assert!(!should_merge(&CiVerdict::DegradedLocalOnly {
            reason: "x".to_string()
        }));
    }

    /// The merge gate fires the `on_merge` callback ONLY on the green path.
    /// A green stub triggers exactly one merge; every non-green stub triggers
    /// zero. This is the executable proof that "Merge 以外では merge 経路に入らない".
    #[test]
    fn poll_and_maybe_merge_fires_only_on_green() {
        use std::cell::Cell;

        // Green ⇒ exactly one merge.
        let merged = Cell::new(0u32);
        let verdict = poll_and_maybe_merge(
            |_| Some((true, JSON_ALL_SUCCESS.to_string())),
            &["pr", "checks", "1"],
            || merged.set(merged.get() + 1),
        );
        assert_eq!(verdict, CiVerdict::Merge);
        assert_eq!(merged.get(), 1, "green must trigger exactly one merge");

        // Every non-green outcome ⇒ zero merges.
        for run_out in [
            Some((false, JSON_ONE_FAILURE.to_string())), // Reenter
            Some((false, JSON_ONE_PENDING.to_string())), // Wait
            Some((false, "unparsable".to_string())),     // DegradedLocalOnly (Unknown)
            None,                                        // DegradedLocalOnly (gh absent)
        ] {
            let merged = Cell::new(0u32);
            let verdict = poll_and_maybe_merge(
                |_| run_out.clone(),
                &["pr", "checks", "1"],
                || merged.set(merged.get() + 1),
            );
            assert_ne!(verdict, CiVerdict::Merge);
            assert_eq!(
                merged.get(),
                0,
                "non-green verdict {verdict:?} must NOT enter the merge path"
            );
        }
    }
}
