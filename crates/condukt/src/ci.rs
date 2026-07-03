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
}
