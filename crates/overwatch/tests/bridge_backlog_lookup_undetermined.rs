// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog abba6f0d — `overwatch review-queue --to-backlog` when the `backlog`
//! binary's presence cannot be determined.
//!
//! With `OVERWATCH_BACKLOG_BIN` unset, no `backlog` on PATH and the plugin
//! cache's `backlog` dir unreadable, the bridge does not know whether backlog
//! is installed. That must NOT be the fail-soft "backlog-unavailable" skip
//! (exit 0, reads as "nothing to do"): it must report `backlog-undetermined`,
//! name "backlog binary" in `undetermined_sources`, and exit 3.
//! Observed RED at `82ee4a78` (the old PATH-only scan returned `None` → the
//! silent skip, exit 0) and GREEN after `27824792`. The control pins that an
//! observed absence keeps the fail-soft skip.
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

/// Run the bridge in an empty project under `home`; returns (exit, stdout JSON, stderr).
fn bridge(home: &Path, project: &Path) -> (i32, serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(["review-queue", "--to-backlog"])
        .current_dir(project)
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("OVERWATCH_BACKLOG_BIN")
        .output()
        .expect("spawn overwatch");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .unwrap_or(serde_json::Value::Null);
    (
        out.status.code().unwrap_or(-1),
        json,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn unreadable_backlog_cache_dir_is_undetermined_exit_3() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let bl = home.join(".claude/plugins/cache/yukineko/backlog");
    std::fs::create_dir_all(bl.join("0.3.0/bin")).unwrap();
    std::fs::set_permissions(&bl, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestoreMode(bl.clone());
    if std::fs::read_dir(&bl).is_ok() {
        eprintln!("SKIP: chmod 000 does not deny access here (root?)");
        return;
    }

    let (code, json, stderr) = bridge(&home, &project);
    assert_eq!(
        code, 3,
        "could-not-locate-backlog must exit 3, not the fail-soft 0; json={json} stderr={stderr}"
    );
    assert_eq!(json["skipped"], "backlog-undetermined", "json={json}");
    assert!(
        json["undetermined_sources"]
            .as_array()
            .is_some_and(|a| a.iter().any(|s| s == "backlog binary")),
        "the backlog binary must be named as undetermined: {json}"
    );
}

#[test]
fn absent_backlog_keeps_the_fail_soft_skip() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();

    let (code, json, stderr) = bridge(&home, &project);
    assert_eq!(code, 0, "json={json} stderr={stderr}");
    assert_eq!(json["skipped"], "backlog-unavailable", "json={json}");
}
