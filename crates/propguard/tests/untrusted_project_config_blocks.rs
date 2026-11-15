#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-11-08 (worktree trust inheritance removed): when a project
//! `propguard.toml` EXISTS but the root is not on the trust list, the Stop
//! gate must BLOCK (refusal to judge, tag `config-untrusted`, naming the
//! file) instead of silently falling back to built-in defaults (CLAUDE.md
//! section 3).
//!
//! Written by an independent test author (CLAUDE.md section 2(a)). Drives the
//! real binary; HOME is a temp dir separate from the project, so the real
//! trust list is never touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

struct Fx {
    _t: tempfile::TempDir,
    proj: PathBuf,
    home: PathBuf,
}

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

fn fixture() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let proj = base.join("proj");
    let home = base.join("home");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&proj, &["init", "-q"]);
    git(&proj, &["config", "user.email", "t@t"]);
    git(&proj, &["config", "user.name", "t"]);
    std::fs::write(proj.join("a.rs"), "fn f() { panic!() }\n").unwrap();
    Fx { _t: t, proj, home }
}

fn run(fx: &Fx, root: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_propguard"));
    harness_core::git_env::scrub(&mut cmd);
    let mut child = cmd
        .args(args)
        .current_dir(root)
        .env("HOME", &fx.home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .env("PROPGUARD_STATE_DIR", fx.home.join("pg-state"))
        .env_remove("PROPGUARD_CRITERIA")
        .env_remove("PROPGUARD_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// One Stop hook; returns (stdout, stderr) after asserting a clean exit 0.
fn stop(fx: &Fx, root: &Path) -> (String, String) {
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "s-untrusted-cfg",
        "stop_hook_active": false,
        "cwd": root.to_string_lossy(),
    })
    .to_string();
    let (code, stdout, stderr) = run(fx, root, &["check"], &payload);
    assert_eq!(code, 0, "stdout={stdout:?} stderr={stderr:?}");
    (stdout, stderr)
}

fn block_reason(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find(|v| v["decision"] == "block")
        .map(|v| v["reason"].as_str().unwrap_or_default().to_string())
}

const PROJECT_CFG: &str = "mode = \"subprocess\"\nchecker_cmd = \"echo PROP x: PASS\"\n";

#[test]
fn untrusted_root_with_project_config_blocks_naming_the_file() {
    let fx = fixture();
    std::fs::write(fx.proj.join("propguard.toml"), PROJECT_CFG).unwrap();
    let (stdout, stderr) = stop(&fx, &fx.proj);
    let reason = block_reason(&stdout)
        .unwrap_or_else(|| panic!("must block: stdout={stdout:?} stderr={stderr:?}"));
    assert!(
        reason.contains("propguard.toml"),
        "reason must name the config file: {reason}"
    );
    assert!(
        reason.contains("config-untrusted"),
        "reason must carry the tag: {reason}"
    );
    assert!(
        reason.contains("propguard trust"),
        "reason must name the remedy: {reason}"
    );
}

/// Control: no project config => the new refusal does not fire (today's
/// behaviour is unchanged).
#[test]
fn untrusted_root_without_project_config_is_not_refused() {
    let fx = fixture();
    assert!(!fx.proj.join("propguard.toml").exists());
    let (stdout, stderr) = stop(&fx, &fx.proj);
    if let Some(r) = block_reason(&stdout) {
        assert!(
            !r.contains("config-untrusted"),
            "no config file => no untrusted-config refusal: {r}"
        );
    }
    assert!(
        !stderr.contains("config-untrusted"),
        "stderr={stderr:?} stdout={stdout:?}"
    );
}

/// Anti-vacuity control: once the SAME root is trusted, the same file is
/// honored and the refusal disappears.
#[test]
fn trusted_root_with_same_project_config_is_not_refused() {
    let fx = fixture();
    std::fs::write(fx.proj.join("propguard.toml"), PROJECT_CFG).unwrap();
    let (code, so, se) = run(&fx, &fx.proj, &["trust"], "");
    assert_eq!(code, 0, "trust failed: {so} {se}");
    let (stdout, stderr) = stop(&fx, &fx.proj);
    if let Some(r) = block_reason(&stdout) {
        assert!(!r.contains("config-untrusted"), "{r}");
    }
    assert!(!stderr.contains("config-untrusted"), "{stderr}");
}

/// A linked worktree of a TRUSTED main tree is itself untrusted (no
/// inheritance): with a project config checked out in it, it blocks.
#[test]
fn linked_worktree_of_trusted_main_with_config_blocks() {
    let fx = fixture();
    std::fs::write(fx.proj.join("propguard.toml"), PROJECT_CFG).unwrap();
    git(&fx.proj, &["add", "-A"]);
    git(&fx.proj, &["commit", "-qm", "seed"]);
    let (code, so, se) = run(&fx, &fx.proj, &["trust"], "");
    assert_eq!(code, 0, "trust failed: {so} {se}");
    let wt = fx.proj.parent().unwrap().join("linked");
    git(
        &fx.proj,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"],
    );
    assert!(wt.join(".git").is_file(), "apparatus: linked worktree");
    assert!(
        wt.join("propguard.toml").exists(),
        "apparatus: config in wt"
    );
    let (stdout, stderr) = stop(&fx, &wt);
    let reason = block_reason(&stdout)
        .unwrap_or_else(|| panic!("must block: stdout={stdout:?} stderr={stderr:?}"));
    assert!(reason.contains("config-untrusted"), "{reason}");
}
