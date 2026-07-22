// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration test for `overwatch reconcile-fixed`.
//!
//! Seeds a finding via the REAL CLI (`record-finding`), builds a real git
//! repo in the sandboxed work dir with commits referencing (and not
//! referencing) that finding-id, and asserts `reconcile-fixed` auto-records
//! a CONFIRMED disposition for the referenced finding while leaving
//! non-matching commits inert. Also covers idempotency (re-running never
//! duplicates a disposition), `--dry-run` (no write), and fail-soft
//! behavior when cwd is not a git repository.
//!
//! Sandboxed via a temp HOME + temp cwd (same pattern as
//! `disposition_metrics.rs` / `audit_round.rs`), with a REAL nested git repo
//! under the temp work dir (never the harness repo itself).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_sandbox(tag: &str) -> (PathBuf, PathBuf) {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let base = std::env::temp_dir().join(format!(
        "overwatch-reconcile-test-{tag}-{}-{n}",
        std::process::id()
    ));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (home, work)
}

fn overwatch_bin() -> &'static str {
    env!("CARGO_BIN_EXE_overwatch")
}

fn run_ow(home: &Path, work: &Path, args: &[&str]) -> String {
    let out = Command::new(overwatch_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        out.status.success(),
        "overwatch {:?} exited non-zero: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("overwatch stdout not utf8")
}

fn git(work: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .current_dir(work)
        .output()
        .expect("failed to spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Init a fresh git repo under `work` and record one commit per message in
/// `messages` (in order), each touching a distinct file so every commit is
/// non-empty.
fn init_repo_with_commits(work: &Path, messages: &[&str]) {
    git(work, &["init", "-q"]);
    git(work, &["config", "commit.gpgsign", "false"]);
    for (i, msg) in messages.iter().enumerate() {
        let file = work.join(format!("f{i}.txt"));
        std::fs::write(&file, format!("{i}")).unwrap();
        git(work, &["add", "."]);
        git(work, &["commit", "-q", "-m", msg]);
    }
}

#[test]
fn reconcile_fixed_confirms_finding_referenced_by_fix_commit() {
    let (home, work) = make_sandbox("core");

    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "CA-overwatch-777",
            "--source",
            "reviewgate",
            "--summary",
            "leak in reconcile path",
        ],
    );

    init_repo_with_commits(
        &work,
        &[
            "chore: unrelated setup commit",
            "fix(overwatch): resolve CA-overwatch-777 leak",
        ],
    );

    let out = run_ow(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    let v: Value = serde_json::from_str(&out).expect("reconcile-fixed --json must parse");
    assert_eq!(v["commits_scanned"], 2);
    assert_eq!(v["reconciled"], serde_json::json!(["CA-overwatch-777"]));

    let metrics_out = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m: Value = serde_json::from_str(&metrics_out).unwrap();
    assert_eq!(m["total"], 1);
    assert_eq!(m["by_verdict"]["confirmed"], 1);
}

#[test]
fn reconcile_fixed_ignores_non_matching_commits() {
    let (home, work) = make_sandbox("no-match");

    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "CA-overwatch-888",
            "--source",
            "reviewgate",
            "--summary",
            "unrelated finding",
        ],
    );

    init_repo_with_commits(&work, &["chore: totally unrelated change"]);

    let out = run_ow(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["reconciled"], serde_json::json!([]));

    let metrics_out = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m: Value = serde_json::from_str(&metrics_out).unwrap();
    assert_eq!(m["total"], 0);
}

#[test]
fn reconcile_fixed_is_idempotent_across_repeated_runs() {
    let (home, work) = make_sandbox("idempotent");

    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "CA-overwatch-999",
            "--source",
            "reviewgate",
            "--summary",
            "double-processed finding",
        ],
    );
    init_repo_with_commits(&work, &["fix: resolve CA-overwatch-999"]);

    run_ow(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    // Run a second time: the same commit still references the finding-id,
    // which is now already dispositioned — must NOT record a duplicate.
    let out2 = run_ow(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    let v2: Value = serde_json::from_str(&out2).unwrap();
    assert_eq!(
        v2["reconciled"],
        serde_json::json!([]),
        "already-disposed finding must not be re-reconciled"
    );

    let metrics_out = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m: Value = serde_json::from_str(&metrics_out).unwrap();
    assert_eq!(m["total"], 1, "no duplicate disposition should be recorded");
}

#[test]
fn reconcile_fixed_dry_run_does_not_write_disposition() {
    let (home, work) = make_sandbox("dry-run");

    run_ow(
        &home,
        &work,
        &[
            "record-finding",
            "--finding-id",
            "CA-overwatch-321",
            "--source",
            "reviewgate",
            "--summary",
            "dry run candidate",
        ],
    );
    init_repo_with_commits(&work, &["fix: resolve CA-overwatch-321"]);

    let out = run_ow(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--dry-run", "--json"],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["reconciled"], serde_json::json!(["CA-overwatch-321"]));
    assert_eq!(v["dry_run"], true);

    let metrics_out = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m: Value = serde_json::from_str(&metrics_out).unwrap();
    assert_eq!(m["total"], 0, "--dry-run must not write a disposition");
    // Early-warning signal: a fix commit for CA-overwatch-321 has landed but
    // --dry-run deliberately left it undisposed — review-metrics must
    // surface that gap rather than silently agreeing with the stale queue.
    assert_eq!(
        m["stale_undisposed_with_fix_commit"], 1,
        "dry-run must not clear the stale-undisposed warning: {m}"
    );

    // A real (non-dry-run) reconcile-fixed clears the warning.
    run_ow(
        &home,
        &work,
        &["reconcile-fixed", "--last-n", "10", "--json"],
    );
    let metrics_out2 = run_ow(&home, &work, &["review-metrics", "--json"]);
    let m2: Value = serde_json::from_str(&metrics_out2).unwrap();
    assert_eq!(
        m2["stale_undisposed_with_fix_commit"], 0,
        "reconcile-fixed must clear the stale-undisposed warning: {m2}"
    );
}

#[test]
fn reconcile_fixed_fail_soft_when_not_a_git_repo() {
    // `work` is a plain directory, never `git init`-ed: `git log` must fail,
    // and reconcile-fixed must degrade to "0 processed" rather than
    // panicking or exiting non-zero (the fail-soft contract pre-push/CI
    // rely on).
    let (home, work) = make_sandbox("no-repo");

    let out = Command::new(overwatch_bin())
        .args(["reconcile-fixed", "--last-n", "10", "--json"])
        .env("HOME", &home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&work)
        .output()
        .expect("failed to spawn overwatch binary");
    assert!(
        out.status.success(),
        "reconcile-fixed must exit 0 even when cwd is not a git repo (fail-soft)"
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["commits_scanned"], 0);
    assert_eq!(v["reconciled"], serde_json::json!([]));
}
