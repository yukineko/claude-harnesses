#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]
//! Observed 2026-11-15 at main 2ace975d1: two rerun refusals carried the
//! byte-identical fingerprint "(2ace975d1fc8783b496cc853711bed7992c900a7:1dcbe1ce95dcba8e)".
//! The second ran as `cd /Users/yuki/.condukt/worktrees/run-20261115-173313-90185-t1
//! && cargo check -p blastguard && tdd green ...`; that worktree's HEAD was
//! ddefb055b with payload.rs modified. 2ace975d1 is the HEAD of the main
//! checkout, the Bash tool's persistent cwd (the hook payload `cwd`).
//! `1dcbe1ce95dcba8e` is the dirty-hash of an empty diff plus an empty
//! untracked list (measured: the same suffix appears for any clean temp repo).
//!
//! Mechanism: crates/condukt/src/testscope.rs `judge(command, cwd, ..)` gets
//! `cwd` from the hook payload; a leading `cd <dir> &&` is an ordinary segment
//! and never changes it. Both `check_rerun` (tree_identity of cwd, rule 3) and
//! `check_task_scope(tests, cwd)` (rule 4, Rule C) judge the payload cwd.
//! The rerun-ledger half is already pinned by
//! backlog_20261114_testscope_cd_tree_identity.rs (backlog 214b39b5, still RED
//! at 2ace975d1). THIS file pins the Rule C half, which is fail-OPEN: from a
//! payload cwd owned by no task, `cd <task worktree> && cargo test --test
//! <out of scope>` is allowed although the test runs inside the task worktree.
//!
//! Readings: (a) defect: the tree a test runs against is the `cd` target, so
//! both rules must judge it; (b) intended: the cd prefix is not modelled. Under
//! (b) the fail-closed direction must still hold: an unmodelled cd that may
//! move into a task worktree is a cannot-determine and must not resolve to
//! Allow. Either reading makes the asserted case non-Allow; they differ only in
//! the reason text.
//!
//! Control (GREEN at HEAD): the same out-of-scope test with payload cwd = the
//! task worktree is denied (existing Rule C behaviour).
//!
//! DOES NOT PROVE: `cd` chains, `pushd`, `git -C`, subshells, `cargo
//! --manifest-path`; how a fix keys the ledger. A fix must not let `cd` dodge
//! the ledger (e.g. cd-ing to a fresh copy to re-run the same test on the same
//! code); that is not asserted here.
//!
//! Helpers are copied from testscope_enforcement_20261114.rs.
//!
//! RED written by an independent test author, not the implementer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

// ── shared fixture ──────────────────────────────────────────────────────────

struct Env {
    home: TempDir,
    ledger_dir: TempDir,
}

impl Env {
    fn new() -> Self {
        Env {
            home: tempfile::tempdir().unwrap(),
            ledger_dir: tempfile::tempdir().unwrap(),
        }
    }
    fn ledger(&self) -> PathBuf {
        self.ledger_dir.path().join("ledger.tsv")
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A git repo on `main` with one commit holding `a.txt` and the given files.
fn repo_with(files: &[(&str, &str)]) -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    for (rel, body) in files {
        let p = d.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn repo() -> TempDir {
    repo_with(&[])
}

/// `condukt testscope` with a PreToolUse(Bash) payload. Returns (exit, stdout).
fn hook(env: &Env, session: &str, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    let payload = serde_json::json!({
        "session_id": session,
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

/// `condukt <args>` in `cwd`; `session` is `$CLAUDE_CODE_SESSION_ID` (None =
/// unset). `path_prefix` is prepended to PATH.
fn condukt(
    env: &Env,
    cwd: &Path,
    session: Option<&str>,
    path_prefix: Option<&Path>,
    args: &[&str],
) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_condukt"));
    harness_core::git_env::scrub(&mut c)
        .args(args)
        .current_dir(cwd)
        .env("HOME", env.home.path())
        .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_WORKTREE_BASE")
        .env_remove("CONDUKT_SINGLE_WORKTREE");
    match session {
        Some(s) => {
            c.env("CLAUDE_CODE_SESSION_ID", s);
        }
        None => {
            c.env_remove("CLAUDE_CODE_SESSION_ID");
        }
    }
    if let Some(p) = path_prefix {
        let old = std::env::var("PATH").unwrap_or_default();
        c.env("PATH", format!("{}:{old}", p.display()));
    }
    c.output().unwrap()
}

fn assert_deny(res: &(Option<i32>, String), what: &str) -> String {
    assert_eq!(
        res.0,
        Some(0),
        "{what}: exit must be 0 (stdout={:?})",
        res.1
    );
    let v: serde_json::Value = serde_json::from_str(&res.1).unwrap_or_else(|e| {
        panic!(
            "{what}: expected a deny, stdout not JSON ({e}): {:?}",
            res.1
        )
    });
    let h = &v["hookSpecificOutput"];
    assert_eq!(h["permissionDecision"], "deny", "{what}: {v}");
    let reason = h["permissionDecisionReason"].as_str().unwrap_or("");
    assert!(!reason.is_empty(), "{what}: empty reason");
    reason.to_string()
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

/// Find `<run>.json` anywhere under `$HOME/.condukt/state`.
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

fn task_status(env: &Env, run: &str, task: &str) -> String {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_state_file(env, run)).unwrap()).unwrap();
    v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == task)
        .unwrap()["status"]
        .as_str()
        .unwrap()
        .to_string()
}

/// `state init --run <run>` with the given tasks `(id, touched file, reproduction_tests)`.
fn init_run(env: &Env, repo: &Path, run: &str, tasks: &[(&str, &str, &str)]) {
    let tasks: Vec<serde_json::Value> = tasks
        .iter()
        .map(|(id, file, repro)| {
            serde_json::json!({
                "id": id, "title": id, "touched_files": [file], "deps": [],
                "class": "parallel", "done_criteria": "d", "reproduction_tests": repro,
            })
        })
        .collect();
    let dec = serde_json::json!({"goal": "g", "tasks": tasks});
    let p = env.home.path().join(format!("{run}.dec.json"));
    std::fs::write(&p, dec.to_string()).unwrap();
    let out = condukt(
        env,
        repo,
        None,
        None,
        &["state", "init", "--run", run, "--file", p.to_str().unwrap()],
    );
    assert!(out.status.success(), "fixture: state init: {out:?}");
}

/// Write the testscope session marker for `session` the public way: one hook
/// call carrying that session id.
fn prime_marker(env: &Env, session: &str, cwd: &Path) {
    assert_allow(&hook(env, session, "echo hi", cwd), "priming hook call");
}

struct WtFx {
    env: Env,
    repo: TempDir,
    wt: PathBuf,
}

const SESSION_C: &str = "sess-c";

/// Repo on main with tests/old_t.rs, tests/repro_t.rs, tests/changed_t.rs; run
/// `runC` whose task(s) carry `repro`; a worktree for the first task created by
/// `condukt worktree create`, recorded on every listed task via `state set
/// --worktree`. In the worktree: tests/new_committed.rs committed,
/// tests/new_untracked.rs untracked, tests/changed_t.rs modified uncommitted.
fn wt_fixture(repro: &str, task_ids: &[&str]) -> WtFx {
    let env = Env::new();
    let r = repo_with(&[
        ("tests/old_t.rs", "#[test] fn o() {}\n"),
        ("tests/repro_t.rs", "#[test] fn r() {}\n"),
        ("tests/changed_t.rs", "#[test] fn c() {}\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
    ]);
    let tasks: Vec<(String, String)> = task_ids
        .iter()
        .map(|id| (id.to_string(), format!("src/{id}.rs")))
        .collect();
    let spec: Vec<(&str, &str, &str)> = tasks
        .iter()
        .map(|(id, f)| (id.as_str(), f.as_str(), repro))
        .collect();
    init_run(&env, r.path(), "runC", &spec);
    let out = condukt(
        &env,
        r.path(),
        Some(SESSION_C),
        None,
        &[
            "worktree",
            "create",
            "--run",
            "runC",
            "--topic",
            task_ids[0],
            "--branch",
            "b1",
        ],
    );
    assert!(out.status.success(), "fixture: worktree create: {out:?}");
    let wt = PathBuf::from(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap(),
    );
    assert!(wt.is_dir(), "fixture: worktree path {wt:?}");
    let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
    prime_marker(&env, SESSION_C, r.path());
    for id in task_ids {
        let out = condukt(
            &env,
            r.path(),
            Some(SESSION_C),
            None,
            &[
                "state",
                "set",
                "--run",
                "runC",
                "--task",
                id,
                "--status",
                "running",
                "--worktree",
                wt.to_str().unwrap(),
                "--branch",
                &branch,
            ],
        );
        assert!(
            out.status.success(),
            "fixture: state set {id} running: {out:?}"
        );
    }
    std::fs::write(wt.join("tests/new_committed.rs"), "#[test] fn n() {}\n").unwrap();
    git(&wt, &["add", "tests/new_committed.rs"]);
    git(&wt, &["commit", "-q", "-m", "add test"]);
    std::fs::write(wt.join("tests/new_untracked.rs"), "#[test] fn u() {}\n").unwrap();
    std::fs::write(wt.join("tests/changed_t.rs"), "#[test] fn c2() {}\n").unwrap();
    WtFx { env, repo: r, wt }
}

fn c_default() -> WtFx {
    wt_fixture("cargo test -p x --test repro_t", &["t1"])
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-15).
#[test]
fn out_of_scope_test_via_cd_into_task_worktree_is_denied() {
    let fx = c_default();
    let cmd = format!("cd {} && cargo test -p x --test old_t", fx.wt.display());
    assert_deny(
        &hook(&fx.env, "s", &cmd, fx.repo.path()),
        "payload cwd = repo root, cd into task worktree, --test old_t (out of scope)",
    );
}

#[test]
fn control_out_of_scope_test_with_payload_cwd_in_task_worktree_is_denied() {
    let fx = c_default();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x --test old_t", &fx.wt),
        "payload cwd = task worktree, --test old_t (out of scope)",
    );
}
