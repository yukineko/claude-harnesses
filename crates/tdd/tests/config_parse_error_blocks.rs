#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A chosen config file (trusted project `tdd.toml`, else home
//! `~/.tdd/config.toml`) that EXISTS but does not parse must make the Stop gate
//! BLOCK (naming the file and the parse error) and `tdd red` / `tdd green`
//! exit non-zero naming the file, instead of silently falling back to the
//! built-in defaults (CLAUDE.md section 3: cannot-determine resolves to the
//! restrictive side).
//!
//! Written by an independent test author (CLAUDE.md section 2(a)). Drives the
//! real binary with a sandbox HOME; the real trust list is never touched.
//! The fixture repo has NO implementation change, so under defaults the gate
//! allows; a block can only come from the config error.

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

/// A clean repo with one commit (no uncommitted change at all).
fn fixture(project_cfg: Option<&str>, home_cfg: Option<&str>) -> Fx {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("tdd-cfgparse-{}-{n}", std::process::id()));
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
    if let Some(c) = project_cfg {
        std::fs::write(proj.join("tdd.toml"), c).unwrap();
    }
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-qm", "seed"]);
    if let Some(c) = home_cfg {
        let d = home.join(".tdd");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.toml"), c).unwrap();
    }
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
        .env("CLAUDE_CODE_SESSION_ID", "s-cfgparse")
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
        "session_id": "s-cfgparse",
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

const BAD_ENUM: &str = "mode = \"strictt\"\n";
const BAD_SYNTAX: &str = "impl_globs = [\n";

#[test]
fn gate_blocks_on_unparseable_project_config_unknown_enum() {
    let fx = fixture(Some(BAD_ENUM), None);
    trust(&fx, &fx.proj);
    let (stdout, stderr) = stop(&fx, &fx.proj);
    let reason = block_reason(&stdout)
        .unwrap_or_else(|| panic!("must block: stdout={stdout:?} stderr={stderr:?}"));
    let path = fx.proj.join("tdd.toml").to_string_lossy().into_owned();
    assert!(
        reason.contains(&path),
        "must name the file {path}: {reason}"
    );
    assert!(
        reason.contains("strictt"),
        "must carry the toml error text (unknown variant `strictt`): {reason}"
    );
}

#[test]
fn gate_blocks_on_unparseable_project_config_bad_syntax() {
    let fx = fixture(Some(BAD_SYNTAX), None);
    trust(&fx, &fx.proj);
    let (stdout, stderr) = stop(&fx, &fx.proj);
    let reason = block_reason(&stdout)
        .unwrap_or_else(|| panic!("must block: stdout={stdout:?} stderr={stderr:?}"));
    let path = fx.proj.join("tdd.toml").to_string_lossy().into_owned();
    assert!(
        reason.contains(&path),
        "must name the file {path}: {reason}"
    );
    assert!(
        reason.to_lowercase().contains("parse"),
        "must say it is a parse error: {reason}"
    );
}

#[test]
fn red_and_green_fail_on_unparseable_project_config() {
    let fx = fixture(Some(BAD_ENUM), None);
    trust(&fx, &fx.proj);
    let path = fx.proj.join("tdd.toml").to_string_lossy().into_owned();
    let (code, _o, err) = run(&fx, &fx.proj, &["red", "--task", "t1"], "");
    assert_ne!(code, 0, "red must fail; stderr={err}");
    assert!(err.contains(&path), "red must name the file {path}: {err}");
    let (code, _o, err) = run(&fx, &fx.proj, &["green", "--task", "t1"], "");
    assert_ne!(code, 0, "green must fail; stderr={err}");
    assert!(
        err.contains(&path),
        "green must name the file {path}: {err}"
    );
}

#[test]
fn gate_blocks_on_unparseable_home_config() {
    let fx = fixture(None, Some(BAD_ENUM));
    let (stdout, stderr) = stop(&fx, &fx.proj);
    let reason = block_reason(&stdout)
        .unwrap_or_else(|| panic!("must block: stdout={stdout:?} stderr={stderr:?}"));
    let path = fx
        .home
        .join(".tdd")
        .join("config.toml")
        .to_string_lossy()
        .into_owned();
    assert!(
        reason.contains(&path),
        "must name the file {path}: {reason}"
    );
    assert!(
        reason.contains("strictt"),
        "must carry error text: {reason}"
    );
}

#[test]
fn red_and_green_fail_on_unparseable_home_config() {
    let fx = fixture(None, Some(BAD_SYNTAX));
    let path = fx
        .home
        .join(".tdd")
        .join("config.toml")
        .to_string_lossy()
        .into_owned();
    let (code, _o, err) = run(&fx, &fx.proj, &["red", "--task", "t1"], "");
    assert_ne!(code, 0, "red must fail; stderr={err}");
    assert!(err.contains(&path), "red must name the file {path}: {err}");
    let (code, _o, err) = run(&fx, &fx.proj, &["green", "--task", "t1"], "");
    assert_ne!(code, 0, "green must fail; stderr={err}");
    assert!(
        err.contains(&path),
        "green must name the file {path}: {err}"
    );
}

/// Control: no config anywhere => defaults; the clean repo is allowed, and a
/// failing --cmd is recorded as a RED (exit 0) as today.
#[test]
fn control_no_config_gate_allows_and_red_records() {
    let fx = fixture(None, None);
    let (stdout, stderr) = stop(&fx, &fx.proj);
    assert!(
        block_reason(&stdout).is_none(),
        "no config, clean repo: must not block: stdout={stdout:?} stderr={stderr:?}"
    );
    let (code, _o, err) = run(
        &fx,
        &fx.proj,
        &["red", "--task", "t1", "--cmd", "false"],
        "",
    );
    assert_eq!(code, 0, "failing cmd is a valid RED; stderr={err}");
}

/// Control: a VALID trusted project config is honoured -- its `test_cmd =
/// "false"` is what `tdd red` runs when --cmd is omitted, which is a valid RED.
#[test]
fn control_valid_trusted_project_config_is_honoured() {
    let fx = fixture(Some("test_cmd = \"false\"\n"), None);
    trust(&fx, &fx.proj);
    let (stdout, stderr) = stop(&fx, &fx.proj);
    assert!(
        block_reason(&stdout).is_none(),
        "valid config, clean repo: must not block: stdout={stdout:?} stderr={stderr:?}"
    );
    let (code, _o, err) = run(&fx, &fx.proj, &["red", "--task", "t1"], "");
    assert_eq!(
        code, 0,
        "test_cmd=false from config is a valid RED; stderr={err}"
    );
}
