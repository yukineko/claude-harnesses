#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-22, main 5e99d94d4): when `tdd green` finds the tests
//! still failing it prints only "tests still failing — keep implementing ..."
//! and keeps NOTHING of the run: `runner::run_cmd` (crates/tdd/src/runner.rs)
//! parses the run log and then removes it, and `green()`
//! (crates/tdd/src/proof.rs) bails in `judge_green` before any artifact is
//! written, so `Outcome.output_tail` / `Outcome.tests` are dropped. The caller
//! cannot see which test failed or why (reported by the t1 worker of condukt
//! run run-20261122-035502-88763).
//!
//! Method: RED with a fake runner whose failing test is `alpha_red`; then
//! GREEN with a run that still fails, on a test name assembled at run time
//! (so the name exists only in that run's output, not in the script). After
//! the refused GREEN, the name must appear in tdd's stdout/stderr or in some
//! file under the test's HOME / work dir (a `.tdd` / state record).
//!
//! Readings: (a) defect — a refused GREEN leaves no observable evidence of
//! the failure (CLAUDE.md §4/§6: what failed must stay visible); (b) intended —
//! tdd only judges, and the caller is expected to re-run the command itself
//! to see the output. Tell apart: whether a spec/README of tdd states that a
//! refused GREEN keeps no output (none found at this rev).
//!
//! DOES NOT PROVE: behaviour for RED, timeouts or spawn errors; the format
//! the output should be kept in.

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

fn files_containing(root: &Path, needle: &str, skip: &Path, hits: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p == skip {
            continue;
        }
        if p.is_dir() {
            files_containing(&p, needle, skip, hits);
        } else if std::fs::read(&p)
            .map(|b| String::from_utf8_lossy(&b).contains(needle))
            .unwrap_or(false)
        {
            hits.push(p);
        }
    }
}

#[test]
fn refused_green_keeps_the_failing_test_output() {
    let d = Dir(std::env::temp_dir().join(format!("tdd-green-fail-out-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&d.0);
    let home = d.0.join("home");
    let work = d.0.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();

    let script = work.join("fake-cargo-test.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         if [ ! -f green.flag ]; then\n\
         printf 'running 1 test\\ntest alpha_red ... FAILED\\n\\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\\n\\n'\n\
         exit 101\n\
         fi\n\
         n=zz_green; n=${n}_only_marker\n\
         printf 'running 1 test\\ntest %s ... FAILED\\n\\nfailures:\\n    %s\\n\\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\\n\\n' \"$n\" \"$n\"\n\
         exit 101\n",
    )
    .unwrap();
    let cmd = "sh ./fake-cargo-test.sh";
    let marker = "zz_green_only_marker";

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
    assert_ne!(
        out.status.code(),
        Some(0),
        "control: GREEN on a still-failing run must be refused.\nstdout: {stdout}\nstderr: {stderr}"
    );

    let mut hits = Vec::new();
    files_containing(&d.0, marker, &script, &mut hits);
    assert!(
        stdout.contains(marker) || stderr.contains(marker) || !hits.is_empty(),
        "refused tdd green kept no trace of the failing test `{marker}` \
         (not in stdout/stderr, no file under {} holds it).\nstdout: {stdout}\nstderr: {stderr}",
        d.0.display()
    );
}
