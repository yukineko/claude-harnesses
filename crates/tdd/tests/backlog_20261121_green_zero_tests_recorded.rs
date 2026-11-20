#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-21): `tdd green` records GREEN for a test run that
//! executed ZERO tests.
//!
//! Observed in condukt run run-20261121-042409-20321 task t1 (tdd 0.1.100):
//! `tdd green --task t1 --cmd 'cargo test -p blastguard --test
//! backlog_20261120_596ef57d_requirements -- --ignored r_kill'` was run after
//! the test's `#[ignore]` had been removed, so cargo ran zero tests
//! (`running 0 tests ... test result: ok. 0 passed; 0 failed; 0 ignored;
//! 0 measured; 11 filtered out`). tdd printed "GREEN recorded" with counts
//! {passed: 0, failed: 0} and tests {}, and the F->P oracle then reported
//! fail_to_pass / valid_fp_oracle true. A GREEN that executed zero tests
//! proves nothing (CLAUDE.md §2(b), §3 "empty set is not clean").
//!
//! Method: record a real RED (`exit 1`), then run `tdd green` with a command
//! that prints the exact cargo/libtest zero-test output and exits 0.
//! Asserted: GREEN is refused (non-zero exit) and no GREEN proof is written.
//!
//! DOES NOT PROVE: behaviour for non-libtest runners, or the oracle side
//! (`tdd oracle` / `condukt state check-oracle`) given an existing 0-test GREEN.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(home: &Path, cwd: &Path, phase: &str, cmd: &str) -> Output {
    harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args([phase, "--task", "t1", "--cmd", cmd])
        .current_dir(cwd)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("tdd runs")
}

fn green_proofs(work: &Path) -> Vec<String> {
    std::fs::read_dir(work.join(".tdd"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("t1.green"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
#[ignore = "RED(<to be filled>): tdd green records GREEN for a run that executed zero tests"]
fn green_refuses_a_run_that_executed_zero_tests() {
    let d = Dir(std::env::temp_dir().join(format!("tdd-green-zero-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    // The command under test is identical for RED and GREEN (slot match), but
    // its behaviour flips on a marker file: first run fails, second run prints
    // the libtest zero-test summary and exits 0.
    let script = work.join("fake-cargo-test.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         if [ ! -f green.flag ]; then\n\
         printf 'running 1 test\\ntest r_kill ... FAILED\\n\\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 10 filtered out; finished in 0.01s\\n\\n'\n\
         exit 101\n\
         fi\n\
         printf '\\nrunning 0 tests\\n\\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 11 filtered out; finished in 0.00s\\n\\n'\n\
         exit 0\n",
    )
    .unwrap();
    let cmd = "sh ./fake-cargo-test.sh";

    let red = run(&home, &work, "red", cmd);
    assert_eq!(
        red.status.code(),
        Some(0),
        "control: RED must be recorded: {}",
        String::from_utf8_lossy(&red.stderr)
    );

    std::fs::write(work.join("green.flag"), "").unwrap();
    let out = run(&home, &work, "green", cmd);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let proofs = green_proofs(&work);
    assert!(
        out.status.code() != Some(0) && proofs.is_empty(),
        "tdd green recorded GREEN for a run that executed zero tests \
         (exit {:?}, green proofs {proofs:?}).\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );
}
