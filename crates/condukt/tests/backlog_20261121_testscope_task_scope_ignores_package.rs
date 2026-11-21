#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog RED (2026-11-21, measured on main cc8738957; the defect predates
//! condukt run run-20261121-114054-75519): testscope rule 4 (task scope,
//! crates/condukt/src/testscope.rs) never looks at `-p`.
//!
//! Inside a condukt task worktree a `--test <name>` is allowed when `<name>` is
//! named in the task's `reproduction_tests`, or when `<name>` is the stem of a
//! `tests/<name>.rs` the worktree changed. Both routes compare only the test
//! NAME (`Selector::Test(String)`; `worktree_changed_tests` keeps only the file
//! stem), so `cargo test -p <crate the task did not touch> --test <name>` is
//! allowed too.
//!
//! Two readings:
//! (a) a scope hole: the selected test may belong to a crate other than the
//!     one whose tests/<name>.rs changed (or that reproduction_tests names);
//! (b) intended: the test name alone identifies the task's test.
//!
//! The fixture tells them apart: crates `a` and `b` BOTH have a
//! `tests/shared.rs` at base, and the worktree changes only
//! `crates/a/tests/shared.rs`. There the name does not identify one test, so
//! (b)'s premise is false and `cargo test -p b --test shared` runs a test file
//! the task never touched. These tests assert reading (a): that command is
//! denied, on both routes. The controls show the same-crate command is allowed,
//! so a deny cannot come from the fixture being broken.
//!
//! Driving follows tests/testscope_enforcement_20261114.rs (temp `$HOME`, temp
//! `CONDUKT_TESTSCOPE_LEDGER`, temp git repo, `condukt worktree create` +
//! `state set --worktree`); commands are only judged, never executed.
//!
//! DOES NOT PROVE: how `-p` should be resolved to a crate directory (the
//! fixture uses `crates/<p>/`); `--manifest-path`, `--workspace` exclusions, or
//! a cwd inside a crate directory without `-p`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

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

fn repo_with(files: &[(&str, &str)]) -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    for (rel, body) in files {
        let p = d.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    git(d.path(), &["add", "-A"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

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

fn condukt(env: &Env, cwd: &Path, session: Option<&str>, args: &[&str]) -> Output {
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
    c.output().unwrap()
}

fn assert_deny(res: &(Option<i32>, String), what: &str) {
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
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "deny",
        "{what}: {v}"
    );
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

const SESSION: &str = "sess-p";

/// Repo with crates/a and crates/b, each holding tests/shared.rs; run `runP`
/// with task t1 carrying `repro`; t1's worktree (recorded via `state set
/// --worktree`) modifies only crates/a/tests/shared.rs (uncommitted) when
/// `change_a` is set.
fn fixture(repro: &str, change_a: bool) -> (Env, TempDir, PathBuf) {
    let env = Env::new();
    let r = repo_with(&[
        ("crates/a/src/lib.rs", "pub fn f() {}\n"),
        ("crates/a/tests/shared.rs", "#[test] fn a() {}\n"),
        ("crates/b/src/lib.rs", "pub fn g() {}\n"),
        ("crates/b/tests/shared.rs", "#[test] fn b() {}\n"),
    ]);
    let dec = serde_json::json!({"goal": "g", "tasks": [{
        "id": "t1", "title": "t1", "touched_files": ["crates/a/src/lib.rs"], "deps": [],
        "class": "parallel", "done_criteria": "d", "reproduction_tests": repro,
    }]});
    let p = env.home.path().join("runP.dec.json");
    std::fs::write(&p, dec.to_string()).unwrap();
    let out = condukt(
        &env,
        r.path(),
        None,
        &[
            "state",
            "init",
            "--run",
            "runP",
            "--file",
            p.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "fixture: state init: {out:?}");
    let out = condukt(
        &env,
        r.path(),
        Some(SESSION),
        &[
            "worktree", "create", "--run", "runP", "--topic", "t1", "--branch", "b1",
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
    assert_allow(
        &hook(&env, SESSION, "echo hi", r.path()),
        "priming hook call",
    );
    let out = condukt(
        &env,
        r.path(),
        Some(SESSION),
        &[
            "state",
            "set",
            "--run",
            "runP",
            "--task",
            "t1",
            "--status",
            "running",
            "--worktree",
            wt.to_str().unwrap(),
            "--branch",
            &branch,
        ],
    );
    assert!(out.status.success(), "fixture: state set running: {out:?}");
    if change_a {
        std::fs::write(wt.join("crates/a/tests/shared.rs"), "#[test] fn a2() {}\n").unwrap();
    }
    (env, r, wt)
}

// ── changed-file route ──────────────────────────────────────────────────────

#[test]
fn changed_route_other_crate_same_stem_is_denied() {
    let (env, _r, wt) = fixture("cargo test -p a --lib nothing", true);
    assert_deny(
        &hook(&env, "s", "cargo test -p b --test shared", &wt),
        "-p b --test shared: only crates/a/tests/shared.rs changed",
    );
}

#[test]
fn control_changed_route_same_crate_is_allowed() {
    let (env, _r, wt) = fixture("cargo test -p a --lib nothing", true);
    assert_allow(
        &hook(&env, "s", "cargo test -p a --test shared", &wt),
        "-p a --test shared: crates/a/tests/shared.rs changed",
    );
}

// ── reproduction_tests route ────────────────────────────────────────────────

#[test]
fn repro_route_other_crate_same_name_is_denied() {
    let (env, _r, wt) = fixture("cargo test -p a --test shared", false);
    assert_deny(
        &hook(&env, "s", "cargo test -p b --test shared", &wt),
        "-p b --test shared: reproduction_tests names -p a --test shared",
    );
}

#[test]
fn control_repro_route_same_crate_is_allowed() {
    let (env, _r, wt) = fixture("cargo test -p a --test shared", false);
    assert_allow(
        &hook(&env, "s", "cargo test -p a --test shared", &wt),
        "-p a --test shared: named in reproduction_tests",
    );
}
