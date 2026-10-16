#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `overwatch reconcile-fixed` outside a git repository (so `git log`
//! cannot be read) exits 0 with `commits_scanned: 0, reconciled: []` -- exactly
//! what "scanned recent commits and found nothing to reconcile" looks like.
//! "Could not read the history" is undetermined and must not exit 0.
//!
//! The existing test `reconcile_fixed.rs::reconcile_fixed_fail_soft_when_not_a_git_repo`
//! PINS the fail-open (it asserts `status.success()` and the empty result).
//! This test asserts the opposite of that pinned behaviour; when the defect is
//! fixed the pinning test must be flipped/removed in the same change.
use std::process::Command;

#[test]
#[ignore = "repro: reconcile-fixed exits 0 outside a git repo; remove ignore when fixed"]
fn reconcile_fixed_outside_git_repo_must_be_nonzero() {
    let t = std::env::temp_dir().join(format!("ow-repro-reconcile-norepo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let home = t.join("home");
    let work = t.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    // Control: this really is not a git repository.
    let g = Command::new("git")
        .current_dir(&work)
        .args(["rev-parse", "--git-dir"])
        .output()
        .unwrap();
    assert!(
        !g.status.success(),
        "control failed: sandbox is inside a git repo"
    );

    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(["reconcile-fixed", "--last-n", "10", "--json"])
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "reconcile-fixed exited 0 although git history could not be read \
         (undetermined, rendered as 'nothing to reconcile').\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
