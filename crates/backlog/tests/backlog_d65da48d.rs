#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog d65da48d: the "deferred" status exists on the READ side but no
//! machine consumer can ever observe it.
//!
//! * `backlog list` (text) renders a task whose `defer_until` is in the future
//!   as `deferred` (derived, main.rs `t.is_deferred(now)`).
//! * `backlog list --json` — the feed overwatch's SessionStart summary parses
//!   (`overwatch::aggregate::parse_backlog`, which counts
//!   `status == "deferred"`) — emits the stored status (`failed`) instead.
//! * `backlog edit --status deferred` is rejected ("valid values are pending |
//!   done | failed").
//!
//! So overwatch's `deferred: N` counter is unreachable and always prints 0,
//! which reads as "nothing is on hold" while deferred work exists.
//!
//! Real binary, real git repo, pinned HOME; nothing outside the temp dir.

use std::path::Path;
use std::process::{Command, Stdio};

fn run(args: &[&str], cwd: &Path, home: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", home)
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("backlog runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
#[ignore = "backlog d65da48d: open defect, remove ignore when fixed"]
fn a_task_listed_as_deferred_is_deferred_in_the_json_feed_too() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("home");
    let repo = base.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let g = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(g.status.success());

    let repo_s = repo.to_string_lossy().into_owned();
    let (rc, _, err) = run(
        &[
            "add",
            "--title",
            "deferral probe d65da48d",
            "--project",
            &repo_s,
        ],
        &repo,
        &home,
    );
    assert_eq!(rc, 0, "add: {err}");
    let (rc, json, err) = run(&["list", "--json"], &repo, &home);
    assert_eq!(rc, 0, "list --json: {err}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let id = v[0]["id"].as_str().expect("id").to_string();

    // `fail` is the only CLI surface that defers (sets defer_until = now+2d).
    let (rc, _, err) = run(&["fail", &id, "--reason", "probe"], &repo, &home);
    assert_eq!(rc, 0, "fail: {err}");

    let (_, text, _) = run(&["list"], &repo, &home);
    assert!(
        text.lines()
            .any(|l| l.contains(&id) && l.contains("deferred")),
        "precondition: the human-facing list shows the task as deferred:\n{text}"
    );

    let (_, json, _) = run(&["list", "--json"], &repo, &home);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let row = v
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id.as_str())
        .expect("task in json feed")
        .clone();
    assert_eq!(
        row["status"], "deferred",
        "the JSON feed overwatch counts (`status == \"deferred\"`) reports this \
         deferred task as {:?}, so overwatch's deferred counter can never be \
         non-zero: {row}",
        row["status"]
    );
}
