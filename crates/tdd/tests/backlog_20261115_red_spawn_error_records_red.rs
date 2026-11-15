#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (tdd defect candidate, observed 2026-11-15 by a verifier agent reading
//! base a8e92b562 + run-20261115-212745-8268 task a worktree):
//! `crates/tdd/src/proof.rs:41-49` `judge_red(passed)` records RED whenever
//! `passed == false` and never looks at `spawn_error`. So when the test command
//! never ran — spawn failure, or the run log cannot be opened
//! (`crates/harness-core/src/gate/runner.rs:103-111` `spawn_failed` returns
//! `passed:false, exit_code:None, spawn_error:Some(..)`) — `tdd red` writes a
//! valid RED proof although no test ran.
//!
//! Two readings:
//!  (1) real defect: a RED proof means "the test was observed to fail"; a
//!      command that never ran observed nothing, so `tdd red` must refuse
//!      (non-zero exit, no `<task>.red.json`). The F->P oracle is fail-open
//!      otherwise (CLAUDE.md §3: undeterminable must not resolve to RED).
//!  (2) intended: spawn failure counted as RED deliberately. Tell apart: look
//!      for a documented decision/spec saying so (proof.rs docs say "REQUIRES
//!      them to fail (>=1 red)"; none found mentioning spawn errors) — if the
//!      owner rules (2), delete the ignore and invert this test, citing the
//!      ruling.
//!
//! Method: a regular FILE is placed at `<HOME>/.tdd/state`, so
//! `<HOME>/.tdd/state/tmp/tdd-run.log` cannot be created and the command is not
//! run. Evidence the command did not run is a marker file the command would
//! create (`ran.marker`): asserted absent. Control: the same command with a
//! usable state dir DOES record RED and DOES run (marker present), so the test
//! is not vacuous.
//!
//! DOES NOT PROVE: the real `Command::spawn` failure path (ENOENT shell, which
//! `judge_red` ignores identically but is not exercised here), the `tdd green`
//! path, or a project `state_dir` override. The log-open failure is one
//! deterministic trigger of the shared `spawn_error` branch.
//!
//! RED written by an independent test author, not the implementer.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_dir(tag: &str) -> Dir {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tdd-spawnerr-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::create_dir_all(dir.join("work")).unwrap();
    Dir(dir)
}

const CMD: &str = "touch ran.marker; exit 1";

fn run_red(home: &Path, cwd: &Path) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(["red", "--task", "t1", "--cmd", CMD])
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("tdd runs")
}

#[test]
#[ignore = "RED: 2026-11-15 proof.rs judge_red ignores spawn_error, so `tdd red` records RED when the test command never ran — remove the ignore when fixed"]
fn red_is_not_recorded_when_the_command_never_ran() {
    let d = unique_dir("spawnerr");
    let home = d.0.join("home");
    let work = d.0.join("work");
    // regular file where the state dir must be: log file cannot be created.
    std::fs::create_dir_all(home.join(".tdd")).unwrap();
    std::fs::write(home.join(".tdd").join("state"), "not a dir").unwrap();

    let out = run_red(&home, &work);
    let proof = work.join(".tdd").join("t1.red.json");
    let ran = work.join("ran.marker").exists();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!ran, "setup invalid: the command actually ran");
    assert!(
        !proof.exists() && out.status.code() != Some(0),
        "command never ran (log could not be opened) yet tdd red exit={:?} and RED proof exists={}\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code(),
        proof.exists()
    );
}

#[test]
fn control_failing_command_that_ran_records_red() {
    let d = unique_dir("ctl");
    let home = d.0.join("home");
    let work = d.0.join("work");
    let out = run_red(&home, &work);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(work.join("ran.marker").exists(), "command must have run");
    assert!(work.join(".tdd").join("t1.red.json").exists());
}
