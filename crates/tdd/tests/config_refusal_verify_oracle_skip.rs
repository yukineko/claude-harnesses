#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `tdd verify`, `tdd oracle` and `tdd skip` must refuse (exit non-zero, stderr
//! naming the config file) when the chosen config cannot be trusted to be read:
//! (a) it exists but does not parse (trusted project `tdd.toml`, or home
//! `~/.tdd/config.toml`), or (b) a project `tdd.toml` exists but the root is
//! not trusted. Otherwise they silently read proofs from / write state to the
//! built-in default directories (CLAUDE.md section 3: cannot-determine
//! resolves to the restrictive side).
//!
//! Anti-vacuity: valid RED+GREEN proofs are recorded in the default `.tdd`
//! dir BEFORE the bad config is added, so verify/oracle would exit 0 under
//! defaults; a non-zero exit can only come from the config refusal.
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

/// A clean repo with one commit and NO config anywhere.
fn fixture() -> Fx {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("tdd-cfgrefuse-{}-{n}", std::process::id()));
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
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-qm", "seed"]);
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

fn run(fx: &Fx, args: &[&str]) -> (i32, String, String) {
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .args(args)
        .current_dir(&fx.proj)
        .env("HOME", &fx.home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .env("CLAUDE_CODE_SESSION_ID", "s-cfgrefuse")
        .env_remove("TDD_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tdd spawns");
    child.stdin.take().unwrap().write_all(b"").unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Record a valid RED then GREEN for task `t1` under defaults (no config).
fn record_proofs(fx: &Fx) {
    // One and the same command for RED and GREEN (oracle requires it): it
    // fails until marker.txt exists.
    let cmd = "test -f marker.txt";
    let (code, _o, err) = run(
        fx,
        &["red", "--task", "t1", "--cmd", cmd, "--author", "author-a"],
    );
    assert_eq!(code, 0, "apparatus: RED must record; stderr={err}");
    std::fs::write(fx.proj.join("marker.txt"), "x\n").unwrap();
    let (code, _o, err) = run(
        fx,
        &[
            "green", "--task", "t1", "--cmd", cmd, "--author", "author-b",
        ],
    );
    assert_eq!(code, 0, "apparatus: GREEN must record; stderr={err}");
    // Apparatus: under defaults, verify and oracle accept the proofs.
    let (code, _o, err) = run(fx, &["verify", "--task", "t1"]);
    assert_eq!(code, 0, "apparatus: verify accepts defaults; stderr={err}");
    let (code, _o, err) = run(fx, &["oracle", "--task", "t1"]);
    assert_eq!(code, 0, "apparatus: oracle accepts defaults; stderr={err}");
}

const BAD_ENUM: &str = "mode = \"strictt\"\n";
const UNTRUSTED_CFG: &str = "test_cmd = \"true\"\n";

fn project_cfg_path(fx: &Fx) -> String {
    fx.proj.join("tdd.toml").to_string_lossy().into_owned()
}

fn home_cfg_path(fx: &Fx) -> String {
    fx.home
        .join(".tdd")
        .join("config.toml")
        .to_string_lossy()
        .into_owned()
}

/// Proofs recorded; then a broken TRUSTED project config.
fn broken_project() -> (Fx, String) {
    let fx = fixture();
    record_proofs(&fx);
    std::fs::write(fx.proj.join("tdd.toml"), BAD_ENUM).unwrap();
    trust(&fx, &fx.proj);
    let p = project_cfg_path(&fx);
    (fx, p)
}

/// Proofs recorded; then a broken HOME config (no project config).
fn broken_home() -> (Fx, String) {
    let fx = fixture();
    record_proofs(&fx);
    let d = fx.home.join(".tdd");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("config.toml"), BAD_ENUM).unwrap();
    let p = home_cfg_path(&fx);
    (fx, p)
}

/// Proofs recorded; then a VALID project config in an UNTRUSTED root.
fn untrusted_project() -> (Fx, String) {
    let fx = fixture();
    record_proofs(&fx);
    std::fs::write(fx.proj.join("tdd.toml"), UNTRUSTED_CFG).unwrap();
    let p = project_cfg_path(&fx);
    (fx, p)
}

fn assert_refuses(fx: &Fx, args: &[&str], path: &str) {
    let (code, out, err) = run(fx, args);
    assert_ne!(
        code, 0,
        "{args:?} must refuse; stdout={out:?} stderr={err:?}"
    );
    assert!(
        err.contains(path),
        "{args:?} stderr must name the config file {path}: {err}"
    );
}

const SKIP: [&str; 3] = ["skip", "--reason", "test"];

// ---- (a) unparseable trusted project config ----

#[test]
fn verify_refuses_on_unparseable_project_config() {
    let (fx, p) = broken_project();
    assert_refuses(&fx, &["verify", "--task", "t1"], &p);
}

#[test]
fn oracle_refuses_on_unparseable_project_config() {
    let (fx, p) = broken_project();
    assert_refuses(&fx, &["oracle", "--task", "t1"], &p);
}

// ---- (a') unparseable home config ----

#[test]
fn verify_refuses_on_unparseable_home_config() {
    let (fx, p) = broken_home();
    assert_refuses(&fx, &["verify", "--task", "t1"], &p);
}

#[test]
fn oracle_refuses_on_unparseable_home_config() {
    let (fx, p) = broken_home();
    assert_refuses(&fx, &["oracle", "--task", "t1"], &p);
}

// ---- (b) untrusted project tdd.toml ----

#[test]
fn verify_refuses_on_untrusted_project_config() {
    let (fx, p) = untrusted_project();
    assert_refuses(&fx, &["verify", "--task", "t1"], &p);
}

#[test]
fn oracle_refuses_on_untrusted_project_config() {
    let (fx, p) = untrusted_project();
    assert_refuses(&fx, &["oracle", "--task", "t1"], &p);
}

// ---- controls ----

fn stop(fx: &Fx) -> String {
    let payload = serde_json::json!({
        "session_id": "s-cfgrefuse",
        "cwd": fx.proj.to_string_lossy(),
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let mut child = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_tdd")))
        .arg("gate")
        .current_dir(&fx.proj)
        .env("HOME", &fx.home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .env("CLAUDE_CODE_SESSION_ID", "s-cfgrefuse")
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
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn is_block(stdout: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .map(|v| v["decision"] == "block")
        .unwrap_or(false)
}

/// Control (per ruling: `tdd skip` is the documented one-stop escape and must
/// NOT be refused): with a broken trusted config, skip exits 0 and the next
/// Stop of the same session allows once; the one after blocks again.
#[test]
fn control_skip_still_works_with_broken_config_and_allows_one_stop() {
    let (fx, p) = broken_project();
    // No stop is run before the skip: the first stop of a session on a broken
    // config blocks (see config_parse_error_blocks.rs), so an allow here is
    // attributable to the skip, not to the repeat-finding bound.
    let (code, _o, err) = run(&fx, &SKIP);
    assert_eq!(code, 0, "skip must still succeed; stderr={err}");
    let after = stop(&fx);
    assert!(
        !is_block(&after),
        "the stop right after skip must be allowed ({p}): {after}"
    );
}

/// Anti-vacuity for the control above: the same fixture WITHOUT a skip blocks
/// on its first stop.
#[test]
fn control_broken_config_first_stop_blocks_without_skip() {
    let (fx, p) = broken_project();
    let first = stop(&fx);
    assert!(is_block(&first), "broken config must block ({p}): {first}");
}

/// Control: no config, valid proofs => verify and oracle exit 0, skip exits 0.
#[test]
fn control_no_config_verify_oracle_skip_succeed() {
    let fx = fixture();
    record_proofs(&fx);
    let (code, _o, err) = run(&fx, &["verify", "--task", "t1"]);
    assert_eq!(code, 0, "verify; stderr={err}");
    let (code, _o, err) = run(&fx, &["oracle", "--task", "t1"]);
    assert_eq!(code, 0, "oracle; stderr={err}");
    let (code, _o, err) = run(&fx, &SKIP);
    assert_eq!(code, 0, "skip; stderr={err}");
}

/// Control: the SAME valid project config in a TRUSTED root is honoured, so
/// the untrusted-case refusals above are caused by trust, not by the config
/// merely existing.
#[test]
fn control_trusted_valid_project_config_verify_oracle_skip_succeed() {
    let fx = fixture();
    record_proofs(&fx);
    std::fs::write(fx.proj.join("tdd.toml"), UNTRUSTED_CFG).unwrap();
    trust(&fx, &fx.proj);
    let (code, _o, err) = run(&fx, &["verify", "--task", "t1"]);
    assert_eq!(code, 0, "verify; stderr={err}");
    let (code, _o, err) = run(&fx, &["oracle", "--task", "t1"]);
    assert_eq!(code, 0, "oracle; stderr={err}");
    let (code, _o, err) = run(&fx, &SKIP);
    assert_eq!(code, 0, "skip; stderr={err}");
}
