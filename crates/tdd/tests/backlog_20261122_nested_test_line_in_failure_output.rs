#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: `tdd red` records a `test <name> ... FAILED` line that sits INSIDE a
//! failing test's captured output (libtest's `failures:` section) as if it
//! were a real per-test result. The phantom name then lands in the GREEN
//! proof's `red_failed_tests`, and `condukt worktree merge` refuses the task:
//! "merge-proof-contract(f): t1.green.json: failed at RED but not \"pass\" at
//! GREEN: t".
//!
//! Observed 2026-11-22 in condukt run run-20261122-012059-29884 task t1
//! (backlog 7b30a4ed, worktree base 3e395b717): the RED run of
//! `cargo test -p backlog --test backlog_20261113_no_deferral_auto_close`
//! had two real failures whose panic messages embed a nested fixture's cargo
//! output containing `test t ... FAILED`; `.tdd/t1.green.json` holds
//! `red_failed_tests: [g_cargo_zero_tests_at_head_is_auto_cancelled,
//! g_control_cargo_compile_failure_at_head_refuses_claim, t]`.
//!
//! Shape: the command prints libtest-shaped output. RED: `test real ...
//! FAILED`, then a `failures:` section whose captured stdout contains
//! `test t ... FAILED`. GREEN: `test real ... ok`. Control: the real name is
//! recorded as `fail` at RED.
//!
//! Readings: (a) defect - lines inside the `failures:` / `---- <name> stdout
//! ----` capture are test output, not results; (b) intended - the parser
//! deliberately counts every matching line (least favourable wins) and the
//! merge contract should tolerate a RED-only name absent at GREEN. Tell apart:
//! libtest prints all result lines before `failures:`; whether the per-test
//! map is meant to be a libtest parse or a conservative line scan.
//!
//! DOES NOT PROVE: real cargo output; nextest/pytest formats; which side
//! (tdd parser or condukt merge contract) should change.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tdd(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("tdd runs")
}

fn git(cwd: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

const RUN: &str = r#"if [ -f impl.txt ]; then
  echo 'running 1 test'
  echo 'test real ... ok'
  echo ''
  echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'
  exit 0
fi
echo 'running 1 test'
echo 'test real ... FAILED'
echo ''
echo 'failures:'
echo ''
echo '---- real stdout ----'
echo 'nested fixture said:'
echo 'test t ... FAILED'
echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored'
echo ''
echo 'failures:'
echo '    real'
echo ''
echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'
exit 101
"#;

#[test]
#[ignore = "RED(867cd678): a test line inside a failure's captured output is recorded as a per-test result"]
fn nested_test_line_in_failure_capture_is_not_a_result() {
    let d = Dir(std::env::temp_dir().join(format!("tdd-nested-line-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@example.invalid"]);
    git(&work, &["config", "user.name", "t"]);
    std::fs::write(work.join("run.sh"), RUN).unwrap();
    git(&work, &["add", "run.sh"]);
    git(&work, &["commit", "-q", "--no-verify", "-m", "init"]);

    let cmd = "sh run.sh";
    let red = tdd(&home, &work, &["red", "--task", "t1", "--cmd", cmd]);
    assert_eq!(red.status.code(), Some(0), "control: RED recorded");
    let red_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(work.join(".tdd/t1.red.json")).unwrap())
            .unwrap();
    assert_eq!(
        red_json["tests"]["real"], "fail",
        "control: the real failure is recorded: {red_json}"
    );
    assert!(
        red_json["tests"].get("t").is_none(),
        "the `test t ... FAILED` line inside `---- real stdout ----` was recorded as a test result: tests = {}",
        red_json["tests"]
    );
}
