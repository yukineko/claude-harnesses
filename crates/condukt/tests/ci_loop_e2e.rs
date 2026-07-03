//! End-to-end coverage for the CI feedback loop, exercised through the REAL
//! built `condukt` binary (`condukt pr poll`) against a STUBBED `gh` binary
//! (never touches live GitHub).
//!
//! `condukt pr poll --pr <n> --branch <b> [--execute]` fetches CI status via
//! `gh pr checks` (through [`gh_probe`] in `main.rs`), maps it through the
//! pure state machine in `ci.rs` (`parse_ci_checks` → `decide_ci_action`), and
//! merges the branch into the default branch ONLY on a green verdict AND
//! `--execute`. This test drives that whole pipeline end-to-end with a stub
//! `gh` script prepended to `PATH`, asserting the full CI feedback loop:
//!
//!   pending → Wait (no merge)
//!   failure → Reenter (no merge)
//!   success, no --execute → Merge verdict but dry-run (no merge)
//!   success, --execute → Merge verdict AND an actual git merge happens
//!
//! and finally a single scenario where the SAME PR/branch is polled three
//! times in a row with the stub progressing pending → failure → success,
//! proving the merge side effect (the observable proxy for "on_merge fires")
//! is absent for the first two polls and present only once CI goes green —
//! i.e. "never merges on a non-green path; always merges on green".
//!
//! `condukt` has no library target (bin-only crate), so `ci::fetch_and_decide`
//! / `ci::poll_and_maybe_merge` are not directly callable from this test
//! binary. Instead this drives the real `condukt pr poll` CLI end-to-end,
//! mirroring the `env!("CARGO_BIN_EXE_condukt")` + stubbed-PATH pattern used
//! by `tests/fp_oracle_e2e.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const PENDING_JSON: &str = r#"[{"name":"build","status":"in_progress","conclusion":null}]"#;
const FAILURE_JSON: &str = r#"[{"name":"build","status":"completed","conclusion":"failure"}]"#;
const SUCCESS_JSON: &str = r#"[{"name":"build","status":"completed","conclusion":"success"}]"#;

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "condukt-ci-loop-e2e-{tag}-{}-{}",
        std::process::id(),
        id
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated dir");
    dir
}

/// Run a git command in `dir`, panicking with full context on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Initialize a bare-minimum git repo with an initial commit on `main` (the
/// `Config::load` default `default_branch`, so this stays unconfigured/plain).
fn init_repo() -> PathBuf {
    let repo = unique_dir("repo");
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("base.txt"), "base\n").expect("write base.txt");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

/// Create `branch` off `main` with one commit adding `file`, then check back
/// out `main` — mirroring `worktree.rs`'s own `make_branch` test helper.
fn make_branch(repo: &Path, branch: &str, file: &str, content: &str) {
    git(repo, &["checkout", "-b", branch]);
    std::fs::write(repo.join(file), content).expect("write branch file");
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", &format!("add {file} on {branch}")]);
    git(repo, &["checkout", "main"]);
}

/// Write a stub `gh` script into a fresh directory and return that directory
/// (to be prepended to `PATH`). The script ignores its argv (the only `gh`
/// call in the `pr poll` path is `gh pr checks <pr> --json ...`) and always
/// prints `json` to stdout, exiting with `exit_code` — deterministic,
/// no network, no live GitHub.
fn write_stub_gh(json: &str, exit_code: i32) -> PathBuf {
    let dir = unique_dir("ghstub");
    let script = format!(
        "#!/bin/sh\ncat <<'CONDUKT_STUB_GH_EOF'\n{json}\nCONDUKT_STUB_GH_EOF\nexit {exit_code}\n"
    );
    let path = dir.join("gh");
    std::fs::write(&path, script).expect("write stub gh script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .expect("stat stub gh")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod stub gh");
    }
    dir
}

/// Overwrite an existing stub `gh` script (same directory) with a new reply —
/// used by the sequential-poll scenario to advance pending → failure →
/// success across repeated `condukt pr poll` invocations without needing a
/// fresh `PATH` entry each time.
fn rewrite_stub_gh(gh_dir: &Path, json: &str, exit_code: i32) {
    let script = format!(
        "#!/bin/sh\ncat <<'CONDUKT_STUB_GH_EOF'\n{json}\nCONDUKT_STUB_GH_EOF\nexit {exit_code}\n"
    );
    let path = gh_dir.join("gh");
    std::fs::write(&path, script).expect("rewrite stub gh script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .expect("stat stub gh")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod stub gh");
    }
}

/// Run the real `condukt` binary with `args`, in `dir`, with `home` as its
/// `$HOME` (isolating `~/.condukt/config.toml` so `default_branch` stays the
/// "main" default) and `gh_dir` prepended to `$PATH` so `gh pr checks`
/// resolves to our stub instead of a real (or absent) `gh`. Returns
/// `(exit_code, stdout, stderr)`.
fn run_condukt(dir: &Path, home: &Path, gh_dir: &Path, args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_condukt");
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![gh_dir.to_path_buf()];
    paths.extend(std::env::split_paths(&existing_path));
    let new_path = std::env::join_paths(paths).expect("join PATH");

    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("PATH", new_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("condukt spawns");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Pending CI ⇒ `Wait`: `condukt pr poll` reports "waiting (no merge)" and the
/// feature branch is NOT merged into `main` (the branch-only file is absent).
#[test]
fn pending_ci_waits_and_does_not_merge() {
    let repo = init_repo();
    let home = unique_dir("home-pending");
    make_branch(&repo, "feat/pending", "feat.txt", "feature content\n");
    let gh_dir = write_stub_gh(PENDING_JSON, 0);

    let (code, stdout, stderr) = run_condukt(
        &repo,
        &home,
        &gh_dir,
        &[
            "pr",
            "poll",
            "--pr",
            "1",
            "--branch",
            "feat/pending",
            "--execute",
        ],
    );

    assert_eq!(code, 0, "poll must exit 0 (fail-soft)\nstderr: {stderr}");
    assert!(
        stdout.contains("waiting (no merge)"),
        "expected a Wait verdict message, got stdout: {stdout}"
    );
    assert!(
        !stdout.to_lowercase().contains("merged"),
        "pending CI must never mention a merge, got stdout: {stdout}"
    );
    assert!(
        !repo.join("feat.txt").exists(),
        "pending CI must NOT merge the branch"
    );
    assert_eq!(
        git(&repo, &["branch", "--show-current"]),
        "main",
        "repo must remain on the default branch when no merge happens"
    );
}

/// Failing CI ⇒ `Reenter`: `condukt pr poll` reports "re-enter the worker" and
/// the feature branch is NOT merged.
#[test]
fn failing_ci_reenters_and_does_not_merge() {
    let repo = init_repo();
    let home = unique_dir("home-failure");
    make_branch(&repo, "feat/failure", "feat.txt", "feature content\n");
    let gh_dir = write_stub_gh(FAILURE_JSON, 1);

    let (code, stdout, stderr) = run_condukt(
        &repo,
        &home,
        &gh_dir,
        &[
            "pr",
            "poll",
            "--pr",
            "1",
            "--branch",
            "feat/failure",
            "--execute",
        ],
    );

    assert_eq!(code, 0, "poll must exit 0 (fail-soft)\nstderr: {stderr}");
    assert!(
        stdout.contains("re-enter the worker to fix it (no merge)"),
        "expected a Reenter verdict message, got stdout: {stdout}"
    );
    assert!(
        !stdout.to_lowercase().contains("merged"),
        "failing CI must never mention a merge, got stdout: {stdout}"
    );
    assert!(
        !repo.join("feat.txt").exists(),
        "failing CI must NOT merge the branch"
    );
}

/// Green CI without `--execute` ⇒ `Merge` verdict but a dry-run: reported as
/// "would merge" and nothing is actually merged (the GATED gate is withheld).
#[test]
fn green_ci_without_execute_is_a_dry_run() {
    let repo = init_repo();
    let home = unique_dir("home-dryrun");
    make_branch(&repo, "feat/dryrun", "feat.txt", "feature content\n");
    let gh_dir = write_stub_gh(SUCCESS_JSON, 0);

    let (code, stdout, stderr) = run_condukt(
        &repo,
        &home,
        &gh_dir,
        &["pr", "poll", "--pr", "1", "--branch", "feat/dryrun"],
    );

    assert_eq!(code, 0, "poll must exit 0\nstderr: {stderr}");
    assert!(
        stdout.contains("would merge") && stdout.contains("dry-run"),
        "expected a dry-run Merge message, got stdout: {stdout}"
    );
    assert!(
        !repo.join("feat.txt").exists(),
        "a dry-run (no --execute) must NOT actually merge"
    );
}

/// Green CI WITH `--execute` ⇒ `Merge` verdict AND the real merge happens:
/// the branch's file lands on the default branch. This is the executable
/// proof that the merge side effect ("on_merge") fires on — and only on —
/// the green path.
#[test]
fn green_ci_with_execute_actually_merges() {
    let repo = init_repo();
    let home = unique_dir("home-merge");
    make_branch(&repo, "feat/merge", "feat.txt", "feature content\n");
    let gh_dir = write_stub_gh(SUCCESS_JSON, 0);

    let (code, stdout, stderr) = run_condukt(
        &repo,
        &home,
        &gh_dir,
        &[
            "pr",
            "poll",
            "--pr",
            "1",
            "--branch",
            "feat/merge",
            "--execute",
        ],
    );

    assert_eq!(code, 0, "poll must exit 0\nstderr: {stderr}");
    assert!(
        stdout.contains("CI green: merged 'feat/merge' into 'main'"),
        "expected the real-merge success message, got stdout: {stdout}"
    );
    assert!(
        repo.join("feat.txt").exists(),
        "green CI with --execute must actually merge the branch"
    );
    assert_eq!(
        git(&repo, &["branch", "--show-current"]),
        "main",
        "merge lands on the default branch"
    );
}

/// The full CI feedback loop, exercised as a single scenario against the
/// SAME PR/branch: the stub `gh` progresses pending → failure → success
/// across three successive `condukt pr poll --execute` invocations. The
/// branch's file must be absent after the first two polls (no merge on a
/// non-green verdict) and present only after the third, green, poll — the
/// executable proof that a worker "re-enter" (failure) never sneaks a merge
/// through, and the merge only ever fires once CI is actually green.
#[test]
fn ci_loop_pending_then_failure_then_success_merges_only_at_the_end() {
    let repo = init_repo();
    let home = unique_dir("home-loop");
    make_branch(&repo, "feat/loop", "feat.txt", "feature content\n");
    let gh_dir = write_stub_gh(PENDING_JSON, 0);

    // 1. pending → Wait, no merge.
    let (code, stdout, stderr) = run_condukt(
        &repo,
        &home,
        &gh_dir,
        &[
            "pr",
            "poll",
            "--pr",
            "1",
            "--branch",
            "feat/loop",
            "--execute",
        ],
    );
    assert_eq!(code, 0, "poll 1 (pending) must exit 0\nstderr: {stderr}");
    assert!(
        stdout.contains("waiting (no merge)"),
        "poll 1 expected Wait, got: {stdout}"
    );
    assert!(
        !repo.join("feat.txt").exists(),
        "poll 1 (pending) must not merge"
    );

    // 2. CI fails (worker would re-enter to fix it) → Reenter, still no merge.
    rewrite_stub_gh(&gh_dir, FAILURE_JSON, 1);
    let (code, stdout, stderr) = run_condukt(
        &repo,
        &home,
        &gh_dir,
        &[
            "pr",
            "poll",
            "--pr",
            "1",
            "--branch",
            "feat/loop",
            "--execute",
        ],
    );
    assert_eq!(code, 0, "poll 2 (failure) must exit 0\nstderr: {stderr}");
    assert!(
        stdout.contains("re-enter the worker to fix it (no merge)"),
        "poll 2 expected Reenter, got: {stdout}"
    );
    assert!(
        !repo.join("feat.txt").exists(),
        "poll 2 (failure) must not merge"
    );

    // 3. Fix lands, CI goes green → Merge, and NOW the branch is actually
    //    merged — the only point in the loop where `feat.txt` appears.
    rewrite_stub_gh(&gh_dir, SUCCESS_JSON, 0);
    let (code, stdout, stderr) = run_condukt(
        &repo,
        &home,
        &gh_dir,
        &[
            "pr",
            "poll",
            "--pr",
            "1",
            "--branch",
            "feat/loop",
            "--execute",
        ],
    );
    assert_eq!(code, 0, "poll 3 (success) must exit 0\nstderr: {stderr}");
    assert!(
        stdout.contains("CI green: merged 'feat/loop' into 'main'"),
        "poll 3 expected an actual merge, got: {stdout}"
    );
    assert!(
        repo.join("feat.txt").exists(),
        "poll 3 (success) must merge the branch — the loop's only merge point"
    );
}
