#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED-with-history repros (testscope rule 3 rerun ledger). Each route below
//! LOOKS like a leading `cd X` but bash does not actually move cargo into X, so
//! the test runs in the payload cwd P. The ledger must therefore not key the
//! command on X (a fresh, never-run repo): that would let an identical test
//! rerun on an unchanged P tree ("ledger dodge").
//!
//! Shape of every route test: (1) `cargo test -p foo --test bar` in P, no cd ->
//! allowed; (2) from P the route command with a FRESH repo X -> must be DENIED
//! (undetermined-directory deny or rerun refused; both are `"deny"`).
//!
//! History (RED observed at the rev before each fix, GREEN at HEAD):
//!   R1 `cd X &>/dev/null | cargo ...`        fixed by 47571a2a2 (RED at 795b6699b)
//!   R2 `env cd X && cargo ...`               fixed by b558ed1b5 (RED at 47571a2a2)
//!   R3 `command VAR=1 cd X; cargo ...`       fixed by 01cd61217 (RED at b558ed1b5)
//!
//! DOES NOT PROVE: which of the two deny reasons fires (only that the command is
//! denied); that other cd-lookalike routes (pushd, subshell, `git -C`, builtin
//! wrappers not listed) are handled; or that the denial is not also produced by
//! something unrelated to the route form — the control only shows a plain
//! `cd X && ...` from P is allowed, so the deny is attributable to the route
//! form rather than to any cd.

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
    git(d.path(), &["add", "a.txt"]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn run(home: &Path, ledger: &Path, cmd: &str, cwd: &Path) -> (Option<i32>, String) {
    let payload = serde_json::json!({
        "session_id": "s",
        "tool_name": "Bash",
        "tool_input": {"command": cmd},
        "cwd": cwd.to_str().unwrap(),
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg("testscope")
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
    fn go(&self, cmd: &str) -> (Option<i32>, String) {
        run(self.home.path(), &self.ledger(), cmd, self.p.path())
    }
    /// Seed the ledger with a plain run in P, then run `route` (built from X).
    fn route(&self, route: &str) -> (Option<i32>, String) {
        let first = self.go(PLAIN);
        assert!(
            !denied(&first),
            "first plain run in P must be allowed: {first:?}"
        );
        let second = self.go(route);
        eprintln!("route {route:?} -> {second:?}");
        second
    }
}

#[test]
fn r1_cd_with_amp_redirect_piped_into_cargo_is_denied() {
    let fx = Fx::new();
    let route = format!("cd {} &>/dev/null | {PLAIN}", fx.x.path().display());
    let res = fx.route(&route);
    assert!(
        denied(&res),
        "route did not move cargo into X yet was allowed: {res:?}"
    );
}

#[test]
fn r2_env_cd_then_cargo_is_denied() {
    let fx = Fx::new();
    let route = format!("env cd {} && {PLAIN}", fx.x.path().display());
    let res = fx.route(&route);
    assert!(
        denied(&res),
        "route did not move cargo into X yet was allowed: {res:?}"
    );
}

#[test]
fn r3_command_assignment_cd_then_cargo_is_denied() {
    let fx = Fx::new();
    let route = format!("command VAR=1 cd {}; {PLAIN}", fx.x.path().display());
    let res = fx.route(&route);
    assert!(
        denied(&res),
        "route did not move cargo into X yet was allowed: {res:?}"
    );
}

// Control: a real `cd X &&` from P with a fresh X after the P run is allowed.
#[test]
fn control_plain_cd_and_then_cargo_into_fresh_tree_is_allowed() {
    let fx = Fx::new();
    let route = format!("cd {} && {PLAIN}", fx.x.path().display());
    let res = fx.route(&route);
    assert!(
        !denied(&res),
        "plain cd into fresh tree must be allowed: {res:?}"
    );
}
