#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `overwatch review-metrics --json` outside a git repository reports
//! `stale_undisposed_with_fix_commit: 0` at exit 0. The count is computed over
//! the git history, which could not be read; `reconcile.rs`
//! `stale_undisposed_count` folds the failed `git log` into "no commits". Zero
//! suppresses the stale-finding warning, so "could not look" renders as "no
//! stale finding". Twin of backlog 589a8bff (reconcile-fixed), fixed in the
//! same batch. (Its one runtime consumer, `scripts/record-audit.py`, already
//! treats a non-zero exit or a non-integer count as undetermined.)
//!
//! Correct behaviour asserted: the count is `null` or the command exits
//! non-zero.
use serde_json::Value;
use std::process::Command;

#[test]
fn review_metrics_stale_count_outside_git_repo_is_not_zero() {
    let t = std::env::temp_dir().join(format!("ow-repro-rm-norepo-{}", std::process::id()));
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
        .args(["review-metrics", "--json"])
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .unwrap();
    let so = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(&so).expect("review-metrics --json parseable");
    assert!(
        !out.status.success() || v["stale_undisposed_with_fix_commit"].is_null(),
        "review-metrics reported a stale count of {} at exit 0 although the git \
         history could not be read.\nstdout={so}\nstderr={}",
        v["stale_undisposed_with_fix_commit"],
        String::from_utf8_lossy(&out.stderr)
    );
}
