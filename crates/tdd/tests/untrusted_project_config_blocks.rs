#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-11-08 (worktree trust inheritance removed): when a project
//! `tdd.toml` EXISTS but the root is not on the trust list, the Stop gate must
//! BLOCK (refusal to judge, naming the file) and `tdd red` / `tdd green` must
//! refuse (exit 1) instead of silently judging / proving with the built-in
//! defaults (CLAUDE.md section 3).
//!
//! Written by an independent test author (CLAUDE.md section 2(a)). Drives the
//! real binary with a sandbox HOME; the real trust list is never touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Fx {
    base: PathBuf,
    proj: PathBuf,
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
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

/// A repo with one commit and an UNCOMMITTED implementation change with no
/// test (the gate would block it on its own merits when it judges at all).
fn fixture(with_config: bool) -> Fx {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("tdd-untrusted-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let proj = base.join("proj");
    let home = base.join("home");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&proj, &["init", "-q"]);
    git(&proj, &["config", "user.email", "t@t.com"]);
    git(&proj, &["config", "user.name", "t"]);
    git(&proj, &["config", "commit.gpgsign", "false"]);
    std::fs::write(proj.join("seed.txt"), "seed\n").unwrap();
    if with_config {
        std::fs::write(proj.join("tdd.toml"), "test_cmd = \"true\"\n").unwrap();
    }
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-qm", "seed"]);
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("src/lib.rs"),
        "pub fn f() -> i32 {\n    let x = 1;\n    let y = x + 1;\n    println!(\"{}\", y);\n    y\n}\n",
    )
    .unwrap();
    Fx { base, proj, home }
}

fn trust(fx: &Fx, root: &Path) {
    let d = fx.home.join(".harness");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("trust.toml"),
        format!("trusted = [{:?}]\n", root.to_string_lossy()),
    )
    .unwrap();
}

fn run(fx: &Fx, cwd: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(args)
        .current_dir(cwd)
        .env("HOME", &fx.home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .env("CLAUDE_CODE_SESSION_ID", "s-untrusted-cfg")
        .env_remove("TDD_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tdd spawns");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn stop(fx: &Fx, cwd: &Path) -> (String, String) {
    let payload = serde_json::json!({
        "session_id": "s-untrusted-cfg",
        "cwd": cwd.to_string_lossy(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let (code, stdout, stderr) = run(fx, cwd, &["gate"], &payload);
    assert_eq!(code, 0, "Stop hook exits 0; stderr={stderr}");
    (stdout, stderr)
}

fn block_reason(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    (v["decision"] == "block").then(|| v["reason"].as_str().unwrap_or_default().to_string())
}

#[test]
fn untrusted_root_with_project_config_blocks_naming_the_file() {
    let fx = fixture(true);
    let (stdout, stderr) = stop(&fx, &fx.proj);
    let reason = block_reason(&stdout)
        .unwrap_or_else(|| panic!("must block: stdout={stdout:?} stderr={stderr:?}"));
    assert!(reason.contains("tdd.toml"), "must name the file: {reason}");
    assert!(
        reason.contains("workspace-trust") && reason.contains("tdd trust"),
        "must say why and name the remedy: {reason}"
    );
}

/// Control: no project config => the new refusal does not fire; the gate
/// judges the uncommitted untested impl on its own merits (still a block, but
/// NOT the untrusted-config refusal).
#[test]
fn untrusted_root_without_project_config_is_not_refused() {
    let fx = fixture(false);
    assert!(!fx.proj.join("tdd.toml").exists());
    let (stdout, stderr) = stop(&fx, &fx.proj);
    if let Some(r) = block_reason(&stdout) {
        assert!(
            !r.contains("workspace-trust"),
            "no config file => no untrusted-config refusal: {r}"
        );
    }
    assert!(!stderr.contains("workspace-trust"), "{stderr}");
}

/// Anti-vacuity control: the SAME config in a root that IS listed is honored.
#[test]
fn trusted_root_with_same_project_config_is_not_refused() {
    let fx = fixture(true);
    trust(&fx, &fx.proj);
    let (stdout, stderr) = stop(&fx, &fx.proj);
    if let Some(r) = block_reason(&stdout) {
        assert!(!r.contains("workspace-trust"), "{r}");
    }
    assert!(!stderr.contains("workspace-trust"), "{stderr}");
}

#[test]
fn red_and_green_refuse_in_untrusted_root_with_project_config() {
    let fx = fixture(true);
    // A failing command is a valid RED and a passing one a valid GREEN, so any
    // exit 1 here is the refusal, not the phase precondition.
    let (code, _o, err) = run(
        &fx,
        &fx.proj,
        &["red", "--task", "t1", "--cmd", "false"],
        "",
    );
    assert_eq!(code, 1, "red must refuse; stderr={err}");
    assert!(err.contains("tdd.toml"), "must name the file: {err}");
    let (code, _o, err) = run(
        &fx,
        &fx.proj,
        &["green", "--task", "t1", "--cmd", "true"],
        "",
    );
    assert_eq!(code, 1, "green must refuse; stderr={err}");
    assert!(err.contains("tdd.toml"), "must name the file: {err}");
}

/// Anti-vacuity control for the red/green refusal: with the root listed, the
/// failing command is accepted as RED (exit 0).
#[test]
fn red_is_accepted_in_trusted_root_with_project_config() {
    let fx = fixture(true);
    trust(&fx, &fx.proj);
    let (code, _o, err) = run(
        &fx,
        &fx.proj,
        &["red", "--task", "t1", "--cmd", "false"],
        "",
    );
    assert_eq!(
        code, 0,
        "trusted root: failing cmd is a valid RED; stderr={err}"
    );
}

/// A linked worktree of a TRUSTED main tree carrying a project config is
/// itself untrusted (no inheritance) and blocks.
#[test]
fn linked_worktree_of_trusted_main_with_config_blocks() {
    let fx = fixture(true);
    // `fixture` leaves an uncommitted change in proj; the worktree is cut from
    // the committed HEAD, which carries tdd.toml.
    trust(&fx, &fx.proj);
    let wt = fx.base.join("linked");
    git(
        &fx.proj,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"],
    );
    assert!(wt.join(".git").is_file(), "apparatus: linked worktree");
    assert!(wt.join("tdd.toml").exists(), "apparatus: config in wt");
    let (stdout, stderr) = stop(&fx, &wt);
    let reason = block_reason(&stdout)
        .unwrap_or_else(|| panic!("must block: stdout={stdout:?} stderr={stderr:?}"));
    assert!(reason.contains("tdd.toml"), "{reason}");
    assert!(reason.contains("workspace-trust"), "{reason}");
}
