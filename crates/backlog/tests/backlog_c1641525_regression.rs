//! Closure regression for backlog c1641525.
//!
//! c1641525: a session worktree's `.backlog/tasks.toml` carried an uncommitted
//! rewrite (from a stale snapshot) that turned another session's `claimed`
//! rows back into `pending`; committing and merging it would silently release
//! that claim, so two sessions would work the same task.
//!
//! Property pinned here (end to end, real binary, real git repo + linked
//! worktree, pinned HOME):
//!   1. a claim made in checkout A does not write the tracked store at all, so
//!      no snapshot of that file -- stale or not -- carries the claim to revert;
//!   2. checkout B (whose tasks.toml is the pre-claim snapshot) runs the
//!      SessionStart requeue and an `add`, commits, and is merged into A; after
//!      that merge the claimed task is STILL not handed out again, from either
//!      checkout.
//!
//! Written by an independent closure verifier, not the implementer.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unique_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-c1641525-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn run_in(args: &[&str], cwd: &Path, home: &Path, stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .env("PATH", common::path_with_condukt_shim())
        .args(args)
        .env("HOME", home)
        .env_remove("BACKLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut s) = child.stdin.take() {
        let _ = s.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Close-evidence fixture: `add` lands `pending` only with a REPRODUCED repro
/// test (otherwise `unconfirmed`, outside the workable queue and unclaimable).
/// The repro script is committed on main so both checkouts can run it.
const REPRO: &str = "bash tests/repro_yes.sh";

fn git(args: &[&str], cwd: &Path, home: &Path) -> String {
    let out = Command::new("git")
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn claim_id(cwd: &Path, home: &Path) -> Option<String> {
    let (code, out, err) = run_in(&["next", "--claim"], cwd, home, "");
    assert_eq!(code, 0, "next --claim: stdout={out} stderr={err}");
    if out.contains("no pending tasks") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("next --claim not JSON ({e}): {out}"));
    Some(v["id"].as_str().expect("claim has an id").to_string())
}

#[test]
fn a_stale_worktree_store_merged_into_main_does_not_release_a_claim() {
    let root = unique_root("t");
    let home = root.join("home");
    let a = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&a).unwrap();
    git(&["init", "-q", "-b", "main"], &a, &home);
    git(&["commit", "-q", "--allow-empty", "-m", "init"], &a, &home);
    std::fs::create_dir_all(a.join("tests")).unwrap();
    std::fs::write(a.join("tests/repro_yes.sh"), "echo 'bug present'; exit 1\n").unwrap();
    git(&["add", "tests/repro_yes.sh"], &a, &home);
    git(&["commit", "-q", "-m", "repro script"], &a, &home);
    let project = a.to_str().unwrap().to_string();
    // 1e6f00ae: seed through a throwaway linked worktree (primary writes are
    // refused), then adopt the store into `a`.
    let sw = common::seed_worktree(&a);
    let (code, _, err) = run_in(
        &[
            "add",
            "--title",
            "only task",
            "--project",
            &project,
            "--repro-test",
            REPRO,
        ],
        &sw,
        &home,
        "",
    );
    assert_eq!(code, 0, "seed add: {err}");
    common::adopt_store(&sw, &a);
    git(&["add", ".backlog/tasks.toml"], &a, &home);
    git(&["commit", "-q", "-m", "seed"], &a, &home);
    let b = root.join("wt");
    git(
        &["worktree", "add", "-q", "-b", "side", b.to_str().unwrap()],
        &a,
        &home,
    );

    // A claims the only task.
    let claimed = claim_id(&a, &home).expect("A must be handed the task");

    // (1) The claim did not touch A's tracked store.
    assert_eq!(
        git(&["status", "--porcelain"], &a, &home),
        "",
        "a claim must not dirty the tracked tasks.toml (it would be revertible by a stale snapshot)"
    );

    // B (pre-claim snapshot) runs the SessionStart requeue and an add, commits.
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "c1641525-b",
        "cwd": b.to_str().unwrap(),
    })
    .to_string();
    let (code, _, err) = run_in(&["session-start"], &b, &home, &payload);
    assert_eq!(code, 0, "session-start in B: {err}");
    let (code, _, err) = run_in(
        &[
            "add",
            "--title",
            "from B",
            "--project",
            &project,
            "--repro-test",
            REPRO,
        ],
        &b,
        &home,
        "",
    );
    assert_eq!(code, 0, "add in B: {err}");
    git(&["add", ".backlog/tasks.toml"], &b, &home);
    git(&["commit", "-q", "-m", "B writes its store"], &b, &home);

    // Merge B into main.
    git(&["merge", "-q", "--no-edit", "side"], &a, &home);

    // (2) The claimed task is not handed out again, from either checkout.
    for cwd in [&a, &b] {
        let next = claim_id(cwd, &home);
        assert_ne!(
            next.as_deref(),
            Some(claimed.as_str()),
            "claimed task {claimed} was handed out again from {} after the merge",
            cwd.display()
        );
    }
}
