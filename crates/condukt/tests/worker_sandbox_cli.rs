//! Integration coverage for the opt-in worker sandbox CLI (`condukt sandbox
//! run`). Exercises BOTH new branches end-to-end against the real binary:
//!   1. sandbox DISABLED (default) → the command runs unchanged on the host and
//!      the verdict is marked `"sandboxed": false`;
//!   2. sandbox ENABLED (`CONDUKT_WORKER_SANDBOX=1`) but docker unavailable →
//!      fail-soft `docker_unavailable` verdict, `"sandboxed": true`, exit 0
//!      (the command is NEVER run on the host as a fallback).
//!
//! Both paths must always exit 0 (never-break-a-turn).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

/// Sandbox OFF (default): `condukt sandbox run` runs the command on the host and
/// marks the verdict `"sandboxed": false`, exit 0.
#[test]
fn sandbox_disabled_runs_on_host_and_marks_verdict() {
    let out = Command::new(BIN)
        .args(["sandbox", "run", "--cmd", "printf hello", "--timeout", "10"])
        // Force sandbox OFF regardless of any ambient config.toml.
        .env("CONDUKT_WORKER_SANDBOX", "0")
        .output()
        .expect("spawn condukt sandbox run");

    assert!(
        out.status.success(),
        "sandbox run must exit 0 (fail-soft); got {:?}",
        out.status
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("verdict must be JSON");
    assert_eq!(
        v.get("sandboxed"),
        Some(&serde_json::Value::Bool(false)),
        "disabled run must mark sandboxed=false: {stdout}"
    );
    // Ran on the host: the command succeeded (exit 0, no panic) ⇒ passed.
    assert_eq!(
        v.get("passed"),
        Some(&serde_json::Value::Bool(true)),
        "a benign host command must pass: {stdout}"
    );
}

/// Sandbox ON but docker unavailable: fail-soft `docker_unavailable`, marked
/// `"sandboxed": true`, exit 0, and the command is NOT run on the host.
///
/// Skips (as a pass) if docker actually IS available on the runner, since then
/// the command really would run in a container and this assertion would not
/// hold — the disabled-path test already covers the host branch.
#[test]
fn sandbox_enabled_fails_soft_when_docker_absent() {
    let docker_ok = Command::new("docker")
        .arg("info")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if docker_ok {
        eprintln!("docker available on runner; skipping docker-absent assertion");
        return;
    }

    let out = Command::new(BIN)
        .args(["sandbox", "run", "--cmd", "printf hello", "--timeout", "10"])
        .env("CONDUKT_WORKER_SANDBOX", "1")
        .output()
        .expect("spawn condukt sandbox run");

    assert!(
        out.status.success(),
        "sandbox run must exit 0 even when docker is absent; got {:?}",
        out.status
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("verdict must be JSON");
    assert_eq!(
        v.get("sandboxed"),
        Some(&serde_json::Value::Bool(true)),
        "enabled run must mark sandboxed=true: {stdout}"
    );
    assert_eq!(
        v.get("note").and_then(|n| n.as_str()),
        Some("docker_unavailable"),
        "docker-absent enabled run must fail soft with docker_unavailable: {stdout}"
    );
}
