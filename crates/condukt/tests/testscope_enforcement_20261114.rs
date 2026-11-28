#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for the testscope enforcement ruling (user ruling 2026-11-14:
//! 「規則からはずれた点は許さないので、余計な実行はシステムでできないようにする」).
//!
//! Three observed holes in `condukt testscope` (crates/condukt/src/testscope.rs)
//! and its neighbours, each pinned with deny cases and allowed controls:
//!
//! * **A** — a session that never loaded the hook can still mark a condukt task
//!   `running`. `condukt testscope` must write a per-session marker
//!   (`<ledger parent>/sessions/<session_id>`), and `condukt state set --status
//!   running` must refuse (non-zero, status unchanged) when the marker of
//!   `$CLAUDE_CODE_SESSION_ID` is absent, or when there is no session id.
//! * **B** — `tdd red|green --cmd "<c>"` (also `--cmd=<c>`) is judged as if `<c>`
//!   were a bare Bash command (full suite deny, same-tree rerun deny). `tdd
//!   green` (backlog 8f82f09d) and `tdd red` (backlog fb4f650d) are one-way
//!   exceptions: a bare run does not refuse a later `tdd green` / `tdd red` of
//!   `<c>`, but each records `<c>`'s bare key, so a bare run after it is
//!   refused, and a second `tdd green` of `<c>` (any nesting depth) is refused. B here exercises `tdd red` across routes and `tdd
//!   green` twice; `condukt state test` consults the same rerun ledger inside
//!   the binary.
//! * **C** — inside a run task's recorded worktree, each `--test <name>` must be
//!   named in the task's `reproduction_tests` or be a `tests/<name>.rs` the
//!   worktree added/changed against its base (committed, uncommitted or
//!   untracked); `--lib`/`--bin`/`--doc` only when `reproduction_tests` names
//!   that selector. Outside a task worktree C does not apply. A cwd matching
//!   several tasks, or unreadable run state, is denied.
//!
//! Isolation follows tests/testscope_20261114.rs: a temp `$HOME` (condukt's
//! state dir and worktree base live under `$HOME/.condukt`), a temp
//! `CONDUKT_TESTSCOPE_LEDGER`, temp git repos. Commands handed to the hook are
//! only judged, never executed; `state test` executes a fake `cargo` on PATH
//! that counts its invocations.
//!
//! DOES NOT PROVE: the exact marker file layout (only that the hook, given the
//! same `CONDUKT_TESTSCOPE_LEDGER`, produces whatever `state set` accepts); how
//! `--test <name>` is matched inside `reproduction_tests` beyond exact presence;
//! which base revision C diffs against when it differs from the fork point (the
//! fixture's base and fork point are the same commit); `nextest` variants of C.

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
    hook_as("testscope", None, env, session, cmd, cwd)
}

/// `hook`, then (when the hook allowed it, i.e. the command ran) the
/// PostToolUse that confirms the run (spec testscope-confirmed-run R2: input
/// only, the asserts stay as they were).
fn hook_confirmed(env: &Env, session: &str, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = format!("tu-{}", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
    let res = hook_as("testscope", Some(&id), env, session, cmd, cwd);
    if res.1.trim().is_empty() {
        hook_as("testscope-confirm", Some(&id), env, session, cmd, cwd);
    }
    res
}

fn hook_as(
    sub: &str,
    tuid: Option<&str>,
    env: &Env,
    session: &str,
    cmd: &str,
    cwd: &Path,
) -> (Option<i32>, String) {
    let mut payload = serde_json::json!({
        "session_id": session,
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    if let Some(id) = tuid {
        payload["tool_use_id"] = serde_json::json!(id);
    }
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .arg(sub)
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

// ── A: a session without the hook cannot start a task ───────────────────────

fn a_fixture() -> (Env, TempDir) {
    let env = Env::new();
    let r = repo();
    init_run(
        &env,
        r.path(),
        "runA",
        &[("t1", "a.txt", "cargo test -p x --test t")],
    );
    (env, r)
}

#[test]
fn a_running_is_refused_without_a_session_marker() {
    let (env, r) = a_fixture();
    let out = condukt(
        &env,
        r.path(),
        Some("sess-nomarker"),
        None,
        &[
            "state", "set", "--run", "runA", "--task", "t1", "--status", "running",
        ],
    );
    assert!(
        !out.status.success(),
        "state set running must be refused when the session never ran the testscope hook: {out:?}"
    );
    assert_ne!(task_status(&env, "runA", "t1"), "running");
}

#[test]
fn a_running_is_refused_without_a_session_id() {
    let (env, r) = a_fixture();
    // A marker exists for some session, but the caller names none.
    prime_marker(&env, "sess-a", r.path());
    let out = condukt(
        &env,
        r.path(),
        None,
        None,
        &[
            "state", "set", "--run", "runA", "--task", "t1", "--status", "running",
        ],
    );
    assert!(
        !out.status.success(),
        "state set running must be refused when CLAUDE_CODE_SESSION_ID is absent: {out:?}"
    );
    assert_ne!(task_status(&env, "runA", "t1"), "running");
}

#[test]
fn a_running_is_refused_when_only_another_sessions_marker_exists() {
    let (env, r) = a_fixture();
    prime_marker(&env, "sess-other", r.path());
    let out = condukt(
        &env,
        r.path(),
        Some("sess-mine"),
        None,
        &[
            "state", "set", "--run", "runA", "--task", "t1", "--status", "running",
        ],
    );
    assert!(
        !out.status.success(),
        "another session's marker must not admit this session: {out:?}"
    );
    assert_ne!(task_status(&env, "runA", "t1"), "running");
}

#[test]
fn a_control_running_is_allowed_with_the_session_marker() {
    let (env, r) = a_fixture();
    prime_marker(&env, "sess-a", r.path());
    let out = condukt(
        &env,
        r.path(),
        Some("sess-a"),
        None,
        &[
            "state", "set", "--run", "runA", "--task", "t1", "--status", "running",
        ],
    );
    assert!(
        out.status.success(),
        "state set running with marker: {out:?}"
    );
    assert_eq!(task_status(&env, "runA", "t1"), "running");
}

// ── B: tdd --cmd and state test follow the same rules ───────────────────────

#[test]
fn b_tdd_full_suite_is_denied() {
    for c in [
        "tdd red --cmd \"cargo test -p x\"",
        "tdd green --cmd \"cargo test -p x\"",
        "tdd red --cmd=\"cargo test -p x\"",
        "tdd red --cmd \"cargo test --workspace --test y\"",
    ] {
        let env = Env::new();
        let r = repo();
        assert_deny(&hook(&env, "s", c, r.path()), c);
    }
}

// test-weakening-justified: backlog fb4f650d (2026-11-28) reverses this pin; a bare
// run must not block the RED proof the F->P oracle requires (mirror of 8f82f09d).
#[test]
fn b_tdd_red_after_bare_run_on_same_tree_is_allowed() {
    let env = Env::new();
    let r = repo();
    assert_allow(
        &hook_confirmed(&env, "s", "cargo test -p x --test t", r.path()),
        "first bare run",
    );
    assert_allow(
        &hook(
            &env,
            "s",
            "tdd red --cmd \"cargo test -p x --test t\"",
            r.path(),
        ),
        "tdd red after bare run, same tree",
    );
}

#[test]
fn b_bare_after_tdd_run_on_same_tree_is_denied() {
    let env = Env::new();
    let r = repo();
    let first = hook_confirmed(
        &env,
        "s",
        "tdd red --cmd \"cargo test -p x --test t\"",
        r.path(),
    );
    assert_allow(&first, "first tdd run");
    assert_deny(
        &hook(&env, "s", "cargo test -p x --test t", r.path()),
        "bare run after tdd red, same tree",
    );
}

#[test]
fn b_tdd_twice_on_same_tree_is_denied() {
    let env = Env::new();
    let r = repo();
    let c = "tdd green --cmd=\"cargo test -p x --test t\"";
    assert_allow(&hook_confirmed(&env, "s", c, r.path()), "first tdd green");
    assert_deny(&hook(&env, "s", c, r.path()), "tdd green twice, same tree");
}

#[test]
fn b_control_tdd_on_another_tree_is_allowed() {
    let env = Env::new();
    let r1 = repo();
    let r2 = repo_with(&[("b.txt", "other\n")]);
    assert_allow(
        &hook(&env, "s", "cargo test -p x --test t", r1.path()),
        "bare run in tree 1",
    );
    assert_allow(
        &hook(
            &env,
            "s",
            "tdd red --cmd \"cargo test -p x --test t\"",
            r2.path(),
        ),
        "tdd red in tree 2",
    );
}

/// A fake `cargo` on PATH that appends one line per invocation to `counter`.
fn fake_cargo(dir: &Path, counter: &Path) {
    let p = dir.join("cargo");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n",
            counter.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn invocations(counter: &Path) -> usize {
    std::fs::read_to_string(counter)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

struct StateTestFx {
    env: Env,
    repo: TempDir,
    bin: TempDir,
    counter: PathBuf,
}

fn state_test_fixture() -> StateTestFx {
    let env = Env::new();
    let r = repo();
    let cfg_dir = env.home.path().join(".condukt");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[test]\ncommand = \"cargo test -p x --test t\"\n",
    )
    .unwrap();
    init_run(
        &env,
        r.path(),
        "runT",
        &[("t1", "a.txt", "cargo test -p x --test t")],
    );
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count.txt");
    fake_cargo(bin.path(), &counter);
    StateTestFx {
        env,
        repo: r,
        bin,
        counter,
    }
}

impl StateTestFx {
    fn state_test(&self) -> Output {
        condukt(
            &self.env,
            self.repo.path(),
            Some("s"),
            Some(self.bin.path()),
            &["state", "test", "--run", "runT"],
        )
    }
}

#[test]
fn b_state_test_twice_on_same_tree_is_refused() {
    let fx = state_test_fixture();
    let first = fx.state_test();
    assert!(first.status.success(), "first state test: {first:?}");
    assert_eq!(
        invocations(&fx.counter),
        1,
        "first state test runs cargo once"
    );
    let second = fx.state_test();
    assert!(
        !second.status.success(),
        "second state test on the same tree must be refused: {second:?}"
    );
    assert_eq!(
        invocations(&fx.counter),
        1,
        "a refused state test must not run the test command"
    );
}

#[test]
fn b_state_test_after_hook_run_on_same_tree_is_refused() {
    let fx = state_test_fixture();
    assert_allow(
        &hook(&fx.env, "s", "cargo test -p x --test t", fx.repo.path()),
        "bare run via hook",
    );
    let out = fx.state_test();
    assert!(
        !out.status.success(),
        "state test of a command the hook already recorded on this tree must be refused: {out:?}"
    );
    assert_eq!(invocations(&fx.counter), 0, "refused: cargo must not run");
}

#[test]
fn b_control_first_state_test_runs() {
    let fx = state_test_fixture();
    let out = fx.state_test();
    assert!(out.status.success(), "first state test: {out:?}");
    assert_eq!(invocations(&fx.counter), 1);
}

// ── C: inside a task worktree, only the task's tests ────────────────────────

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

#[test]
fn c_unrelated_existing_test_in_task_worktree_is_denied() {
    let fx = c_default();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x --test old_t", &fx.wt),
        "--test old_t (pre-existing, not in reproduction_tests) in task worktree",
    );
}

#[test]
fn c_unrelated_test_via_tdd_in_task_worktree_is_denied() {
    let fx = c_default();
    assert_deny(
        &hook(
            &fx.env,
            "s",
            "tdd red --cmd \"cargo test -p x --test old_t\"",
            &fx.wt,
        ),
        "tdd --cmd --test old_t in task worktree",
    );
}

#[test]
fn c_unrelated_test_from_worktree_subdir_is_denied() {
    let fx = c_default();
    let sub = fx.wt.join("src");
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x --test old_t", &sub),
        "--test old_t from a subdir of the task worktree",
    );
}

#[test]
fn c_mixed_allowed_and_unrelated_tests_is_denied() {
    let fx = c_default();
    assert_deny(
        &hook(
            &fx.env,
            "s",
            "cargo test -p x --test new_untracked --test old_t",
            &fx.wt,
        ),
        "one allowed --test plus one unrelated --test",
    );
}

#[test]
fn c_lib_selector_not_in_reproduction_tests_is_denied() {
    let fx = c_default();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x --lib foo", &fx.wt),
        "--lib when reproduction_tests names only --test repro_t",
    );
}

#[test]
fn c_cwd_matching_two_tasks_is_denied() {
    let fx = wt_fixture("cargo test -p x --test repro_t", &["t1", "t2"]);
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x --test new_untracked", &fx.wt),
        "task worktree recorded on two tasks (undetermined)",
    );
}

#[test]
fn c_unreadable_run_state_is_denied() {
    let fx = c_default();
    std::fs::write(run_state_file(&fx.env, "runC"), "{ not json").unwrap();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x --test new_untracked", &fx.wt),
        "run state unreadable while cwd is under the condukt worktree base",
    );
}

#[test]
fn c_control_worktree_added_or_changed_tests_are_allowed() {
    let fx = c_default();
    for c in [
        "cargo test -p x --test new_committed",
        "cargo test -p x --test new_untracked",
        "cargo test -p x --test changed_t",
        "tdd red --cmd \"cargo test -p x --test new_untracked -- --nocapture\"",
    ] {
        assert_allow(&hook(&fx.env, "s", c, &fx.wt), c);
    }
}

#[test]
fn c_control_reproduction_test_is_allowed() {
    let fx = c_default();
    assert_allow(
        &hook(&fx.env, "s", "cargo test -p x --test repro_t", &fx.wt),
        "--test repro_t named in reproduction_tests",
    );
}

#[test]
fn c_control_lib_selector_named_in_reproduction_tests_is_allowed() {
    let fx = wt_fixture("cargo test -p x --lib foo", &["t1"]);
    assert_allow(
        &hook(&fx.env, "s", "cargo test -p x --lib foo", &fx.wt),
        "--lib named in reproduction_tests",
    );
}

#[test]
fn c_control_outside_task_worktree_c_does_not_apply() {
    let fx = c_default();
    assert_allow(
        &hook(&fx.env, "s", "cargo test -p x --test old_t", fx.repo.path()),
        "--test old_t in the main checkout (not a task worktree)",
    );
}

// ── C': the 2026-11-20 ruling, in a fixture where "the task's crate" is determinate ──
//
// Backlog 6f653d96. User ruling 2026-11-20 (verbatim):
// 「証明ができないというのであれば、それを証明するためのテストを実施できないのはtestscopeの不具合」
//
// The four `c_unrelated_*` / `c_mixed_*` pins above are left as they are: their
// fixture (`wt_fixture`) has no Cargo manifest, so `-p x` names no package, and
// its worktree changes only tests/*.rs files, no crate source. Whether `x` is
// "a crate the task worktree changed" is therefore undetermined there, and the
// ruling does not say which way that falls (CLAUDE.md §3 would refuse it).
// Instead each of those four rows is re-anchored here in a workspace fixture
// where it is determinate, twice: once for the crate the task changed (`x`,
// allowed by the ruling — RED until 6f653d96 is fixed) and once for a crate the
// task did not change (`y`, still denied — keeps the original "unrelated test
// is denied" intent alive; GREEN now).
//
// DOES NOT PROVE: how a fix resolves `-p <name>` to a package (manifest read vs
// cargo metadata); the manifest-less case above; `--lib`/`--bin`/`--doc`; that
// the same-tree rerun refusal is unaffected (rule 3; not exercised here).

/// Workspace repo on main with crates/x (src/lib.rs, tests/old_t.rs,
/// tests/repro_t.rs) and crates/y (src/lib.rs, tests/y_t.rs). Run `runK`, task
/// `t1` touching crates/x/src/lib.rs with reproduction_tests `cargo test -p x
/// --test repro_t`. The task worktree commits a change to crates/x/src/lib.rs
/// and leaves crates/x/tests/new_untracked.rs untracked; crates/y is untouched.
fn crates_fixture() -> WtFx {
    let env = Env::new();
    let r = repo_with(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/x\", \"crates/y\"]\n",
        ),
        (
            "crates/x/Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/x/src/lib.rs", "pub fn f() {}\n"),
        ("crates/x/tests/old_t.rs", "#[test] fn o() {}\n"),
        ("crates/x/tests/repro_t.rs", "#[test] fn r() {}\n"),
        (
            "crates/y/Cargo.toml",
            "[package]\nname = \"y\"\nversion = \"0.1.0\"\n",
        ),
        ("crates/y/src/lib.rs", "pub fn g() {}\n"),
        ("crates/y/tests/y_t.rs", "#[test] fn y() {}\n"),
    ]);
    init_run(
        &env,
        r.path(),
        "runK",
        &[(
            "t1",
            "crates/x/src/lib.rs",
            "cargo test -p x --test repro_t",
        )],
    );
    let out = condukt(
        &env,
        r.path(),
        Some(SESSION_C),
        None,
        &[
            "worktree", "create", "--run", "runK", "--topic", "t1", "--branch", "b1",
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
    let out = condukt(
        &env,
        r.path(),
        Some(SESSION_C),
        None,
        &[
            "state",
            "set",
            "--run",
            "runK",
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
        wt.join("crates/x/src/lib.rs"),
        "pub fn f() { let _ = 1; }\n",
    )
    .unwrap();
    git(&wt, &["add", "crates/x/src/lib.rs"]);
    git(&wt, &["commit", "-q", "-m", "fix x"]);
    std::fs::write(
        wt.join("crates/x/tests/new_untracked.rs"),
        "#[test] fn u() {}\n",
    )
    .unwrap();
    WtFx { env, repo: r, wt }
}

// Re-anchor of `c_unrelated_existing_test_in_task_worktree_is_denied` (6f653d96,
// ruling 2026-11-20 quoted above): an existing test of the changed crate runs.
#[test]
fn c_existing_test_of_changed_crate_in_task_worktree_is_allowed() {
    let fx = crates_fixture();
    assert_allow(
        &hook(&fx.env, "s", "cargo test -p x --test old_t", &fx.wt),
        "--test old_t, an existing test of crate x which the task worktree changed",
    );
}

// Intent of `c_unrelated_existing_test_in_task_worktree_is_denied`, kept.
#[test]
fn c_existing_test_of_untouched_crate_in_task_worktree_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p y --test y_t", &fx.wt),
        "--test y_t of crate y, which the task worktree did not change",
    );
}

// Re-anchor of `c_unrelated_test_via_tdd_in_task_worktree_is_denied` (6f653d96,
// ruling 2026-11-20 quoted above): the tdd --cmd route follows the bare route.
#[test]
fn c_existing_test_of_changed_crate_via_tdd_in_task_worktree_is_allowed() {
    let fx = crates_fixture();
    assert_allow(
        &hook(
            &fx.env,
            "s",
            "tdd red --cmd \"cargo test -p x --test old_t\"",
            &fx.wt,
        ),
        "tdd --cmd --test old_t of changed crate x in task worktree",
    );
}

// Intent of `c_unrelated_test_via_tdd_in_task_worktree_is_denied`, kept.
#[test]
fn c_existing_test_of_untouched_crate_via_tdd_in_task_worktree_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(
            &fx.env,
            "s",
            "tdd red --cmd \"cargo test -p y --test y_t\"",
            &fx.wt,
        ),
        "tdd --cmd --test y_t of untouched crate y in task worktree",
    );
}

// Re-anchor of `c_unrelated_test_from_worktree_subdir_is_denied` (6f653d96,
// ruling 2026-11-20 quoted above): same answer from a subdir of the worktree.
#[test]
fn c_existing_test_of_changed_crate_from_worktree_subdir_is_allowed() {
    let fx = crates_fixture();
    let sub = fx.wt.join("crates/x/src");
    assert_allow(
        &hook(&fx.env, "s", "cargo test -p x --test old_t", &sub),
        "--test old_t of changed crate x from a subdir of the task worktree",
    );
}

// Intent of `c_unrelated_test_from_worktree_subdir_is_denied`, kept.
#[test]
fn c_existing_test_of_untouched_crate_from_worktree_subdir_is_denied() {
    let fx = crates_fixture();
    let sub = fx.wt.join("crates/x/src");
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p y --test y_t", &sub),
        "--test y_t of untouched crate y from a subdir of the task worktree",
    );
}

// Re-anchor of `c_mixed_allowed_and_unrelated_tests_is_denied` (6f653d96,
// ruling 2026-11-20 quoted above): a worktree-added test plus an existing test
// of the same changed crate runs.
#[test]
fn c_mixed_added_and_existing_test_of_changed_crate_is_allowed() {
    let fx = crates_fixture();
    assert_allow(
        &hook(
            &fx.env,
            "s",
            "cargo test -p x --test new_untracked --test old_t",
            &fx.wt,
        ),
        "worktree-added --test new_untracked plus existing --test old_t, both of changed crate x",
    );
}

// Intent of `c_mixed_allowed_and_unrelated_tests_is_denied`, kept: one allowed
// selector does not admit a test of an untouched crate in the same command.
#[test]
fn c_mixed_allowed_and_untouched_crate_tests_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(
            &fx.env,
            "s",
            "cargo test -p x --test new_untracked -p y --test y_t",
            &fx.wt,
        ),
        "allowed --test new_untracked of x plus --test y_t of untouched crate y",
    );
}

// ── C'': fail-opens of the changed-crate branch (skeptic review of 1a0381494) ──
//
// Skeptic finding (1), as relayed: "Clustered short flags hide a package:
// `cargo test -qp blastguard -p <changed-crate> --test <t>` — package_specs does
// not see `-qp blastguard`, so only the changed crate is checked and the command
// is allowed, while cargo selects blastguard too."
// Premise observed (cargo 1.97.1, 2026-11-21) in a scratch package `probe`:
//   `cargo test -qp nosuchzz --test aa`  -> "error: package ID specification
//     `nosuchzz` did not match any packages" (exit 101)
//   `cargo test -qpnosuchzz --test aa`   -> same error, exit 101
//   `cargo pkgid -vp condukt`            -> resolves condukt, exit 0
// i.e. cargo parses `-qp <spec>`, `-qp<spec>` and `-vp <spec>` as `-q/-v -p <spec>`.
//
// Skeptic finding (2), as relayed: "`cargo test -p <changed-crate> --test '*'`
// is allowed by the new branch, but cargo expands the glob to every
// integration test of the crate (≈ `--tests`, which testscope denies as a full
// suite)."
// Premise observed: cargo-test(1) man page, Target Selection: "Note that
// --bin, --example, --test and --bench flags also support common Unix glob
// patterns like *, ? and []." and in the scratch package
// `cargo test -p probe --test '*'` ran both tests/aa.rs and tests/bb.rs.
//
// Expected (CLAUDE.md §3): both shapes are refused — a `--test` value that is
// not a plain test name, or a short-flag cluster that may carry `p`, does not
// reach allow.
//
// DOES NOT PROVE: which fix shape is used (parse clusters vs refuse them); other
// short flags cargo might cluster with `p` beyond `-q`/`-v`; `--bin`/`--example`
// globs (already denied as Kind selectors outside reproduction_tests); that a
// glob named in reproduction_tests is handled any particular way.

#[test]
fn c_clustered_qp_untouched_crate_beside_changed_crate_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -qp y -p x --test old_t", &fx.wt),
        "`-qp y` selects untouched crate y beside changed crate x",
    );
}

#[test]
fn c_clustered_qp_attached_untouched_crate_beside_changed_crate_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -qpy -p x --test old_t", &fx.wt),
        "`-qpy` selects untouched crate y beside changed crate x",
    );
}

#[test]
fn c_clustered_vp_untouched_crate_beside_changed_crate_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -vp y -p x --test old_t", &fx.wt),
        "`-vp y` selects untouched crate y beside changed crate x",
    );
}

#[test]
fn c_test_star_glob_of_changed_crate_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x --test '*'", &fx.wt),
        "`--test '*'` expands to every integration test of crate x",
    );
}

#[test]
fn c_test_eq_question_glob_of_changed_crate_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x '--test=old_?'", &fx.wt),
        "`--test=old_?` is a glob, not a plain test name",
    );
}

#[test]
fn c_test_bracket_glob_of_changed_crate_is_denied() {
    let fx = crates_fixture();
    assert_deny(
        &hook(&fx.env, "s", "cargo test -p x --test '[or]*'", &fx.wt),
        "`--test '[or]*'` is a glob matching old_t and repro_t",
    );
}
