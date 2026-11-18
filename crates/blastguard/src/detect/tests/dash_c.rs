//! `bash -c` / `sh -c` invocations: bg1 and the B.5 no-opinion twin for a
//! `-c` without a program. Moved out of `tests.rs` unchanged, which is at
//! its file-growth cap.

use super::*;

#[test]
fn bg1_benign_dash_c_invocations_stay_allowed() {
    assert_eq!(bash(r#"sh -c "ls""#), Decision::Allow);
    assert_eq!(bash("bash -c 'echo hi'"), Decision::Allow);
    assert_eq!(bash("bash -c -o pipefail 'cargo test'"), Decision::Allow);
    assert_eq!(bash("sh -c ''"), Decision::Allow);
    assert_eq!(bash("bash -c"), Decision::Allow);
}

/// `bash -c` / `sh -c` with no operand run nothing: observed 2026-11-19
/// (relay bgr-41bb2e87c9a7) — "-c: option requires an argument", exit 2,
/// also with a piped stdin. The B.5 layer must give NO opinion for that exact
/// shape: `raise` returns the incoming verdict unchanged. The controls pin
/// that the carve-out is that one shape only (any other operand-less `-c`
/// spelling, and the unobserved shells, stay B.5 Undetermined).
#[test]
fn b5_gives_no_opinion_for_a_dash_c_without_a_program() {
    let scope = SafeRoots::none();
    let judge = |_: &str| Decision::Allow;
    let probes = ast::b5::Probes {
        judge: &judge,
        scope: &scope,
    };
    let raise = |c: &str| ast::b5::raise(Decision::Allow, c, &probes);
    assert_eq!(raise("bash -c"), Decision::Allow);
    assert_eq!(raise("sh -c"), Decision::Allow);
    // Never lowers: an incoming Ask / Deny is returned as is.
    let legacy_ask = Decision::ask("legacy-marker");
    assert_eq!(
        ast::b5::raise(legacy_ask.clone(), "bash -c", &probes),
        legacy_ask
    );
    for c in ["bash -c --", "bash -xc", "zsh -c", "bash -c -o pipefail"] {
        let d = raise(c);
        assert!(
            matches!(&d, Decision::Ask(r) if r.contains("Table B.5: Undetermined")),
            "{c}: {d:?}"
        );
    }
}
