#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED: for a run log with NO `running N tests` line (unframed),
//! `crates/tdd/src/evidence.rs` `parse_test_log` sums EVERY `test result:` line
//! (`scan_conservative`) and reports the sum as `counts.determined: true`. A
//! nested `test result:` printed inside a failing test's captured output thus
//! inflates the recorded counts while they are reported as determined. The
//! framed path reports `determined: false` in the analogous case ("a nested
//! `test result:` could have inflated the sum, so the pass count is unknown",
//! `parse_test_log` docstring). Pinned today by the unit test
//! `unframed_log_keeps_the_conservative_scan` (asserts `failed == 2` from a
//! duplicated result line, `determined == true`).
//!
//! Observed 2026-11-22 by static reading at main 6ae161ee7 after condukt run
//! run-20261122-020202-63344 (which changed crates/tdd/src/evidence.rs).
//!
//! Shape: the command prints `test real ... FAILED`, a captured block holding a
//! nested `test result: FAILED. 0 passed; 1 failed; 0 ignored`, then the real
//! `test result: FAILED. 0 passed; 1 failed; ...` -- and no `running` line.
//! Passing means: the recorded RED proof's `counts` is either
//! `determined: false`, or determined with `failed == 1` (the one real
//! failure).
//!
//! Readings: (a) defect -- an inflated sum reported as determined (a count
//! claimed as fact that was not observed); (b) intended -- an unframed log is
//! not libtest's pretty format, so the conservative sum is the documented
//! reading ("A log with no `running` header at all ... is read by the
//! conservative scan for both") and inflation is the restrictive direction.
//! Tell apart: whether `determined: true` is meant to assert "these counts are
//! the run's counts" or only "some result line parsed"; whether any consumer
//! reads `counts.passed`/`failed` as exact.
//!
//! DOES NOT PROVE: that real cargo/libtest output ever lacks the `running`
//! line; that any consumer (condukt merge contract, donegate) acts on the
//! inflated number; nextest/pytest formats; which side should change.

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

const RUN: &str = r#"echo 'test real ... FAILED'
echo ''
echo 'failures:'
echo ''
echo '---- real stdout ----'
echo 'nested fixture said:'
echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored'
echo ''
echo 'failures:'
echo '    real'
echo ''
echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'
exit 101
"#;

#[test]
#[ignore = "RED: unframed log sums a nested test result line and reports it as determined"]
fn unframed_log_nested_result_line_is_not_a_determined_inflated_count() {
    let d = Dir(std::env::temp_dir().join(format!("tdd-unframed-counts-{}", std::process::id())));
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

    let red = tdd(&home, &work, &["red", "--task", "t1", "--cmd", "sh run.sh"]);
    assert_eq!(
        red.status.code(),
        Some(0),
        "control: RED recorded; stderr: {}",
        String::from_utf8_lossy(&red.stderr)
    );
    let red_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(work.join(".tdd/t1.red.json")).unwrap())
            .unwrap();
    assert_eq!(
        red_json["tests"]["real"], "fail",
        "control: the real failure is recorded: {red_json}"
    );
    let counts = &red_json["counts"];
    assert!(
        counts.is_object(),
        "control: the proof carries counts: {red_json}"
    );
    if counts["determined"] == serde_json::json!(true) {
        assert_eq!(
            counts["failed"],
            serde_json::json!(1),
            "an unframed log's nested `test result:` line was summed and reported as determined: counts = {counts}"
        );
    }
}
