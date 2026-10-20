#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro: `overwatch pause` / `resume` delegate to `condukt state pause|resume`
//! and exit 0 even when condukt exits non-zero. `control.rs` run_fail_soft
//! returns false and the caller only records `pause ok=false` in the ledger and
//! returns `Ok(())`. A pause/resume that did not happen is reported to the
//! operator/script as success.
//!
//! A stub `condukt` that exits 1 is placed first on PATH.
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_overwatch"))
}

fn run_with_failing_condukt(tag: &str, verb: &str) -> (Option<i32>, String, String) {
    let t = std::env::temp_dir().join(format!("ow-repro-ctl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let home = t.join("home");
    let cwd = t.join("proj");
    let stubs = t.join("stubs");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&stubs).unwrap();
    assert!(Command::new("git")
        .current_dir(&cwd)
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success());
    let stub = stubs.join("condukt");
    std::fs::write(&stub, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        stubs.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(bin())
        .current_dir(&cwd)
        .env("HOME", &home)
        .env("PATH", path)
        .args([verb, "--run", "run-x"])
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn pause_must_be_nonzero_when_condukt_fails() {
    let (code, so, se) = run_with_failing_condukt("pause", "pause");
    assert!(
        so.contains("condukt exited 1"),
        "control failed: stub condukt was not the one invoked.\nstdout={so}\nstderr={se}"
    );
    assert_ne!(
        code,
        Some(0),
        "pause exited 0 although condukt exited 1.\nstdout={so}\nstderr={se}"
    );
}

#[test]
fn resume_must_be_nonzero_when_condukt_fails() {
    let (code, so, se) = run_with_failing_condukt("resume", "resume");
    assert!(
        so.contains("condukt exited 1"),
        "control failed: stub condukt was not the one invoked.\nstdout={so}\nstderr={se}"
    );
    assert_ne!(
        code,
        Some(0),
        "resume exited 0 although condukt exited 1.\nstdout={so}\nstderr={se}"
    );
}
