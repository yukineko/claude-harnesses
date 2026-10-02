// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! IMPLEMENTER-WRITTEN (backlog f49e4a72): written by the same agent that
//! implemented the `specguard pending` fix, so it is NOT independent evidence in
//! the CLAUDE.md 2(a) sense. The independent RED test is
//! `tests/pending_undetermined.rs`. This file pins the choices that test left
//! open: the exit code, the dangling-symlink case, and what the real hook
//! command line delivers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn project(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("pending_contract_{name}"));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn pending(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(dir)
        .arg("pending")
        .output()
        .expect("specguard spawns")
}

/// A present-but-unloadable config exits 2 (EXIT_USAGE), the code every other
/// subcommand returns when config loading fails, so a direct caller cannot read
/// exit 0 as "nothing pending".
#[test]
fn unloadable_config_exits_usage_not_ok() {
    let d = project("exitcode");
    fs::write(d.join("specguard.toml"), "this is = = not [ toml\n").unwrap();
    let o = pending(&d);
    assert_eq!(
        o.status.code(),
        Some(2),
        "stdout={}",
        String::from_utf8_lossy(&o.stdout)
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("UNKNOWN"));
}

/// A dangling symlink at the config path is "present but unloadable", not
/// "absent": the absence check must not follow the link.
#[cfg(unix)]
#[test]
fn dangling_symlink_config_is_surfaced_not_silent() {
    let d = project("dangling");
    std::os::unix::fs::symlink(d.join("nowhere.toml"), d.join("specguard.toml")).unwrap();
    let o = pending(&d);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("UNKNOWN"),
        "dangling symlink config printed {out:?} (exit {:?})",
        o.status.code()
    );
}

/// Through the exact hooks.json command line (stderr discarded, exit code
/// masked by `|| true`), the notice still reaches stdout — the only channel the
/// SessionStart consumer reads.
#[cfg(unix)]
#[test]
fn notice_survives_the_hook_command_line() {
    let d = project("hookline");
    fs::write(d.join("specguard.toml"), "this is = = not [ toml\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_specguard");
    let o = Command::new("sh")
        .current_dir(&d)
        .arg("-c")
        .arg(format!("'{bin}' pending 2>/dev/null || true"))
        .output()
        .expect("sh spawns");
    assert_eq!(o.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("UNKNOWN"),
        "hook line stdout: {:?}",
        String::from_utf8_lossy(&o.stdout)
    );
}
