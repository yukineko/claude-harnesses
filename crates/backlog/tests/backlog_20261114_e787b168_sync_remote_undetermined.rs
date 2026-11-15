#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog e787b168 (GH #1667): `backlog sync` reads each locally-open
//! mirrored row's remote state via `gh`. A gh failure, or output that is not
//! a recognisable issue state, is UNDETERMINED: reported explicitly with a
//! non-zero exit, never "0 to reconcile".
mod common;

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

fn tool_path(name: &str) -> PathBuf {
    for d in std::env::var("PATH").unwrap().split(':') {
        let p = PathBuf::from(d).join(name);
        if p.exists() {
            return p;
        }
    }
    panic!("no {name} on PATH");
}

/// Dry-run sync over one open row (#77), with a stub `gh` whose body is `gh_body`.
fn run_sync(tag: &str, gh_body: &str) -> (Option<i32>, String, String) {
    run_sync_with(tag, gh_body, "", &["sync"])
}

/// A second pending row with no issue yet: `sync --apply` plans a Create for it.
const CREATE_ROW: &str = r#"
[[task]]
id = "new00001"
title = "pending, never mirrored"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 3
updated_at = 3
weight = 0.0
"#;

/// Sync over the open row (#77) plus `extra_rows`, running `backlog <args>`.
fn run_sync_with(
    tag: &str,
    gh_body: &str,
    extra_rows: &str,
    args: &[&str],
) -> (Option<i32>, String, String) {
    let t = std::env::temp_dir().join(format!("bl-20261114-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, bin, repo) = (t.join("home"), t.join("bin"), t.join("repo"));
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(tool_path("git"), bin.join("git")).unwrap();
    std::fs::write(bin.join("gh"), format!("#!/bin/sh\n{gh_body}\n")).unwrap();
    std::fs::set_permissions(
        bin.join("gh"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();
    common::linked_checkout(&repo);
    assert!(harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["remote", "add", "origin", "https://github.com/o/r.git"])
        .current_dir(&repo)
        .env("HOME", &home)
        .status()
        .unwrap()
        .success());
    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    std::fs::write(
        repo.join(".backlog/tasks.toml"),
        r#"[[task]]
id = "open0077"
title = "pending, issue open locally"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 2
updated_at = 2
weight = 0.0
issue_number = 77
issue_url = "https://github.com/o/r/issues/77"
"#
        .to_string()
            + extra_rows,
    )
    .unwrap();
    std::fs::write(repo.join(".backlog/tasks.done.toml"), "").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .current_dir(&repo)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &bin)
        .output()
        .unwrap();
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn gh_nonzero_exit_is_undetermined_not_zero_to_reconcile() {
    let (code, out, err) = run_sync("fail", "echo 'HTTP 500' >&2; exit 1");
    assert_ne!(code, Some(0), "gh failure must exit non-zero: {out}{err}");
    assert!(
        err.contains("UNDETERMINED") && err.contains("open0077"),
        "must report the row as undetermined: {out}{err}"
    );
}

#[test]
fn gh_unparsable_output_is_undetermined() {
    let (code, out, err) = run_sync("junk", "echo 'not json at all'; exit 0");
    assert_ne!(
        code,
        Some(0),
        "unparsable output must exit non-zero: {out}{err}"
    );
    assert!(
        err.contains("UNDETERMINED") && err.contains("open0077"),
        "must report the row as undetermined: {out}{err}"
    );
}

#[test]
fn gh_unknown_state_is_undetermined() {
    let (code, out, err) = run_sync("weird", "echo '{\"state\":\"MERGED\"}'; exit 0");
    assert_ne!(
        code,
        Some(0),
        "unknown state must exit non-zero: {out}{err}"
    );
    assert!(err.contains("UNDETERMINED"), "{out}{err}");
}

#[test]
fn control_open_remote_is_clean_exit_zero() {
    let (code, out, err) = run_sync("open", "echo '{\"state\":\"OPEN\"}'; exit 0");
    assert_eq!(
        code,
        Some(0),
        "open remote is determined, exit 0: {out}{err}"
    );
    assert!(!err.contains("UNDETERMINED"), "{out}{err}");
    assert!(!out.contains("reopen"), "{out}{err}");
}

/// A gh that succeeds with valid JSON on stdout but also prints a warning on
/// stderr is a DETERMINED answer: the warning must not be parsed as part of the
/// JSON (which would invent a false UNDETERMINED).
#[test]
fn stderr_warning_with_valid_stdout_is_determined() {
    let (code, out, err) = run_sync(
        "warn",
        "echo '{\"state\":\"OPEN\"}'; echo 'warning: gh is outdated' >&2; exit 0",
    );
    assert_eq!(code, Some(0), "valid stdout is determined: {out}{err}");
    assert!(!err.contains("UNDETERMINED"), "{out}{err}");
}

/// `--apply` with an unreadable remote row AND a failing action: both must be
/// reported (the failed-actions list is not hidden behind the undetermined
/// report), and the exit is non-zero.
#[test]
fn apply_reports_undetermined_and_failed_actions_non_zero() {
    let (code, out, err) = run_sync_with(
        "apply",
        "echo 'HTTP 500' >&2; exit 1",
        CREATE_ROW,
        &["sync", "--apply"],
    );
    assert_ne!(code, Some(0), "--apply must exit non-zero: {out}{err}");
    assert!(
        err.contains("UNDETERMINED") && err.contains("open0077"),
        "--apply must report the undetermined row: {out}{err}"
    );
    assert!(
        err.contains("FAILED") && err.contains("new00001"),
        "--apply must still list the failed create: {out}{err}"
    );
    assert!(out.contains("0 confirmed"), "nothing succeeded: {out}{err}");
}
