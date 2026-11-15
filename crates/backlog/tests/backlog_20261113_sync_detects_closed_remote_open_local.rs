#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `backlog sync` never looks at GitHub's actual issue state. Its plan is a
//! function of the local store alone (`status`, `issue_number`,
//! `issue_closed_at`), so a row that is OPEN locally (pending, no
//! `issue_closed_at`) whose issue was closed ON GITHUB (by hand, by another
//! session's `gh issue close`) is invisible: the dry run prints
//! "sync plan: 0 issue(s) to create, 0 issue(s) to close" while the two sides
//! disagree.
//!
//! Observed 2026-11-13 on this repo (main 5d5eeaab): 33 open rows in
//! `.backlog/tasks.toml`, 17 open GitHub issues, 16 of those rows' issues
//! closed COMPLETED on GitHub (e.g. #1551 for 433ca3be), and the dry run
//! planned nothing.
//!
//! Drives the real binary's dry-run `sync` against a hand-written store and a
//! stub `gh` that reports EVERY issue it is asked about as CLOSED (whatever
//! query shape the fix uses: `issue view`, `issue list`, `api`). The control
//! proves the reopen arm is printed for this store shape when the closure is
//! recorded locally, so the only missing piece is reading the remote state.
//!
//! Repro: cargo test -p backlog --test backlog_20261113_sync_detects_closed_remote_open_local -- --include-ignored
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

/// Dry-run `backlog sync` over a store holding one pending row whose issue is
/// recorded closed locally (control) and one pending row whose issue is open
/// locally but closed on the (stubbed) remote.
fn dry_run_sync(tag: &str) -> String {
    let t = std::env::temp_dir().join(format!("bl-20261113-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, bin, repo) = (t.join("home"), t.join("bin"), t.join("repo"));
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(tool_path("git"), bin.join("git")).unwrap();
    // Stub gh: every issue is CLOSED on the remote. Answers the JSON shapes
    // of `gh issue view --json`, `gh issue list --json` and `gh api`.
    let stub = "#!/bin/sh\n\
        case \"$*\" in\n\
        *list*) echo '[{\"number\":1551,\"state\":\"CLOSED\",\"stateReason\":\"COMPLETED\"},{\"number\":420,\"state\":\"CLOSED\",\"stateReason\":\"COMPLETED\"}]' ;;\n\
        *api*) echo '[{\"number\":1551,\"state\":\"closed\",\"state_reason\":\"completed\"},{\"number\":420,\"state\":\"closed\",\"state_reason\":\"completed\"}]' ;;\n\
        *) echo '{\"number\":1551,\"state\":\"CLOSED\",\"stateReason\":\"COMPLETED\"}' ;;\n\
        esac\nexit 0\n";
    std::fs::write(bin.join("gh"), stub).unwrap();
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
id = "ctrl0420"
title = "pending, issue recorded closed locally (control)"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 1
updated_at = 1
weight = 0.0
issue_number = 420
issue_url = "https://github.com/o/r/issues/420"
issue_closed_at = 1791024507

[[task]]
id = "pend1551"
title = "pending, issue open locally but closed on GitHub"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 2
updated_at = 2
weight = 0.0
issue_number = 1551
issue_url = "https://github.com/o/r/issues/1551"
"#,
    )
    .unwrap();
    std::fs::write(repo.join(".backlog/tasks.done.toml"), "").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .arg("sync")
        .current_dir(&repo)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &bin)
        .output()
        .unwrap();
    let out = format!(
        "code={:?}\nstdout={}\nstderr={}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(o.status.code(), Some(0), "dry-run sync must run: {out}");
    out
}

#[test]
fn control_locally_recorded_closure_is_planned_for_reopen() {
    let out = dry_run_sync("ctrl");
    assert!(
        out.contains("ctrl0420") && out.contains("#420"),
        "control: the recorded-closed row must be planned for reopen: {out}"
    );
}

#[test]
fn open_local_closed_remote_row_is_planned_for_reopen() {
    let out = dry_run_sync("remote");
    assert!(
        out.contains("pend1551") && out.contains("#1551"),
        "pend1551 is pending locally but its issue #1551 is CLOSED on GitHub; \
         the dry run must plan to reopen it instead of reporting nothing: {out}"
    );
}
