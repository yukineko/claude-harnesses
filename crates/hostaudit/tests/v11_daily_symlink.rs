//! Canon ed704868 item 13 (CONTRACT.md 12.11): `daily-task --print` emits commands
//! that start with the absolute `$HOME/.hostaudit/bin/hostaudit`, and (re)creates that
//! symlink to the current executable. `session-start` refreshes it too. If the symlink
//! cannot be created the command fails non-zero and prints no stanza.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_hostaudit")
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(exe())
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .output()
        .unwrap()
}

fn link_path(home: &Path) -> PathBuf {
    home.join(".hostaudit/bin/hostaudit")
}

fn commands(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.trim().strip_prefix("command = "))
        .map(|c| c.trim_matches('"').to_string())
        .collect()
}

fn resolves_to_exe(link: &Path) -> bool {
    match (std::fs::canonicalize(link), std::fs::canonicalize(exe())) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[test]
fn every_command_starts_with_the_stable_home_path() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    let out = run(&home, &["daily-task", "--print"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    let cmds = commands(&text);
    assert!(cmds.len() >= 3, "audit/local/aws tasks expected:\n{text}");
    let want = link_path(&home);
    for c in &cmds {
        let first = c.split_whitespace().next().unwrap_or("").trim_matches('\'');
        assert_eq!(
            first,
            want.to_str().unwrap(),
            "command must start with the stable path, not the plugin cache path: {c:?}\n{text}"
        );
    }
}

#[test]
fn print_creates_the_symlink_to_the_current_executable() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    assert!(!link_path(&home).exists(), "precondition");
    let out = run(&home, &["daily-task", "--print"]);
    assert!(out.status.success(), "{out:?}");
    let l = link_path(&home);
    assert!(
        std::fs::symlink_metadata(&l)
            .unwrap()
            .file_type()
            .is_symlink(),
        "{l:?} must be a symlink"
    );
    assert!(resolves_to_exe(&l), "{l:?} must resolve to {}", exe());
}

#[test]
fn print_repoints_a_stale_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    let l = link_path(&home);
    std::fs::create_dir_all(l.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("/bin/true", &l).unwrap();
    assert!(!resolves_to_exe(&l), "precondition: stale link");
    let out = run(&home, &["daily-task", "--print"]);
    assert!(out.status.success(), "{out:?}");
    assert!(resolves_to_exe(&l), "stale symlink must be refreshed");
}

#[test]
fn print_replaces_a_dangling_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    let l = link_path(&home);
    std::fs::create_dir_all(l.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(home.join("gone/hostaudit"), &l).unwrap();
    let out = run(&home, &["daily-task", "--print"]);
    assert!(out.status.success(), "{out:?}");
    assert!(resolves_to_exe(&l), "dangling symlink must be refreshed");
}

#[test]
fn print_fails_closed_when_the_symlink_cannot_be_created() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(home.join(".hostaudit")).unwrap();
    // ~/.hostaudit/bin is a regular file: the directory cannot be created.
    std::fs::write(home.join(".hostaudit/bin"), "not a directory").unwrap();
    let out = run(&home, &["daily-task", "--print"]);
    assert_ne!(out.status.code(), Some(0), "must exit non-zero: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        commands(&text).is_empty() && !text.contains("[[task]]"),
        "no stanza may be printed when the stable path cannot be provided:\n{text}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).trim().is_empty(),
        "the reason must be said on stderr"
    );
}

#[test]
fn session_start_refreshes_the_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    let l = link_path(&home);
    std::fs::create_dir_all(l.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("/bin/true", &l).unwrap();
    let out = run(&home, &["session-start"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        resolves_to_exe(&l),
        "session-start must repoint the stable symlink after a plugin update"
    );
}

#[test]
fn session_start_creates_a_missing_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    let out = run(&home, &["session-start"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(resolves_to_exe(&link_path(&home)));
}

#[test]
fn session_start_still_prints_its_message_when_the_symlink_cannot_be_made() {
    let tmp = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(home.join(".hostaudit")).unwrap();
    std::fs::write(home.join(".hostaudit/bin"), "not a directory").unwrap();
    let out = run(&home, &["session-start"]);
    // A hook must stay visible: a systemMessage is emitted, and it must not read as healthy.
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("session-start stdout is not JSON ({e}): {out:?}"));
    let msg = v["systemMessage"].as_str().unwrap_or("");
    assert!(!msg.trim().is_empty(), "{out:?}");
    assert!(
        msg.contains("UNKNOWN") || msg.contains("symlink") || msg.contains("daily"),
        "the failure to refresh the stable path must be surfaced, got: {msg}"
    );
}
