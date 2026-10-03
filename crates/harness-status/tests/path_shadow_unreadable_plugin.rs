// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog abba6f0d — `harness-status path-shadow` with one unreadable plugin
//! cache dir.
//!
//! The scan cannot know what that plugin ships, so it cannot say nothing is
//! shadowed. It must report the scan as undetermined (exit 1) naming the
//! plugin, not "[no PATH-shadowed plugin binaries]". Observed RED at
//! `82ee4a78` (the old `read_dir(..).ok()?` silently dropped the plugin and
//! printed the clean line, exit 0) and GREEN after `27824792`. The control
//! pins that a fully readable cache still scans clean.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct RestoreMode(PathBuf);
impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

fn plant(root: &Path, name: &str, version: &str) {
    let bin = root.join(name).join(version).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join(name), b"#!/bin/sh\n").unwrap();
}

fn path_shadow(home: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_harness-status"))
        .arg("path-shadow")
        .current_dir(home)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("harness-status runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn unreadable_plugin_dir_makes_the_scan_undetermined_and_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let root = home.join(".claude/plugins/cache/yukineko");
    plant(&root, "vabba-alpha", "0.1.0");
    plant(&root, "vabba-zeta", "0.1.0");
    let locked = root.join("vabba-zeta");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestoreMode(locked.clone());
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("SKIP: chmod 000 does not deny access here (root?)");
        return;
    }

    let (code, stdout, stderr) = path_shadow(&home);
    assert_eq!(
        code, 1,
        "an incomplete scan must not exit clean; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("no PATH-shadowed"),
        "an incomplete scan must never print the clean line: {stdout}"
    );
    assert!(
        stderr.contains("could not be completed") && stderr.contains("vabba-zeta"),
        "the undetermined scan must name the unreadable plugin: {stderr}"
    );
}

#[test]
fn readable_cache_scans_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let root = home.join(".claude/plugins/cache/yukineko");
    plant(&root, "vabba-alpha", "0.1.0");
    plant(&root, "vabba-zeta", "0.1.0");

    let (code, stdout, stderr) = path_shadow(&home);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("no PATH-shadowed"), "stdout={stdout}");
}
