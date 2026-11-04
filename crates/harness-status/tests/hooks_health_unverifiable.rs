#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repros for two gaps left by the 0.2.83 hook-health fix (harness-core
//! `extract_binary_path`). Fixed in harness-status 0.1.35 (GH #1483 / #1484).
//!
//! 1. A hook whose target the check cannot resolve (relative script after an
//!    interpreter, e.g. a typo `bash hooks/gaurd.sh`) is dropped from the
//!    report, and `hooks-health` then prints "all registered hook binaries
//!    present" — "could not check" rendered as "checked, fine".
//! 2. A target that exists but is not executable is reported present, though
//!    the hook cannot exec it.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn sandbox(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("hs-unverifiable-{tag}-"))
        .tempdir()
        .unwrap()
}

fn write_settings(home: &std::path::Path, command: &str) {
    let dir = home.join(".claude");
    std::fs::create_dir_all(&dir).unwrap();
    let settings = serde_json::json!({
        "hooks": { "PreToolUse": [ { "hooks": [ { "type": "command", "command": command } ] } ] }
    });
    std::fs::write(dir.join("settings.json"), settings.to_string()).unwrap();
}

fn hooks_health(home: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_harness-status"))
        .arg("hooks-health")
        .current_dir(home)
        .env("HOME", home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .output()
        .expect("binary runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn unresolvable_target_is_not_reported_as_all_present() {
    let home = sandbox("relative");
    write_settings(home.path(), "bash hooks/gaurd.sh");
    let out = hooks_health(home.path());
    assert!(
        !out.contains("all registered hook binaries present"),
        "a hook whose target could not be checked was summarised as present:\n{out}"
    );
}

#[test]
fn non_executable_target_is_flagged() {
    let home = sandbox("noexec");
    let target = home.path().join("hook-bin");
    std::fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_settings(home.path(), &format!("{} check", target.display()));
    let out = hooks_health(home.path());
    assert!(
        !out.contains("all registered hook binaries present"),
        "a non-executable hook target was summarised as present:\n{out}"
    );
}
