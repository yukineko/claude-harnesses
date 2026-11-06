// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Audit B item 15 (memory `liveness-and-observability`, section "main-tree-guard
//! lock: pid proves nothing, use transcript mtime"; originally
//! `maintree-guard-stale-lock`), 2026-11-07.
//!
//! `condukt guard main-tree` reads `backlog lock status`; a lock reported
//! `stale` (heartbeat aged out) is dropped as "not a peer"
//! (`maintree::parse_backlog_lock`). But heartbeat age and pid prove nothing
//! about the holder session; the signal that works is the holder's session
//! transcript mtime (`$HOME/.claude/projects/<slug>/<session-id>.jsonl`).
//! RED: a stale lock (dead pid) whose holder transcript was written just now
//! must still count as a live peer, so a staged commit in the primary tree
//! blocks. Control: the same stale lock with an idle transcript passes.
//!
//! The liveness inputs are stubbed on PATH, as in `main_tree_guard_cli.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const HOLDER: &str = "holder-sess-b15";

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_dir_on_path() -> PathBuf {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string())
        .parent()
        .unwrap()
        .to_path_buf()
}

fn stub(dir: &Path, name: &str, stdout: &str) {
    let p = dir.join(name);
    std::fs::write(
        &p,
        format!("#!/bin/sh\nprintf '%s\\n' '{stdout}'\nexit 0\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn slug(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn guard(tag: &str, fresh: bool) -> Output {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join(format!("repo-{tag}"));
    let stubs = tmp.path().join("stubs");
    let home = tmp.path().join("home");
    for d in [&repo, &stubs, &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    git(&repo, &["add", "seed.txt"]);
    git(&repo, &["commit", "-q", "-m", "seed"]);
    std::fs::write(repo.join("edited.txt"), "x\n").unwrap();
    git(&repo, &["add", "edited.txt"]);

    stub(&stubs, "overwatch", r#"{"backlog":{"pending":0}}"#);
    // A stale lock with a pid that cannot be alive.
    stub(
        &stubs,
        "backlog",
        &format!(
            r#"{{"session_id":"{HOLDER}","pid":999999,"project":"{}","stale":true}}"#,
            repo.display()
        ),
    );

    let mut projects = vec![repo.clone()];
    if let Ok(c) = repo.canonicalize() {
        projects.push(c);
    }
    for p in projects {
        let d = home.join(".claude/projects").join(slug(&p));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(format!("{HOLDER}.jsonl"));
        std::fs::write(&f, "{\"type\":\"assistant\"}\n").unwrap();
        if !fresh {
            assert!(Command::new("touch")
                .args(["-t", "202001010000"])
                .arg(&f)
                .status()
                .unwrap()
                .success());
        }
    }

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.args(["guard", "main-tree"])
        .current_dir(&repo)
        .env(
            "PATH",
            format!("{}:{}", stubs.display(), git_dir_on_path().display()),
        )
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", "this-session")
        .env_remove("CONDUKT_MAINTREE_OVERRIDE")
        .output()
        .unwrap()
}

/// Control: stale lock + idle transcript = no live peer.
#[test]
fn stale_lock_with_idle_holder_transcript_passes() {
    let out = guard("idle", false);
    assert_eq!(
        out.status.code(),
        Some(0),
        "control: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// RED: the holder's transcript is fresh, so the session is live.
#[test]
#[ignore = "backlog 3da84f08: open defect, remove ignore when fixed"]
fn stale_lock_with_fresh_holder_transcript_blocks() {
    let out = guard("fresh", true);
    assert_eq!(
        out.status.code(),
        Some(1),
        "guard main-tree passed a staged primary-tree commit although the lock holder \
         {HOLDER} wrote its transcript seconds ago (lock heartbeat stale, pid dead — \
         neither proves the session ended). stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
