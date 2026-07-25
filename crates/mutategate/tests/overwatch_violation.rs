// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests for the FAIL-branch overwatch violation emit.
//!
//! These exercise the compiled `mutategate` binary as a subprocess (not just
//! in-process helpers) because `overwatch::store`'s path is derived from
//! `$HOME` + the cwd's repo root, so isolating the store fully needs a real
//! subprocess with its own `$HOME` and cwd — an in-process call would
//! otherwise write into the developer's real `~/.overwatch`.

use std::path::Path;
use std::process::Command;

const SAMPLE_FAIL: &str = r#"{ "outcomes": [
    { "scenario": { "Mutant": {} }, "summary": "CaughtMutant" },
    { "scenario": { "Mutant": {} }, "summary": "MissedMutant" }
] }"#;

const SAMPLE_PASS: &str = r#"{ "outcomes": [
    { "scenario": { "Mutant": {} }, "summary": "CaughtMutant" }
] }"#;

/// Sets up an isolated `$HOME` and a repo-root cwd (needs a `.git` dir so
/// `harness_core::projkey::repo_root` resolves to it, matching real usage)
/// under a fresh temp directory, and writes `outcomes.json` there.
struct Sandbox {
    dir: std::path::PathBuf,
}

impl Sandbox {
    fn new(name: &str, outcomes_json: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "mutategate-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::write(dir.join("outcomes.json"), outcomes_json).unwrap();
        Sandbox { dir }
    }

    fn home(&self) -> std::path::PathBuf {
        self.dir.join("home")
    }

    fn violations_path(&self) -> std::path::PathBuf {
        // Mirrors overwatch::store's storage_root: ~/.overwatch/<project_key>/overwatch/violations.jsonl
        let entries: Vec<_> = std::fs::read_dir(self.home().join(".overwatch"))
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one project_key dir under ~/.overwatch"
        );
        entries[0].path().join("overwatch").join("violations.jsonl")
    }

    fn run(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_mutategate"))
            .arg("--outcomes")
            .arg("outcomes.json")
            .current_dir(&self.dir)
            .env("HOME", self.home())
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .expect("failed to run mutategate binary")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn fail_outcome_appends_a_signed_nonempty_violation() {
    let sb = Sandbox::new("fail-appends", SAMPLE_FAIL);
    let out = sb.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "gate must fail: below threshold"
    );

    let lines = read_lines(&sb.violations_path());
    assert_eq!(lines.len(), 1, "exactly one violation must be appended");

    let ev: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(ev["source"], "mutategate");
    // "signed" here means the event carries the source/session/ts identity
    // fields `build_event` stamps (not a cryptographic signature) — assert
    // presence of each.
    assert!(ev["session_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(ev["ts"].as_i64().is_some());
    assert!(ev["task_key"].as_str().is_some_and(|s| !s.is_empty()));

    let sig = ev["signature"].as_str().unwrap();
    assert!(
        sig.starts_with("mutategate:"),
        "signature must be namespaced: {sig}"
    );
    let discriminator = sig.strip_prefix("mutategate:").unwrap();
    assert!(
        !discriminator.is_empty() && discriminator != "unknown",
        "discriminator must be a deterministic non-empty reason-class, not empty/unknown: {sig}"
    );
    assert_eq!(
        sig, "mutategate:below-threshold",
        "SAMPLE_FAIL has a viable kill-rate (1/2) below the default 0.80 threshold"
    );
}

#[test]
fn no_viable_mutants_appends_distinct_reason_class() {
    let sb = Sandbox::new(
        "no-viable",
        r#"{ "outcomes": [ { "scenario": { "Mutant": {} }, "summary": "Unviable" } ] }"#,
    );
    let out = sb.run();
    assert_eq!(out.status.code(), Some(1));

    let lines = read_lines(&sb.violations_path());
    assert_eq!(lines.len(), 1);
    let ev: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(ev["signature"], "mutategate:no-viable-mutants");
}

#[test]
fn pass_outcome_appends_no_violation() {
    let sb = Sandbox::new("pass-appends-none", SAMPLE_PASS);
    let out = sb.run();
    assert_eq!(out.status.code(), Some(0), "gate must pass: full kill-rate");

    // No ~/.overwatch dir at all should have been created for a PASS.
    assert!(
        !sb.home().join(".overwatch").exists(),
        "a PASS must emit nothing"
    );
}

#[test]
fn exit_code_and_output_unchanged_with_vs_without_emit_on_fail() {
    // "without emit" = a HOME where the overwatch store directory cannot be
    // created (a file sits where the dir needs to go), forcing
    // append_violation's create_dir_all/write to fail. The gate's exit code
    // and stdout/stderr must be identical either way — emit is fail-soft and
    // must not gate the gate.
    let sb = Sandbox::new("exit-code-stable", SAMPLE_FAIL);
    let baseline = sb.run();
    assert_eq!(baseline.status.code(), Some(1));

    let blocked = Sandbox::new("exit-code-stable-blocked", SAMPLE_FAIL);
    // Pre-create `~/.overwatch` as a *file* so storage_root's
    // create_dir_all inside append_violation cannot succeed.
    std::fs::write(blocked.home().join(".overwatch"), b"not a dir").unwrap();
    let blocked_out = blocked.run();

    assert_eq!(
        blocked_out.status.code(),
        baseline.status.code(),
        "emit failure must not change the gate's exit code"
    );
    assert_eq!(
        blocked_out.stdout, baseline.stdout,
        "stdout must be unaffected"
    );
    assert_eq!(
        blocked_out.stderr, baseline.stderr,
        "stderr must be unaffected"
    );
}

#[test]
fn no_panic_when_store_unwritable() {
    // Same unwritable-store setup as above, but this test's whole point is
    // that the process must not panic/abort — a clean exit (any code) proves
    // emit_violation's `Result` was swallowed, not `.unwrap()`ed.
    let sb = Sandbox::new("no-panic", SAMPLE_FAIL);
    std::fs::write(sb.home().join(".overwatch"), b"not a dir").unwrap();
    let out = sb.run();
    assert!(
        out.status.code().is_some(),
        "process must exit cleanly, not be killed by a panic/signal"
    );
    assert_eq!(out.status.code(), Some(1));
}

/// Real-CLI counterpart to the lib unit tests (backlog cde2212c): a
/// gate-disabling `--min-kill-rate 0` must be REJECTED with a usage error
/// (exit 2) BEFORE any evaluation, not silently accepted.
///
/// The outcomes file here is a genuine PASS (`SAMPLE_FAIL` is 1 caught / 1
/// missed = 0.5 kill-rate, which is `>= 0.0`), so the OLD `!(0.0..=1.0)`
/// guard accepted `0.0`, evaluated, and exited 0 — the gate silently passed
/// while effectively disabled. Observed RED against that behavior (exit 0);
/// GREEN after the floor fix (exit 2). Distinct exit code (2 = usage) from the
/// pass/fail codes (0/1) so this can't be confused with a normal gate verdict.
#[test]
fn zero_min_kill_rate_is_rejected_by_the_cli_before_evaluating() {
    let sb = Sandbox::new("zero-min-kill-rate", SAMPLE_FAIL);
    let out = Command::new(env!("CARGO_BIN_EXE_mutategate"))
        .arg("--outcomes")
        .arg("outcomes.json")
        .arg("--min-kill-rate")
        .arg("0")
        .current_dir(&sb.dir)
        .env("HOME", sb.home())
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .expect("failed to run mutategate binary");

    assert_eq!(
        out.status.code(),
        Some(2),
        "a gate-disabling min-kill-rate of 0 must be rejected as a usage error \
         (exit 2), not accepted and evaluated"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("disable the gate"),
        "stderr must explain WHY 0 is refused, got: {stderr}"
    );
    // It was rejected up front, so no store dir was ever created.
    assert!(
        !sb.home().join(".overwatch").exists(),
        "rejection must happen before any evaluation/emit"
    );
}
