// 丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// The src modules are compiled in whole (tdd is bin-only); most of their items
// are unused from this file.
#![allow(dead_code, unused_imports)]
//! Fault-injection fail-closed check for tdd's F->P oracle read
//! (backlog 8696dd7e, slice 4).
//!
//! `proof::read_passed` is the verdict-bearing read behind the oracle's
//! `has_red` / `has_green`. `Known(Some(true))` for the green proof is the
//! permissive answer. These tests force EVERY boundary IO entry to
//! `Undetermined` (`FaultPlan::blind()`, via `degrade::assert_fails_closed`)
//! and require (1) the verdict is not permissive, (2) the read is
//! `Undetermined` rather than silently `Known(None)`, (3) `read_author`
//! (through `proof::green` under strict_separation) is refused, and (4)
//! `transition::oracle_report` forwards an Undetermined side as exit 2.
//!
//! Kill targets (each fails one of these tests): `read_passed` or `read_author`
//! reading through `std::fs` (VACUOUS), `read_passed` collapsing `Undetermined`
//! to `Known(None)` (`blind_boundary_never_leaves_the_green_proof_read_permissive`'s
//! `Undetermined` assertion), `read_author` collapsing `Undetermined` to
//! `Known(None)` (`blind_boundary_never_leaves_the_separation_check_permissive`'s
//! message assertion: that mutant still
//! refuses, but with the "missing identity" text instead of "could not be read"),
//! and `oracle_report` mapping an Undetermined side to anything but exit 2
//! (`oracle_report_forwards_undetermined_as_exit_2`). Mutation evidence is recorded in the 8696dd7e slice 4 session.
//!
//! The src modules are pulled in via `#[path]` because tdd has no lib target.

#[path = "../src/config.rs"]
mod config;
#[path = "../src/proof.rs"]
mod proof;
#[path = "../src/runner.rs"]
mod runner;
#[path = "../src/transition.rs"]
mod transition;

use config::Config;
use harness_core::boundary::fault::{with_fault_plan, FaultPlan};
use harness_core::degrade::assert_fails_closed;
use harness_core::verdict::Determination;

#[test]
fn blind_boundary_never_leaves_the_green_proof_read_permissive() {
    let root = tempfile::tempdir().unwrap();
    let cfg = Config {
        proof_dir: ".tdd".to_string(),
        ..Config::default()
    };
    let green = proof::artifact_path(root.path(), &cfg, "t1", "green");
    std::fs::create_dir_all(green.parent().unwrap()).unwrap();
    std::fs::write(&green, br#"{"passed": true}"#).unwrap();

    // Permissive answer = the read CLAIMS TO KNOW (any `Known(_)`: both
    // `Known(Some(_))` and `Known(None)` = "no proof" are a settled answer
    // downstream). Only `Undetermined` is the restrictive answer.
    let claims_to_know = |root: &std::path::Path| {
        matches!(
            proof::read_passed(root, &cfg, "t1", "green"),
            Determination::Known(_)
        )
    };

    // Anti-vacuity control: with no fault the oracle read returns the recorded value.
    assert!(
        matches!(
            proof::read_passed(root.path(), &cfg, "t1", "green"),
            Determination::Known(Some(true))
        ),
        "control: an intact green proof must read as Known(Some(true))"
    );

    assert_fails_closed(|| claims_to_know(root.path()));

    // Stronger than "not permissive": a blinded read must be Undetermined, not
    // silently folded into Known(None) ("no proof"), which would be reported as
    // an ordinary invalid oracle instead of "could not look".
    let run = with_fault_plan(FaultPlan::blind(), || {
        proof::read_passed(root.path(), &cfg, "t1", "green")
    });
    assert!(run.injected > 0, "the read must reach the boundary");
    assert!(
        matches!(run.value, Determination::Undetermined(_)),
        "blind boundary must yield Undetermined, got a Known verdict"
    );
}

/// Forwarding arm: an Undetermined side must surface as exit 2 / "undetermined",
/// never as a valid oracle and never as the ordinary "unknown" (exit 1).
#[test]
fn oracle_report_forwards_undetermined_as_exit_2() {
    let und = || Determination::<Option<bool>>::undetermined("read blocked");
    for (pre, post) in [
        (und(), Determination::known(Some(true))),
        (Determination::known(Some(false)), und()),
        (und(), und()),
    ] {
        let (report, code) = transition::oracle_report(pre.clone(), post.clone());
        assert_eq!(report["valid_fp_oracle"], false);
        assert_eq!(report["transition"], "undetermined");
        assert_eq!(code, 2);
        // The undetermined side's has_* is null (neither observed), never a bool.
        if matches!(pre, Determination::Undetermined(_)) {
            assert!(
                report["has_red"].is_null(),
                "has_red must be null: {report}"
            );
        } else {
            assert!(report["has_red"].is_boolean());
        }
        if matches!(post, Determination::Undetermined(_)) {
            assert!(
                report["has_green"].is_null(),
                "has_green must be null: {report}"
            );
        } else {
            assert!(report["has_green"].is_boolean());
        }
    }
    // Control: fully known F->P is valid (exit 0), so the above is not a constant.
    let (report, code) = transition::oracle_report(
        Determination::known(Some(false)),
        Determination::known(Some(true)),
    );
    assert_eq!(report["valid_fp_oracle"], true);
    assert_eq!(code, 0);
}

/// Twin of `blind_boundary_never_leaves_the_green_proof_read_permissive` for
/// `proof::read_author`, which is private, so it is
/// driven through its only verdict-bearing consumer: `proof::green` under
/// `strict_separation`, where the RED author read feeds `judge_separation`.
/// `Ok(())` (green accepted) is the permissive answer. With the boundary blind
/// the RED author is unreadable, so green must be refused AND the refusal must
/// say the author "could not be read" -- not the `judge_separation` "identity is
/// missing" text, which is what a `read_author` that folds Undetermined into
/// `Known(None)` would produce (that is also an Err, so `is_ok()` alone cannot
/// tell the two apart).
#[test]
fn blind_boundary_never_leaves_the_separation_check_permissive() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let cfg = Config {
        proof_dir: ".tdd".to_string(),
        strict_separation: true,
        test_cmd: "true".to_string(),
        state_dir: state.path().to_path_buf(),
        ..Config::default()
    };
    let red = proof::artifact_path(root.path(), &cfg, "t1", "red");
    std::fs::create_dir_all(red.parent().unwrap()).unwrap();
    std::fs::write(&red, br#"{"passed": false, "author": "alice"}"#).unwrap();

    let green = |root: &std::path::Path| {
        proof::green(root, &cfg, "t1", &None, &Some("bob".to_string())).is_ok()
    };

    // Anti-vacuity control: distinct authors, no fault => green is accepted.
    assert!(
        green(root.path()),
        "control: alice(RED) vs bob(GREEN) must be accepted under strict_separation"
    );

    assert_fails_closed(|| green(root.path()));

    let run = with_fault_plan(FaultPlan::blind(), || {
        proof::green(root.path(), &cfg, "t1", &None, &Some("bob".to_string()))
    });
    assert!(run.injected > 0, "green must reach the boundary");
    let err = run
        .value
        .expect_err("blind boundary must refuse GREEN")
        .to_string();
    assert!(
        err.contains("author could not be read"),
        "must report the RED author as unreadable, got: {err}"
    );
    assert!(
        !err.contains("identity is missing"),
        "must not be the missing --author refusal, got: {err}"
    );
}
