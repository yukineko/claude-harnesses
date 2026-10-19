#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `scripts/continuous-audit.sh` fail-opens in three places. Each test
//! asserts the CORRECT (fail-closed) behaviour: a round that could not be
//! recorded must not exit 0 / must not print the PASS line.
//!
//! 1. overwatch binary not found -> script prints "nothing recorded" and
//!    `exit 0` (the `else` arm of the binary lookup).
//! 2. `--round` missing (not a dry run) -> `[ "$DRY_RUN" -eq 1 ] || exit 0`.
//! 3. `overwatch record-finding` fails -> `run_ow` swallows the non-zero exit
//!    ("continuing, fail-soft") and the script ends with `PASS: round ...
//!    recorded (findings -> review store ...)`, exit 0, although the finding
//!    was never stored.
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf()
}

/// Copy the script into `<tmp>/scripts/` so its `REPO=` (parent of the script
/// dir) is a sandbox with no `target/{release,debug}/overwatch`.
fn sandboxed_script(tag: &str) -> (PathBuf, PathBuf) {
    let t = std::env::temp_dir().join(format!("ow-repro-ca-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    std::fs::create_dir_all(t.join("scripts")).unwrap();
    let dst = t.join("scripts/continuous-audit.sh");
    std::fs::copy(repo_root().join("scripts/continuous-audit.sh"), &dst).unwrap();
    (t, dst)
}

fn run(script: &PathBuf, home: &PathBuf, ow_bin: Option<&str>, args: &[&str]) -> Output {
    let mut c = Command::new("/bin/bash");
    c.arg(script)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home);
    if let Some(b) = ow_bin {
        c.env("OVERWATCH_BIN", b);
    }
    c.output().unwrap()
}

#[test]
#[ignore = "repro: continuous-audit.sh exits 0 when the overwatch binary is missing; remove ignore when fixed"]
fn missing_overwatch_binary_must_not_exit_zero() {
    let (t, script) = sandboxed_script("nobin");
    let home = t.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = run(&script, &home, None, &["--round", "R1"]);
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    assert!(
        se.contains("binary not found"),
        "control failed: the missing-binary path was not taken.\nstdout={so}\nstderr={se}"
    );
    assert!(
        !out.status.success(),
        "script exited 0 with NOTHING recorded (binary missing).\nstdout={so}\nstderr={se}"
    );
}

#[test]
#[ignore = "repro: continuous-audit.sh exits 0 when --round is missing; remove ignore when fixed"]
fn missing_round_must_not_exit_zero() {
    let (t, script) = sandboxed_script("noround");
    let home = t.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ow = env!("CARGO_BIN_EXE_overwatch");
    let out = run(&script, &home, Some(ow), &["--target", "overwatch"]);
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    assert!(
        se.contains("--round <round-id> is required"),
        "control failed: the missing-round path was not taken.\nstdout={so}\nstderr={se}"
    );
    assert!(
        !out.status.success(),
        "script exited 0 although --round was required and nothing was recorded.\nstdout={so}\nstderr={se}"
    );
}

#[test]
#[ignore = "repro: continuous-audit.sh swallows record-finding failure and prints PASS; remove ignore when fixed"]
fn record_finding_failure_must_not_exit_zero() {
    let (t, script) = sandboxed_script("recfail");
    let home = t.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let stub = t.join("ow-stub");
    std::fs::write(
        &stub,
        "#!/bin/sh\ncase \"$1\" in record-finding) echo 'stub: store write failed' >&2; exit 1;; esac\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = run(
        &script,
        &home,
        Some(stub.to_str().unwrap()),
        &[
            "--round",
            "R1",
            "--finding",
            "CA-R1-001|high|summary|crates/x/src/a.rs|because a.rs:1 does it",
        ],
    );
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    assert!(
        se.contains("stub: store write failed"),
        "control failed: stub record-finding was not invoked.\nstdout={so}\nstderr={se}"
    );
    assert!(
        !out.status.success(),
        "script exited 0 (and printed PASS: {}) although record-finding failed, \
         so a CONFIRMED finding was never stored.\nstdout={so}\nstderr={se}",
        so.contains("PASS: round")
    );
}
