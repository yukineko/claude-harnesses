// Integration test: unwrap/expect/panic are allowed (the workspace lint denies
// them for production code only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Closure regression tests written by an independent verifier for the
//! backlog-closure audit (batch b1_0). Each test is named after the backlog id
//! whose closure it proves and drives the REAL `backlog` binary with `HOME`
//! pinned to a temp dir, so no test touches the operator's real stores.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-audit-b1-0-it-{}-{}-{}",
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

fn run_with_stdin(args: &[&str], cwd: &Path, home: &Path, stdin: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
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

fn run(args: &[&str], cwd: &Path, home: &Path) -> (i32, String, String) {
    run_with_stdin(args, cwd, home, "")
}

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
        .expect("git is available (failing rather than skipping)");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo (main tree `a`) with a committed `.backlog/tasks.toml` holding two
/// tasks added through the real CLI, plus a real linked worktree `b`.
struct Repo {
    home: PathBuf,
    a: PathBuf,
    b: PathBuf,
}

fn repo(tag: &str) -> Repo {
    let root = unique(tag);
    let home = root.join("home");
    let a = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&a).unwrap();
    git(&["init", "-q", "-b", "main"], &a, &home);
    git(&["commit", "-q", "--allow-empty", "-m", "init"], &a, &home);
    let project = a.to_str().unwrap().to_string();
    for (title, prio) in [("first", "p0"), ("second", "p1")] {
        let (code, out, err) = run(
            &[
                "add",
                "--title",
                title,
                "--project",
                &project,
                "--priority",
                prio,
            ],
            &a,
            &home,
        );
        assert_eq!(code, 0, "fixture add: out={out} err={err}");
    }
    git(&["add", ".backlog/tasks.toml"], &a, &home);
    git(&["commit", "-q", "-m", "seed"], &a, &home);
    let b = root.join("wt");
    git(
        &["worktree", "add", "-q", "-b", "side", b.to_str().unwrap()],
        &a,
        &home,
    );
    assert!(b.join(".git").is_file(), "b must be a linked worktree");
    assert_eq!(
        git(&["status", "--porcelain"], &a, &home),
        "",
        "fixture clean"
    );
    Repo { home, a, b }
}

/// backlog b9e09691 — done_criteria: "after a normal flow cycle the main
/// working tree is NOT dirty". A flow cycle claims (`next --claim`) and the
/// next session runs the SessionStart hook; neither may write the tracked
/// store, so `git status --porcelain` of the main tree stays empty and the
/// committed bytes are unchanged.
#[test]
fn backlog_b9e09691_claim_and_session_start_leave_the_main_tree_clean() {
    let r = repo("b9e09691");
    let committed = std::fs::read(r.a.join(".backlog/tasks.toml")).unwrap();

    for _ in 0..2 {
        let (code, out, err) = run(&["next", "--claim"], &r.a, &r.home);
        assert_eq!(code, 0, "claim: out={out} err={err}");
        assert!(out.contains("\"claimed\""), "a task was claimed: {out}");
    }
    let payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "audit-b9e09691",
        "cwd": r.a.to_str().unwrap(),
    })
    .to_string();
    let (code, _o, err) = run_with_stdin(&["session-start"], &r.a, &r.home, &payload);
    assert_eq!(code, 0, "session-start: {err}");

    assert_eq!(
        git(&["status", "--porcelain"], &r.a, &r.home),
        "",
        "b9e09691: claim + SessionStart must leave the main working tree clean"
    );
    assert_eq!(
        std::fs::read(r.a.join(".backlog/tasks.toml")).unwrap(),
        committed,
        "b9e09691: the tracked store must be byte-identical to the committed one"
    );
}

/// backlog 6f629b2b — a driver registered under a LINKED WORKTREE path must be
/// visible to a presence query made with the MAIN tree path (what autoflow /
/// daily ask). If the registry keyed on the raw worktree path, the main-tree
/// query would report `active:false` — the double-drive the item feared.
#[test]
fn backlog_6f629b2b_driver_registered_from_worktree_is_seen_from_main_tree() {
    let r = repo("6f629b2b");
    let wt = r.b.to_str().unwrap();
    let main = r.a.to_str().unwrap();
    let (code, out, err) = run(
        &[
            "driver",
            "register",
            "--session-id",
            "audit-6f629b2b",
            "--project",
            wt,
        ],
        &r.b,
        &r.home,
    );
    assert_eq!(code, 0, "register: out={out} err={err}");

    let (code, out, err) = run(&["driver", "status", "--project", main], &r.a, &r.home);
    assert_eq!(code, 0, "status: out={out} err={err}");
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("status must be JSON ({e}): {out}"));
    assert_eq!(
        v["active"],
        serde_json::Value::Bool(true),
        "6f629b2b: a driver registered from the worktree must be active when queried by \
         the main tree path: {v}"
    );
    assert_ne!(
        v["undetermined"],
        serde_json::Value::Bool(true),
        "the answer must be an observation, not an undetermined registry: {v}"
    );
}

/// backlog 96072327 — with `store_dir` pinned, a task this checkout ADDS is
/// listed from this checkout, and a foreign project's row in the same pinned
/// store is not. Uses the default (symlinked, on macOS) temp dir, which is
/// exactly where the two `a_pinned_store_dir_still_*` tests were RED.
#[test]
fn backlog_96072327_pinned_store_dir_scopes_by_project_under_default_tmpdir() {
    let home = unique("96072327-home");
    let pinned = unique("96072327-pinned");
    let root = unique("96072327-repo");
    git(&["init", "-q"], &root, &home);
    std::fs::create_dir_all(home.join(".backlog")).unwrap();
    std::fs::write(
        home.join(".backlog").join("config.toml"),
        format!("store_dir = \"{}\"\n", pinned.display()),
    )
    .unwrap();
    // A foreign project's row, written directly (as another machine would).
    std::fs::write(
        pinned.join("tasks.toml"),
        "[[task]]\nid = \"22222222\"\ntitle = \"belongs elsewhere\"\n\
         project = \"/Users/some-other-machine/src/thing\"\ntags = [\"p1\"]\n\
         status = \"pending\"\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\n\
         weight = 0.0\n\n",
    )
    .unwrap();
    let (code, out, err) = run(
        &[
            "add",
            "--title",
            "belongs here",
            "--project",
            root.to_str().unwrap(),
        ],
        &root,
        &home,
    );
    assert_eq!(code, 0, "add into pinned store: out={out} err={err}");
    assert!(
        std::fs::read_to_string(pinned.join("tasks.toml"))
            .unwrap()
            .contains("belongs here"),
        "96072327: with store_dir pinned, add must write the PINNED store"
    );

    let (rc, out, err) = run(&["list", "--status", "pending"], &root, &home);
    assert_eq!(rc, 0, "out={out}\nerr={err}");
    assert!(
        out.contains("belongs here"),
        "96072327: this checkout's own task must be listed from a pinned store.\nout={out}\nerr={err}"
    );
    assert!(
        !out.contains("22222222"),
        "96072327: a pinned store is cross-project; the foreign row must not be listed.\nout={out}"
    );
}
