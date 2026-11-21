#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]
//! Observed 2026-11-20 in condukt task worktree
//! /Users/yuki/.condukt/worktrees/run-20261120-170138-92681-t8cad (task t8cad,
//! changed file crates/blastguard/src/detect/payload.rs): running an existing
//! integration test of the same crate to check that the fix leaves pinned
//! asserts unchanged (e.g. `cargo test -p blastguard --test
//! substitution_stdin_exec`) was refused by `condukt testscope` with
//! "inside a condukt task worktree only the task's own tests may run".
//!
//! User ruling 2026-11-20 (verbatim): 「証明ができないというのであれば、それを
//! 証明するためのテストを実施できないのはtestscopeの不具合」.
//!
//! Mechanism: crates/condukt/src/testscope.rs `check_task_scope` allows a
//! `--test <name>` only when it is named in the task's reproduction_tests or is
//! a tests/<name>.rs the worktree added or changed; every other `--test` of the
//! same crate is denied with TASK_SCOPE_REASON.
//!
//! Expected: inside a task worktree, a single named `--test <file>` of an
//! existing integration test of a crate the task changed is not denied
//! (regression check). Unchanged, and asserted here only by the controls: the
//! full-suite refusal (rule 1). The same-tree rerun refusal (rule 3) is not
//! touched by this file.
//!
//! CONFLICT (stated, not hidden): this contradicts the pinned assert
//! testscope_enforcement_20261114.rs `c_unrelated_existing_test_in_task_worktree_is_denied`
//! (`--test old_t`, a pre-existing test of the same single crate). A fix that
//! makes this RED green must re-anchor that assert per the 2026-11-20 ruling.
//!
//! Controls (GREEN at HEAD): the full suite `cargo test -p foo` in the task
//! worktree is denied; an existing test of a crate the task did NOT change
//! (`cargo test -p bar --test bar_t`) is denied. The second control encodes
//! the "task's own crate" boundary of the relayed expectation.
//!
//! DOES NOT PROVE: how a fix determines "the task's crate" (touched_files vs
//! worktree diff vs Cargo metadata); lib/doc selectors; tdd or bash -c
//! routes; rerun-ledger interaction.
//!
//! Helpers are copied from backlog_20261115_testscope_cd_prefix_rule_c.rs.
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
/// unset).
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
        &["state", "init", "--run", run, "--file", p.to_str().unwrap()],
    );
    assert!(out.status.success(), "fixture: state init: {out:?}");
}

/// Write the testscope session marker for `session` the public way: one hook
/// call carrying that session id.
fn prime_marker(env: &Env, session: &str, cwd: &Path) {
    assert_allow(&hook(env, session, "echo hi", cwd), "priming hook call");
}

const SESSION: &str = "sess-r";

struct Fx {
    env: Env,
    repo: TempDir,
    wt: PathBuf,
}

/// Workspace repo on main with crates/foo (src/lib.rs, tests/old_t.rs,
/// tests/repro_t.rs) and crates/bar (src/lib.rs, tests/bar_t.rs). Run `runR`,
/// task `t1` touching crates/foo/src/lib.rs with reproduction_tests
/// `cargo test -p foo --test repro_t`. The task worktree (created by `condukt
/// worktree create`, recorded by `state set --worktree`) commits a change to
/// crates/foo/src/lib.rs only, mirroring the observed t8cad diff.
fn fixture() -> Fx {
    let env = Env::new();
    let r = repo_with(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/foo\", \"crates/bar\"]\n",
        ),
        (
            "crates/foo/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/foo/src/lib.rs", "pub fn f() {}\n"),
        ("crates/foo/tests/old_t.rs", "#[test] fn o() {}\n"),
        ("crates/foo/tests/repro_t.rs", "#[test] fn r() {}\n"),
        (
            "crates/bar/Cargo.toml",
            "[package]\nname = \"bar\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/bar/src/lib.rs", "pub fn b() {}\n"),
        ("crates/bar/tests/bar_t.rs", "#[test] fn b() {}\n"),
    ]);
    init_run(
        &env,
        r.path(),
        "runR",
        &[(
            "t1",
            "crates/foo/src/lib.rs",
            "cargo test -p foo --test repro_t",
        )],
    );
    let out = condukt(
        &env,
        r.path(),
        Some(SESSION),
        &[
            "worktree", "create", "--run", "runR", "--topic", "t1", "--branch", "b1",
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
    prime_marker(&env, SESSION, r.path());
    let out = condukt(
        &env,
        r.path(),
        Some(SESSION),
        &[
            "state",
            "set",
            "--run",
            "runR",
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
    assert!(
        out.status.success(),
        "fixture: state set t1 running: {out:?}"
    );
    std::fs::write(
        wt.join("crates/foo/src/lib.rs"),
        "pub fn f() { let _ = 1; }\n",
    )
    .unwrap();
    git(&wt, &["add", "crates/foo/src/lib.rs"]);
    git(&wt, &["commit", "-q", "-m", "fix foo"]);
    Fx { env, repo: r, wt }
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-20).
#[test]
fn existing_test_of_task_crate_runs_as_regression_check_in_task_worktree() {
    let fx = fixture();
    assert_allow(
        &hook(&fx.env, "s", "cargo test -p foo --test old_t", &fx.wt),
        "task worktree changed crates/foo; --test old_t is an existing foo integration test \
         (regression check of pinned asserts)",
    );
}

#[test]
fn control_full_suite_of_task_crate_in_task_worktree_is_denied() {
    let fx = fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p foo", &fx.wt),
        "full suite `cargo test -p foo` (no target selector) in task worktree",
    );
}

#[test]
fn control_existing_test_of_untouched_crate_in_task_worktree_is_denied() {
    let fx = fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p bar --test bar_t", &fx.wt),
        "--test bar_t of crate bar, which the task did not change, in task worktree",
    );
}
