#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog filing, 2026-11-22, main 950c8cdce): `tdd green` on a command
//! whose output ends with a pytest summary (`==== 2 passed in 0.10s ====`,
//! exit 0) records `counts: {"determined": false, "reason": "no libtest `test
//! result:` line in the run log ..."}`. `condukt worktree merge` then refuses
//! the task with `merge-proof-contract(undetermined-proof): <task>.green.json:
//! counts.determined is not true`. Observed on run-20261122-052625-87637 task
//! 0afbb200 (repro `python3 -m pytest --runxfail scripts/test_scout_..._repro.py`,
//! output_tail ends "2 passed in 1.11s"). `backlog add --repro-test` allowlists
//! `pytest`, so a pytest repro is a sanctioned test kind that can never merge.
//!
//! Readings: (a) defect -- tdd does not parse pytest summaries, so a pytest
//! GREEN is a false-positive refusal; (b) intended fail-closed -- tdd counts are
//! libtest-only and pytest tasks need another integration path. No pytest
//! handling is declared in crates/tdd/src, its README, or the condukt README
//! "Merge proof contract" (searched 2026-11-22); this test asserts reading (a).
//!
//! DOES NOT PROVE: that real pytest output has exactly this shape (the stub
//! only echoes the summary line; pytest is not run); pytest failure counts or
//! per-test names.

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

#[test]
fn pytest_style_summary_gives_determined_counts() {
    let d = Dir(std::env::temp_dir().join(format!("tdd-pytest-counts-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@example.invalid"]);
    git(&work, &["config", "user.name", "t"]);
    // First run fails (RED) and leaves a marker; the second passes (GREEN).
    std::fs::write(
        work.join("run.sh"),
        "echo '==== test session starts ===='\nif [ -f seen ]; then\n  echo '============================== 2 passed in 0.10s ==============================='\n  exit 0\nfi\ntouch seen\necho '==== 1 failed in 0.10s ===='\nexit 1\n",
    )
    .unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "--no-verify", "-m", "init"]);

    let red = tdd(&home, &work, &["red", "--task", "t1", "--cmd", "sh run.sh"]);
    assert_eq!(
        red.status.code(),
        Some(0),
        "control: RED recorded; stderr: {}",
        String::from_utf8_lossy(&red.stderr)
    );
    let green = tdd(
        &home,
        &work,
        &["green", "--task", "t1", "--cmd", "sh run.sh"],
    );
    assert_eq!(
        green.status.code(),
        Some(0),
        "control: GREEN recorded; stderr: {}",
        String::from_utf8_lossy(&green.stderr)
    );
    let proof = work.join(".tdd/t1.green.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&proof).expect("green proof written"))
            .unwrap();
    assert_eq!(
        v["counts"]["determined"],
        serde_json::json!(true),
        "pytest-style `2 passed` summary left counts undetermined: counts = {}",
        v["counts"]
    );
    assert_eq!(
        v["counts"]["passed"],
        serde_json::json!(2),
        "{}",
        v["counts"]
    );
}
