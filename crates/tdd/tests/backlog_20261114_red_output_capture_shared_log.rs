#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (tdd defect candidate, observed 2026-11-14 in run-20261114-111444-69657):
//! `tdd red --task X --cmd "<cargo test ...>"` in worktree A wrote a
//! `.tdd/X.red.json` whose `output_tail` contained the cargo output of a
//! DIFFERENT worktree B running at the same time ("Compiling blastguard
//! v0.2.568 (/Users/yuki/.condukt/worktrees/...-f9861c4e/...)", another
//! test's "Running tests/...").
//!
//! Cause located in code (base b0b4ac6e3): `crates/tdd/src/runner.rs`
//! `run_cmd` uses `tmp_dir.join("tdd-run.log")`, and `proof::red` /
//! `proof::green` pass `cfg.state_dir.join("tmp")`, where `state_dir`
//! defaults to `$HOME/.tdd/state` — one fixed path per HOME, not per
//! root/task/pid. `harness_core::gate::runner::run` `File::create`s it
//! (truncating the other run's file in place — same inode), points the child's
//! stdout/stderr at it, then reads the tail back from the path. Two concurrent
//! runs therefore write into, truncate, and read back one shared file.
//!
//! Asserted: two `tdd red` runs started concurrently under one HOME but in two
//! different cwds each record only their own output in `output_tail`.
//! Control: a single `tdd red` run records its own markers.
//!
//! DOES NOT PROVE: the `tdd green` path (same helper, not exercised here),
//! behaviour with a project `state_dir` override, or that cargo itself did not
//! also interleave output via a shared target dir (CARGO_TARGET_DIR was unset
//! in the run; this test uses no cargo at all). Timing-based: B is started
//! 0.5s after A so that B's create truncates a file A is already writing.
//!
//! RED written by an independent test author, not the implementer.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_dir(tag: &str) -> Dir {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "tdd-sharedlog-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    Dir(dir)
}

fn spawn_red(home: &Path, cwd: &Path, task: &str, cmd: &str) -> Child {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(["red", "--task", task, "--cmd", cmd])
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tdd spawns")
}

fn finish(child: Child, cwd: &Path, task: &str) -> String {
    let out = child.wait_with_output().expect("tdd runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "tdd red must record the RED (cmd exits 1); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let proof = cwd.join(".tdd").join(format!("{task}.red.json"));
    let text = std::fs::read_to_string(&proof)
        .unwrap_or_else(|e| panic!("RED proof {} unreadable: {e}", proof.display()));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["output_tail"].as_str().unwrap_or_default().to_string()
}

const A_CMD: &str = "echo AAA-START; sleep 2; echo AAA-END; exit 1";
const B_CMD: &str = "sleep 1; echo BBB-ONLY; exit 1";

#[test]
#[ignore = "RED: 2026-11-14 concurrent tdd red runs share $HOME/.tdd/state/tmp/tdd-run.log so output_tail mixes worktrees — remove the ignore when fixed"]
fn concurrent_red_runs_in_different_roots_do_not_mix_output_tail() {
    let d = unique_dir("red");
    let home = d.0.join("home");
    let (a, b) = (d.0.join("a"), d.0.join("b"));
    let ca = spawn_red(&home, &a, "ta", A_CMD);
    std::thread::sleep(Duration::from_millis(500));
    let cb = spawn_red(&home, &b, "tb", B_CMD);
    let tail_b = finish(cb, &b, "tb");
    let tail_a = finish(ca, &a, "ta");

    let mut bad = Vec::new();
    if tail_a.contains("BBB") {
        bad.push(format!("A's output_tail contains B's output: {tail_a:?}"));
    }
    if !(tail_a.contains("AAA-START") && tail_a.contains("AAA-END")) {
        bad.push(format!("A's output_tail lost its own output: {tail_a:?}"));
    }
    if tail_b.contains("AAA") {
        bad.push(format!("B's output_tail contains A's output: {tail_b:?}"));
    }
    if !tail_b.contains("BBB-ONLY") {
        bad.push(format!("B's output_tail lost its own output: {tail_b:?}"));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn control_single_red_run_records_its_own_output() {
    let d = unique_dir("ctl");
    let home = d.0.join("home");
    let a = d.0.join("a");
    let tail = finish(
        spawn_red(&home, &a, "ta", "echo AAA-START; echo AAA-END; exit 1"),
        &a,
        "ta",
    );
    assert!(
        tail.contains("AAA-START") && tail.contains("AAA-END"),
        "single run must capture its own output: {tail:?}"
    );
}
