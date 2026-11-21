#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reviewer suspicions (static reading, unconfirmed) turned into tests for the
//! testscope rule 3 rerun ledger.
//!
//! A-series: forms where bash DOES move cargo into X before the test but testscope
//! may not recognise the cd (then the ledger keys on payload cwd P, and a test
//! already run in X reruns unchanged = ledger dodge). Shape: (1) from P run
//! `cd X && cargo test -p foo --test bar` -> allowed; (2) from P run form F (same
//! test, X unchanged) -> must be DENIED (rerun refused or undetermined; both
//! surface as `"deny"`).
//!   A3 assumption (NOT verified here): in bash `time` is a keyword and
//!   `time cd X` runs the cd builtin in the current shell.
//!
//! B-series: payload cwd = X, run the test (allowed), then from the same payload
//! cwd run `cd X/sub && cargo test -p foo --test bar` (same tree, nothing edited)
//! -> must be DENIED (same tree must collide). B has an untracked file at the repo
//! top; B-control has none.
//!
//! DOES NOT PROVE: that the ledger key itself is (top, tree, cmd) of X (only the
//! deny outcome is observed, not which reason fired or the ledger contents); that
//! bash really executes each form A1..A5 as described (not run under bash here);
//! that forms not listed (pushd, subshell, `git -C`, `cd -`, `$(cd X; ...)`) are
//! handled; that a deny is not caused by something unrelated to the form (A6 and
//! B-control are the only attribution controls).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    git(d.path(), &["init", "-q"]);
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    std::fs::write(d.path().join("sub").join("s.txt"), "s\n").unwrap();
    git(d.path(), &["add", "a.txt", "sub/s.txt"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn run(home: &Path, ledger: &Path, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    run_as("testscope", None, home, ledger, cmd, cwd)
}

/// `run`, then (when the hook allowed it, i.e. the command ran) the
/// PostToolUse that confirms the run (spec testscope-confirmed-run R2 / C2:
/// input only, the asserts stay as they were).
fn run_confirmed(home: &Path, ledger: &Path, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    let res = run_as("testscope", Some("tu-1"), home, ledger, cmd, cwd);
    if res.1.trim().is_empty() {
        run_as("testscope-confirm", Some("tu-1"), home, ledger, cmd, cwd);
    }
    res
}

fn run_as(
    sub: &str,
    tuid: Option<&str>,
    home: &Path,
    ledger: &Path,
    cmd: &str,
    cwd: &Path,
) -> (Option<i32>, String) {
    let mut payload = serde_json::json!({
        "session_id": "s",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    if let Some(id) = tuid {
        payload["tool_use_id"] = serde_json::json!(id);
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg(sub)
        .env("HOME", home)
        .env("CONDUKT_TESTSCOPE_LEDGER", ledger)
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

fn denied(res: &(Option<i32>, String)) -> bool {
    res.1.contains("\"deny\"")
}

const PLAIN: &str = "cargo test -p foo --test bar";

struct Fx {
    home: TempDir,
    ledger_dir: TempDir,
    p: TempDir,
    x: TempDir,
}

impl Fx {
    fn new() -> Self {
        Fx {
            home: tempfile::tempdir().unwrap(),
            ledger_dir: tempfile::tempdir().unwrap(),
            p: repo(),
            x: repo(),
        }
    }
    fn ledger(&self) -> PathBuf {
        self.ledger_dir.path().join("ledger.tsv")
    }
    fn go(&self, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
        run(self.home.path(), &self.ledger(), cmd, cwd)
    }
    fn go_confirmed(&self, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
        run_confirmed(self.home.path(), &self.ledger(), cmd, cwd)
    }
    /// From P: run `cd X && PLAIN` (must be allowed), then `form` (X unchanged).
    fn a(&self, form: &str) -> (Option<i32>, String) {
        let seed = format!("cd {} && {PLAIN}", self.x.path().display());
        let first = self.go_confirmed(&seed, self.p.path());
        assert!(!denied(&first), "seed run in X must be allowed: {first:?}");
        let second = self.go(form, self.p.path());
        eprintln!("form {form:?} -> {second:?}");
        second
    }
}

fn assert_a(form_of: impl Fn(&str) -> String) {
    let fx = Fx::new();
    let x = fx.x.path().display().to_string();
    let form = form_of(&x);
    let res = fx.a(&form);
    assert!(
        denied(&res),
        "unchanged X rerun via {form:?} was allowed (ledger dodge): {res:?}"
    );
}

#[test]
fn a1_brace_group_cd_is_denied() {
    assert_a(|x| format!("{{ cd {x}; }} && {PLAIN}"));
}

#[test]
fn a2_if_cd_then_cargo_is_denied() {
    assert_a(|x| format!("if cd {x}; then {PLAIN}; fi"));
}

#[test]
fn a3_time_cd_is_denied() {
    assert_a(|x| format!("time cd {x} && {PLAIN}"));
}

#[test]
fn a4_builtin_cd_is_denied() {
    assert_a(|x| format!("builtin cd {x} && {PLAIN}"));
}

#[test]
fn a5_bash_c_cd_is_denied() {
    assert_a(|x| format!("bash -c 'cd {x} && {PLAIN}'"));
}

// Control: expected already denied.
#[test]
fn a6_cd_semicolon_cargo_is_denied() {
    assert_a(|x| format!("cd {x}; {PLAIN}"));
}

fn b(untracked: bool) {
    let fx = Fx::new();
    let x = fx.x.path();
    if untracked {
        std::fs::write(x.join("untracked.txt"), "u\n").unwrap();
    }
    let first = fx.go_confirmed(PLAIN, x);
    assert!(!denied(&first), "first run in X must be allowed: {first:?}");
    let cmd = format!("cd {} && {PLAIN}", x.join("sub").display());
    let second = fx.go(&cmd, x);
    eprintln!("sub route {cmd:?} -> {second:?}");
    assert!(
        denied(&second),
        "same tree via cd X/sub (untracked={untracked}) was allowed: {second:?}"
    );
}

#[test]
fn b_subdir_cd_same_tree_with_untracked_top_file_is_denied() {
    b(true);
}

#[test]
fn b_control_subdir_cd_same_tree_without_untracked_is_denied() {
    b(false);
}
