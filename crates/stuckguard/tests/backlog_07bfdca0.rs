#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 07bfdca0 (also covers a2c171dd): the unit test
//! `anchor::backlog_c397cd15::heartbeat_reaches_overwatch_that_is_only_in_the_plugin_cache`
//! (crates/stuckguard/src/anchor.rs) rewrites the process-wide `HOME`/`PATH`
//! WITHOUT taking the module's `ENV_LOCK` that serializes the other
//! `HOME`/`PATH`-mutating `anchor` tests (`with_env`). Under the default
//! parallel test runner they race and the heartbeat test fails intermittently
//! (measured 2026-10-15 at main e1aca620: 7/30 runs of the `anchor::` subset at
//! `--test-threads=16`).
//!
//! The claimed property is "the stuckguard unit suite is deterministic under
//! the parallel runner", so this drives the real unit-test binary at that
//! boundary: build it with cargo, run the `anchor::` subset repeatedly in
//! parallel, and require every run to pass. With a ~23% per-run failure rate,
//! 60 runs pass by chance with probability < 1e-6; once the test takes the
//! lock it passes every time.

use std::process::Command;

const RUNS: usize = 60;

fn unit_test_binary() -> String {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let out = Command::new(cargo)
        .args([
            "test",
            "-p",
            "stuckguard",
            "--bin",
            "stuckguard",
            "--no-run",
            "--message-format=json",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("spawn cargo");
    assert!(
        out.status.success(),
        "building the stuckguard unit-test binary failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["reason"] == "compiler-artifact" && v["profile"]["test"] == true)
        .filter_map(|v| v["executable"].as_str().map(str::to_string))
        .next_back()
        .expect("cargo reported no unit-test executable for stuckguard")
}

#[test]
#[ignore = "backlog 07bfdca0: RED repro"]
fn anchor_unit_tests_are_deterministic_under_the_parallel_runner() {
    let bin = unit_test_binary();
    for run in 1..=RUNS {
        let out = Command::new(&bin)
            .args(["anchor::", "--test-threads=16"])
            .output()
            .expect("run unit-test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("heartbeat_reaches_overwatch_that_is_only_in_the_plugin_cache"),
            "anti-vacuity: the filtered run must include the racing test: {stdout}"
        );
        assert!(
            out.status.success(),
            "run {run}/{RUNS} of the anchor:: unit tests failed under the parallel \
             runner (HOME/PATH race: the c397cd15 heartbeat test does not take ENV_LOCK):\n{}",
            stdout
                .lines()
                .filter(|l| l.contains("FAILED") || l.contains("panicked"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
