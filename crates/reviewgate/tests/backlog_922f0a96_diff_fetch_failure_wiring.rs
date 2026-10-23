#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED wiring test for reviewgate verdict-path audit items P1/P2.
//!
//! `git::diff_text()`'s three content-fetch sub-commands (`diff --`, `diff
//! --cached --`, `ls-files --others --exclude-standard --`) used to swallow a
//! spawn error / non-zero exit with no `else` branch, so a git failure AFTER
//! `changed_files()` already confirmed real changes produced an empty/partial
//! `DiffText` with no failure signal. `evaluate()` read an empty diff as
//! "empty-diff" (silent allow, P1) and a non-empty-but-partial diff as a
//! certifiable, hashable "already-reviewed" diff (P2) — either way, a real,
//! unreviewed change slipped through with zero diagnostic.
//!
//! After the fix, `DiffText.fetch_failed` must be set and `evaluate()` must
//! route through `decide_scan_failed` — same blocking treatment as a
//! `changed_files()` scan failure (bounded, escapable, loud).
//!
//! The binary is driven in a CHILD process with a shimmed `PATH` scoped to
//! that child only — mirrors `git_probe_wiring.rs`'s existing convention, no
//! process-global env mutation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "reviewgate-fetchfail-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

fn real_git_path() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate real git");
    assert!(out.status.success(), "no real git on PATH; cannot shim it");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Writes an executable fake `git` at `dir/git` that fails (exit 7) any
/// invocation matched by `fail_script` (a POSIX shell condition body with
/// access to `$@`), and otherwise execs the real git.
fn write_fake_git(dir: &Path, real_git: &str, fail_script: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let script = format!("#!/bin/sh\n{fail_script}\nexec {real_git} \"$@\"\n");
    let path = dir.join("git");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn run_review(cwd: &Path, home: &Path, path: &Path, payload: &str) -> (i32, String, String) {
    ensure_inject_config(home);
    let bin = env!("CARGO_BIN_EXE_reviewgate");
    let mut child = Command::new(bin)
        .arg("review")
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stop_payload(cwd: &Path) -> String {
    format!(
        r#"{{"hook_event_name":"Stop","session_id":"probe-fetchfail","stop_hook_active":false,"cwd":{}}}"#,
        serde_json::to_string(&cwd.to_string_lossy()).unwrap()
    )
}

fn setup_repo_with_a_tracked_edit(work: &Path) {
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t.com"][..],
        &["config", "user.name", "t"][..],
    ] {
        assert!(Command::new("git")
            .current_dir(work)
            .args(args)
            .status()
            .expect("git")
            .success());
    }
    std::fs::write(work.join("a.rs"), "fn a() {}\n").unwrap();
    assert!(Command::new("git")
        .current_dir(work)
        .args(["add", "a.rs"])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .current_dir(work)
        .args(["commit", "-qm", "init"])
        .status()
        .unwrap()
        .success());
    std::fs::write(work.join("a.rs"), "fn a() -> u8 { 1 }\n").unwrap();
}

/// THE fail-open (P1): `changed_files()` succeeds (real change detected), but
/// every content-fetch command inside `diff_text()` fails — the real,
/// unreviewed change must BLOCK, not silently allow as "empty-diff".
#[test]
fn content_fetch_failure_after_a_confirmed_change_blocks_not_allows() {
    let root = unique_dir("all-fetches-fail");
    let real_git = real_git_path();
    let shim = root.join("shim-bin");
    // Fail any invocation carrying a literal "--" arg: diff_text's three
    // content-fetch commands all end that way; changed_files()'s scan
    // commands (`diff --name-only` etc.) do not, so the scan itself succeeds.
    write_fake_git(
        &shim,
        &real_git,
        "for a in \"$@\"; do if [ \"$a\" = \"--\" ]; then exit 7; fi; done",
    );
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    setup_repo_with_a_tracked_edit(&work);

    let (code, stdout, stderr) = run_review(&work, &home, &shim, &stop_payload(&work));

    assert_eq!(
        code, 0,
        "the Stop hook must always exit 0; stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\""),
        "changed_files() confirmed a real change, but every diff_text() content-fetch failed. \
         The diff is UNDETERMINED (not \"no changes\") — allowing here is the P1 fail-open: a \
         real edit slips through unreviewed with zero diagnostic. stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("変更内容を特定できませんでした"),
        "must be the git-scan-failed block, not the inject-mode review block. stdout: {stdout:?}"
    );
    assert!(
        stderr.contains("diff content fetch failed"),
        "the block must come from diff_text()'s fetch_failed path, not from a changed_files() \
         scan failure (both produce the same git-scan-failed block text). stderr: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// THE fail-open (P2): only the untracked-file scan fails; the tracked diff
/// still comes through non-empty. The diff must still be flagged undetermined
/// (not silently hashed and certified as "already-reviewed") because it is
/// missing real content.
#[test]
fn partial_content_fetch_failure_blocks_not_certifies() {
    let root = unique_dir("partial-fetch-fails");
    let real_git = real_git_path();
    let shim = root.join("shim-bin");
    // Fail ONLY diff_text()'s content-fetch form of ls-files
    // (`ls-files --others --exclude-standard -- <files>`, the one carrying the
    // literal `--` separator). changed_files()'s scan form
    // (`ls-files --others --exclude-standard`, no `--`) must SUCCEED: if it
    // failed too, the git-scan-failed block below would come from the scan
    // path and this test would not exercise the fetch_failed wiring at all.
    // diff / diff --cached also succeed, so the fetched text is non-empty.
    write_fake_git(
        &shim,
        &real_git,
        "if [ \"$1\" = \"ls-files\" ]; then for a in \"$@\"; do \
         if [ \"$a\" = \"--\" ]; then exit 7; fi; done; fi",
    );
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    setup_repo_with_a_tracked_edit(&work);
    std::fs::write(work.join("untracked.rs"), "NEVER REVIEWED MARKER\n").unwrap();

    let (code, stdout, stderr) = run_review(&work, &home, &shim, &stop_payload(&work));

    assert_eq!(
        code, 0,
        "the Stop hook must always exit 0; stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\""),
        "the untracked-file scan failed, so the diff is missing real content (untracked.rs) even \
         though the tracked diff is non-empty. Certifying this partial diff as complete/reviewed \
         is the P2 fail-open. stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("変更内容を特定できませんでした"),
        "must be the git-scan-failed block, not the inject-mode review block. stdout: {stdout:?}"
    );
    assert!(
        stderr.contains("diff content fetch failed"),
        "the block must come from diff_text()'s fetch_failed path, not from a changed_files() \
         scan failure (both produce the same git-scan-failed block text). stderr: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Control: real git, nothing shimmed — a real change must still block for
/// review (unrelated to fetch failure; pins the happy path is not regressed).
#[test]
fn control_real_git_confirmed_change_blocks_for_review() {
    // No PATH means the control cannot run real git; that is a FAILURE to
    // check, not a pass — never report green without having checked.
    let path = std::env::var_os("PATH")
        .expect("PATH is unset: the control cannot locate real git, so it cannot check anything");
    let root = unique_dir("control");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    setup_repo_with_a_tracked_edit(&work);

    let (code, stdout, stderr) = run_review(&work, &home, Path::new(&path), &stop_payload(&work));

    assert_eq!(
        code, 0,
        "the Stop hook must always exit 0; stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\""),
        "a real, un-reviewed tracked edit must block for review (inject mode default). stdout: \
         {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The unreadable-untracked-file arm of `diff_text()`: real git (the scan and
/// every git content fetch succeed), but an untracked file among the changes
/// cannot be read. Its content is missing from the diff, so the diff is
/// UNDETERMINED and must block through `fetch_failed`, not be certified.
#[cfg(unix)]
#[test]
fn unreadable_untracked_file_blocks_via_fetch_failed() {
    use std::os::unix::fs::PermissionsExt;
    // Root reads mode-000 files, so the read failure cannot be produced. That
    // is a failure to check, not a pass: fail loudly.
    let uid = Command::new("id").arg("-u").output().expect("run id -u");
    assert!(
        uid.status.success(),
        "id -u failed: cannot tell whether running as root"
    );
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    assert_ne!(
        uid, "0",
        "running as root: a mode-000 file is still readable, so this test cannot produce the \
         read failure it checks. Run it as a non-root user."
    );
    let path = std::env::var_os("PATH")
        .expect("PATH is unset: cannot locate real git, so this test cannot check anything");

    let root = unique_dir("unreadable-untracked");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    setup_repo_with_a_tracked_edit(&work);
    let secret = work.join("unreadable.rs");
    std::fs::write(&secret, "NEVER REVIEWED MARKER\n").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();

    let (code, stdout, stderr) = run_review(&work, &home, Path::new(&path), &stop_payload(&work));

    // Restore before asserting so cleanup works whatever the outcome.
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(
        code, 0,
        "the Stop hook must always exit 0; stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\""),
        "an untracked file among the changes could not be read, so its content is missing from \
         the diff. Certifying that diff is the fail-open. stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("変更内容を特定できませんでした"),
        "must be the git-scan-failed block, not the inject-mode review block. stdout: {stdout:?}"
    );
    assert!(
        stderr.contains("diff content fetch failed") && stderr.contains("read untracked"),
        "the block must come from diff_text()'s unreadable-untracked-file arm via fetch_failed. \
         stderr: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// reviewgate's built-in default is `mode = "off"` (no review). These tests
/// exercise the reviewing path, so they opt in explicitly with a home config
/// `mode = "inject"` -- written only when the test has not placed its own.
fn ensure_inject_config(home: &Path) {
    let cfg = home.join(".reviewgate").join("config.toml");
    if !cfg.exists() {
        std::fs::create_dir_all(home.join(".reviewgate")).unwrap();
        std::fs::write(cfg, "mode = \"inject\"\n").unwrap();
    }
}
