#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 7e575392: `backlog sync` has no reopen arm. A task in an ACTIVE
//! status (pending / needs-ruling) whose linked issue is recorded CLOSED
//! (`issue_closed_at` set — e.g. the row was closed, synced, then came back to
//! the active store through a merge or a hand edit) gets no action at all, so
//! the GitHub mirror keeps reading "resolved" while the work is open.
//!
//! Observed 2026-10-04: six issues (#420 #427 #497 #633 #862 #924) were CLOSED
//! while their tasks were active, and the dry-run plan did not list them.
//!
//! Drives the real binary's dry-run `sync` (no `gh` is called by a dry run)
//! against a hand-written store, like `sync_only_scope.rs`. The control row
//! proves the plan is printed for this store at all.
//!
//! Repro: cargo test -p backlog --test backlog_7e575392 -- --ignored
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

/// Dry-run `backlog sync` over a store holding: one unmirrored pending task
/// (control: must plan a create), one pending and one needs-ruling task whose
/// issues are recorded closed.
fn dry_run_sync(tag: &str) -> String {
    let t = std::env::temp_dir().join(format!("bl-7e575392-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let (home, bin, repo) = (t.join("home"), t.join("bin"), t.join("repo"));
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    symlink(tool_path("git"), bin.join("git")).unwrap();
    common::linked_checkout(&repo);
    assert!(Command::new("git")
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
id = "ctrl0001"
title = "pending, unmirrored (control)"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 1
updated_at = 1
weight = 0.0

[[task]]
id = "pend0420"
title = "pending again, issue recorded closed"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 2
updated_at = 2
weight = 0.0
issue_number = 420
issue_url = "https://github.com/o/r/issues/420"
issue_closed_at = 1791024507

[[task]]
id = "rule0427"
title = "needs-ruling, issue recorded closed"
project = "/repo"
tags = []
status = "needs-ruling"
notes = ""
created_at = 3
updated_at = 3
weight = 0.0
issue_number = 427
issue_url = "https://github.com/o/r/issues/427"
issue_closed_at = 1791045089
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
fn control_dry_run_plans_the_unmirrored_create() {
    let out = dry_run_sync("ctrl");
    assert!(
        out.contains("create  ctrl0001"),
        "control: the plan must list the create: {out}"
    );
}

#[test]
#[ignore = "backlog 7e575392: RED repro"]
fn active_task_with_closed_issue_is_planned_for_reopen() {
    let out = dry_run_sync("reopen");
    for (id, num) in [("pend0420", "#420"), ("rule0427", "#427")] {
        assert!(
            out.contains(id) && out.contains(num),
            "active task {id} has its issue {num} recorded CLOSED, but sync plans nothing for it: {out}"
        );
    }
}
