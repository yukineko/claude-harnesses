//! RED repro (2026-11-04): `hostaudit daily-task --print` emits bare
//! `command = "hostaudit audit"`. daily runs tasks via `sh -c` with PATH =
//! ~/.cargo/bin + its inherited PATH; plugin bin dirs are NOT on a hook
//! process's PATH (repo memory: hook-processes-lack-plugin-path), so the
//! automatic daily audit exits 127 every day instead of auditing.
//! Expected: every emitted task command starts with an absolute path to an
//! existing executable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
use std::process::Command;

#[test]
fn daily_stanza_commands_use_an_absolute_executable_path() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_hostaudit"))
        .args(["daily-task", "--print"])
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(out.status.success(), "daily-task --print failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    let cmds: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("command = "))
        .collect();
    assert!(!cmds.is_empty(), "no task commands in stanza:\n{text}");
    for c in cmds {
        let c = c.trim_matches('"');
        let exe = c.split_whitespace().next().unwrap_or("");
        let p = std::path::Path::new(exe.trim_matches('\''));
        assert!(
            p.is_absolute() && p.is_file(),
            "task command does not start with an absolute path to an existing executable: {c:?}\n{text}"
        );
    }
}
