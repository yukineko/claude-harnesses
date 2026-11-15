#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-11-08 (worktree trust inheritance removed): when a project
//! `reviewgate.toml` EXISTS but the root is not on the trust list, the Stop
//! hook must BLOCK (refusal to judge, tag `config-untrusted`, naming the
//! file). It used to fall back to the `Off` default and ALLOW, silently
//! skipping the review the project declared (CLAUDE.md section 3).
//!
//! Written by an independent test author (CLAUDE.md section 2(a)). Drives the
//! real binary with a sandboxed HOME; the real trust list is never touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// (project root, sandbox HOME). The project is a git repo with one commit
/// and one uncommitted reviewable change.
fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("rg-untrusted-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let proj = base.join("proj");
    let home = base.join("home");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&proj, &["init", "-q"]);
    git(&proj, &["config", "user.email", "t@t"]);
    git(&proj, &["config", "user.name", "t"]);
    std::fs::write(proj.join("seed.rs"), "fn a() {}\n").unwrap();
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-qm", "seed"]);
    std::fs::write(
        proj.join("changed.rs"),
        "fn f() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n",
    )
    .unwrap();
    (proj, home)
}

fn trust(home: &Path, root: &Path) {
    let d = home.join(".harness");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("trust.toml"),
        format!("trusted = [{:?}]\n", root.to_string_lossy()),
    )
    .unwrap();
}

fn stop(proj: &Path, home: &Path) -> (i32, String, String) {
    let payload = serde_json::json!({
        "session_id": "s-untrusted-cfg",
        "cwd": proj.to_string_lossy(),
        "hook_event_name": "Stop",
    })
    .to_string();
    let mut child =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_reviewgate")))
            .arg("review")
            .current_dir(proj)
            .env("HOME", home)
            .env_remove("HARNESS_SANDBOX_HOME")
            .env("CLAUDE_CODE_SESSION_ID", "s-untrusted-cfg")
            .env_remove("REVIEWGATE_DISABLE")
            .env_remove("HARNESS_TRUST_ALL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn block_json(stdout: &str, stderr: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected block JSON, got {stdout:?} ({stderr}): {e}"))
}

#[test]
fn untrusted_root_with_project_config_blocks_naming_the_file() {
    let (proj, home) = fixture("block");
    std::fs::write(proj.join("reviewgate.toml"), "mode = \"off\"\n").unwrap();
    let (code, stdout, stderr) = stop(&proj, &home);
    assert_eq!(code, 0, "Stop-hook block is exit 0 + JSON; stderr={stderr}");
    let v = block_json(&stdout, &stderr);
    assert_eq!(v["decision"], "block", "stdout={stdout}");
    let reason = v["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("reviewgate.toml"),
        "reason must name the config file: {reason}"
    );
    assert!(
        reason.contains("config-untrusted"),
        "reason must carry the tag: {reason}"
    );
    assert!(
        reason.contains("reviewgate trust"),
        "reason must name the remedy: {reason}"
    );
}

/// Control: no project config at all => today's behaviour (default `off`,
/// allow, no block from this cause).
#[test]
fn untrusted_root_without_project_config_does_not_block() {
    let (proj, home) = fixture("noconfig");
    assert!(!proj.join("reviewgate.toml").exists());
    let (code, stdout, stderr) = stop(&proj, &home);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stdout.contains("block") && !stdout.contains("config-untrusted"),
        "no project config must not block: stdout={stdout:?} stderr={stderr:?}"
    );
}

/// Anti-vacuity control: the SAME config in a TRUSTED root is honored (mode
/// off => allow), so the block above is caused by untrusted-ness alone.
#[test]
fn trusted_root_with_same_project_config_is_honored() {
    let (proj, home) = fixture("trusted");
    std::fs::write(proj.join("reviewgate.toml"), "mode = \"off\"\n").unwrap();
    trust(&home, &proj);
    let (code, stdout, stderr) = stop(&proj, &home);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stdout.contains("block") && !stdout.contains("config-untrusted"),
        "trusted root must honor mode=off: stdout={stdout:?} stderr={stderr:?}"
    );
}

/// A linked worktree of a TRUSTED main tree carrying a project config is
/// itself untrusted (no inheritance) and therefore blocks.
#[test]
fn linked_worktree_of_trusted_main_with_config_blocks() {
    let (main, home) = fixture("wt");
    std::fs::write(main.join("reviewgate.toml"), "mode = \"off\"\n").unwrap();
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "cfg"]);
    trust(&home, &main);
    let wt = main.parent().unwrap().join("linked");
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"],
    );
    assert!(wt.join(".git").is_file(), "apparatus: linked worktree");
    assert!(
        wt.join("reviewgate.toml").exists(),
        "apparatus: config in wt"
    );
    std::fs::write(wt.join("changed2.rs"), "fn g() {}\n").unwrap();
    let (code, stdout, stderr) = stop(&wt, &home);
    assert_eq!(code, 0, "stderr={stderr}");
    let v = block_json(&stdout, &stderr);
    assert_eq!(v["decision"], "block", "stdout={stdout}");
    assert!(v["reason"]
        .as_str()
        .unwrap_or_default()
        .contains("config-untrusted"));
}
