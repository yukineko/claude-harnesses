//! DoD14 (c)/(d): driver liveness is one fact per PROJECT, not per checkout.
//!
//! A driver registers from linked worktree W1. `backlog lock status` must then
//! report that same live driver whether it is asked about the main tree, W1, or
//! a sibling linked worktree W2 (c). An unrelated repo that nobody registered
//! must read `none` (d) — otherwise an implementation that reports "busy" for
//! everything would satisfy (c) vacuously.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-dlw-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn backlog(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(home)
        .stdin(Stdio::null())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lock_status(home: &Path, project: &Path) -> String {
    let (code, out, err) = backlog(
        home,
        &["lock", "status", "--project", project.to_str().unwrap()],
    );
    assert_eq!(code, 0, "lock status {} failed: {err}", project.display());
    out.trim().to_string()
}

#[test]
fn driver_registered_in_one_worktree_is_live_from_main_and_every_worktree() {
    let root = temp_root("c");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // W1 = linked worktree of `<root>/w1.main`; W2 = a second linked worktree
    // of the SAME main.
    let w1 = common::linked_checkout(&root.join("w1"));
    let main = root.join("w1.main");
    let w2 = root.join("w2");
    git(
        &main,
        &["worktree", "add", "-q", "--detach", w2.to_str().unwrap()],
    );

    let (code, _, err) = backlog(
        &home,
        &[
            "driver",
            "register",
            "--session-id",
            "S1",
            "--project",
            w1.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "driver register failed: {err}");

    for (label, x) in [("main", &main), ("w1", &w1), ("w2", &w2)] {
        let out = lock_status(&home, x);
        let v: serde_json::Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("{label}: expected JSON ({e}), got: {out}"));
        assert!(v.is_object(), "{label}: expected a JSON object, got: {out}");
        assert_eq!(
            v["driver_count"], 1,
            "{label}: driver_count must be 1, got: {out}"
        );
        assert_eq!(
            v["drivers"][0]["session_id"], "S1",
            "{label}: drivers[0] must be S1, got: {out}"
        );
    }

    // DoD14(d) anti-vacuity control: a separate repo nobody registered in.
    let other = root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    git(&other, &["commit", "-q", "--allow-empty", "-m", "init"]);
    assert_eq!(
        lock_status(&home, &other),
        "none",
        "an unrelated, never-registered repo must read exactly `none`"
    );
}
