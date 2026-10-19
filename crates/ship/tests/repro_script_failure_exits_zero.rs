#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: failed or missing scripts leave `ship` exiting 0.
//!
//! `checklist::run_safe` / `rollout` return `Ok(summary)` even when the script
//! exits non-zero (the summary says FAILED), and `main.rs` only `print!`s it;
//! on `Err` (script missing) it `eprintln!`s. Neither path sets an exit code,
//! so a caller or script reading `$?` sees success.
//!
//! Asserts the correct behaviour: a failed/missing script yields non-zero exit.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn git_init(p: &std::path::Path) {
    let o = Command::new("git")
        .args(["init", "-q"])
        .current_dir(p)
        .output()
        .unwrap();
    assert!(o.status.success());
}

fn repo_with_script(name: &str, body: &str) -> tempfile::TempDir {
    let repo = tempfile::TempDir::new().unwrap();
    let p = repo.path();
    git_init(p);
    fs::create_dir_all(p.join("scripts")).unwrap();
    let s = p.join("scripts").join(name);
    fs::write(&s, body).unwrap();
    fs::set_permissions(&s, fs::Permissions::from_mode(0o755)).unwrap();
    repo
}

fn run(args: &[&str], dir: &std::path::Path) -> (Option<i32>, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_ship"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

#[test]
fn run_safe_script_failure_exits_nonzero() {
    let repo = repo_with_script("rebuild-plugins.sh", "#!/bin/sh\necho boom >&2\nexit 1\n");
    let (c, out) = run(&["check", "--run-safe"], repo.path());
    assert!(
        out.contains("FAILED"),
        "setup: failure not surfaced:\n{out}"
    );
    assert_ne!(
        c,
        Some(0),
        "rebuild script failed but ship exited 0:\n{out}"
    );
}

#[test]
fn rollout_script_failure_exits_nonzero() {
    let repo = repo_with_script("rollout-plugins.sh", "#!/bin/sh\necho boom >&2\nexit 1\n");
    let (c, out) = run(&["rollout"], repo.path());
    assert!(
        out.contains("FAILED"),
        "setup: failure not surfaced:\n{out}"
    );
    assert_ne!(
        c,
        Some(0),
        "rollout script failed but ship exited 0:\n{out}"
    );
}

#[test]
fn rollout_script_missing_exits_nonzero() {
    let repo = tempfile::TempDir::new().unwrap();
    git_init(repo.path());
    let (c, _) = run(&["rollout"], repo.path());
    assert_ne!(c, Some(0), "rollout script missing but ship exited 0");
}
