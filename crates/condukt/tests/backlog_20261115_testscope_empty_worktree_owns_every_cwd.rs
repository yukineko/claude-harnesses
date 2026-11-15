#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observed 2026-11-15 at main 2ace975d1: every `cargo test` in every cwd of
//! the harness project was denied by `condukt testscope` with
//! "which tests worktree  changed is unknown: ... No such file or directory
//! (os error 2)". Trigger: a stale run state (run-20260702-081932, task
//! `requeue-lock`, status `verified`, `"worktree": ""`), now quarantined at
//! ~/.condukt/quarantine/run-20260702-081932.json.
//!
//! Mechanism: crates/condukt/src/subagent_stop.rs `tasks_owning_cwd` skips only
//! `worktree: None`; for `Some("")`, `cwd.starts_with("")` is true for every
//! cwd, so the task owns every cwd. crates/condukt/src/testscope.rs
//! `check_task_scope` then diffs in `PathBuf::from("")` and git fails ENOENT,
//! which is reported as undetermined → Deny.
//!
//! Readings: (a) defect — an empty recorded worktree names no directory, so it
//! must not own an unrelated cwd; (b) intended — the gate is fail-closed on a
//! malformed run state and denying everything is the chosen resolution. Tell
//! apart: under (b) the reason would name the malformed run state; the observed
//! reason instead claims the cwd is inside task worktree "" and fails a git
//! diff in a nonexistent dir, i.e. ownership was misattributed.
//!
//! Asserted: with a run whose only task has `worktree: ""` (status verified),
//! `cargo test -p x --test old_t` in the repo root (no task worktree) is
//! allowed. Control: a different test command (`--test ctl_t`, so it does not
//! occupy the rerun-ledger key of the asserted call) in the same repo before
//! the empty worktree is written is allowed (so a denial is caused by the ""
//! task, not by the rerun ledger).
//! Fail-closed for genuinely owned cwds is pinned by
//! testscope_enforcement_20261114.rs `c_unrelated_existing_test_in_task_worktree_is_denied`.
//!
//! DOES NOT PROVE: how a relative (non-empty) worktree string is resolved; how
//! `""` gets written in the first place; whether a fixed gate should instead
//! Deny with a "malformed run state" reason (reading (b) variant) — this test
//! asserts reading (a).
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

fn repo() -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(d.path().join("tests")).unwrap();
    std::fs::write(d.path().join("tests/old_t.rs"), "#[test] fn o() {}\n").unwrap();
    std::fs::write(d.path().join("tests/ctl_t.rs"), "#[test] fn c() {}\n").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

struct Env {
    home: TempDir,
    ledger_dir: TempDir,
}

impl Env {
    fn ledger(&self) -> PathBuf {
        self.ledger_dir.path().join("ledger.tsv")
    }
}

fn hook(env: &Env, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    let payload = serde_json::json!({
        "session_id": "s",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .arg("testscope")
        .env("HOME", env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_WORKTREE_BASE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn condukt(env: &Env, cwd: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_WORKTREE_BASE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .unwrap()
}

fn run_state_file(env: &Env, run: &str) -> PathBuf {
    fn walk(dir: &Path, name: &str) -> Option<PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(f) = walk(&p, name) {
                    return Some(f);
                }
            } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                return Some(p);
            }
        }
        None
    }
    walk(
        &env.home.path().join(".condukt").join("state"),
        &format!("{run}.json"),
    )
    .unwrap_or_else(|| panic!("fixture: run state {run}.json not found"))
}

fn assert_allow(res: &(Option<i32>, String), what: &str) {
    assert_eq!(
        res.0,
        Some(0),
        "{what}: exit must be 0 (stdout={:?})",
        res.1
    );
    assert!(
        res.1.trim().is_empty(),
        "{what}: expected allow (empty stdout), got {:?}",
        res.1
    );
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-15).
#[test]
#[ignore = "RED: 2026-11-15 a task with worktree \"\" owns every cwd, so testscope denies every cargo test — remove when fixed"]
fn empty_string_worktree_task_does_not_own_unrelated_cwd() {
    let env = Env {
        home: tempfile::tempdir().unwrap(),
        ledger_dir: tempfile::tempdir().unwrap(),
    };
    let r = repo();
    let dec = serde_json::json!({"goal": "g", "tasks": [{
        "id": "requeue-lock", "title": "t", "touched_files": ["src/a.rs"], "deps": [],
        "class": "parallel", "done_criteria": "d",
        "reproduction_tests": "cargo test -p x --test repro_t",
    }]});
    let p = env.home.path().join("dec.json");
    std::fs::write(&p, dec.to_string()).unwrap();
    let out = condukt(
        &env,
        r.path(),
        &[
            "state",
            "init",
            "--run",
            "runE",
            "--file",
            p.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "fixture: state init: {out:?}");

    let cmd = "cargo test -p x --test old_t";
    // Control: before the empty worktree is recorded, the repo root is owned by
    // no task and a test command is allowed. The control uses a DIFFERENT test
    // (`ctl_t`) from the asserted call: both calls share one rerun ledger and
    // one unchanged tree, so reusing `cmd` would make the asserted call hit
    // rule 3 (rerun refused: "already ran on this exact tree") instead of being
    // judged on worktree ownership (observed at 501535dac: the original fixture
    // was refused with exactly that text).
    assert_allow(
        &hook(&env, "cargo test -p x --test ctl_t", r.path()),
        "control: no task worktree recorded",
    );

    // Reproduce the quarantined shape: status verified, worktree "".
    let f = run_state_file(&env, "runE");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
    let t = &mut v["tasks"][0];
    t["status"] = serde_json::json!("verified");
    t["worktree"] = serde_json::json!("");
    std::fs::write(&f, serde_json::to_string_pretty(&v).unwrap()).unwrap();

    let res = hook(&env, cmd, r.path());
    assert_allow(&res, "repo root with a stale task whose worktree is \"\"");
}
