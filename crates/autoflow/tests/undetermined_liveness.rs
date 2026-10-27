//! End-to-end: a driver-liveness answer autoflow could NOT obtain must never be
//! read as "allow the stop". Silence (empty stdout, exit 0) is what Claude Code
//! reads as a clean pass, so an undetermined liveness has to be visible: a
//! `block` on the first stop. Once `stop_hook_active` bounds the loop the stop
//! is allowed with exactly one short `systemMessage` line on stdout (operator
//! ruling 2026-10-27: results in one line, never logs) and recorded as a
//! `bounded_allow` line in `<state_dir>/log.jsonl`, so it stays
//! distinguishable from a clean pass.
//!
//! Controls (`control_*`) prove the harness reaches the branch: real
//! observations (`none`, a live driver, an explicit user stop) keep their
//! existing silent behaviour.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Env {
    home: PathBuf,
    repo: PathBuf,
    session: String,
}

impl Env {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "autoflow-liveness-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let env = Env {
            home,
            repo,
            session: format!("sess-{tag}"),
        };
        env.write_phase("record_requested");
        env
    }

    fn state_path(&self) -> PathBuf {
        self.home
            .join(".autoflow")
            .join("state")
            .join(format!("{}.json", self.session))
    }

    fn write_phase(&self, phase: &str) {
        let p = self.state_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!(r#"{{"phase":"{phase}"}}"#)).unwrap();
    }

    fn phase(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.state_path()).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(v.get("phase")?.as_str()?.to_string())
    }

    fn marker_path(&self) -> PathBuf {
        self.home
            .join(".autoflow")
            .join("state")
            .join(format!("{}.resume-flow", self.session))
    }

    fn stub_dir(&self) -> PathBuf {
        let d = self.home.join("stub-bin");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Stub `backlog`: `--version` and everything else exit 0; the `lock`
    /// subcommand runs `lock_body` (a shell snippet).
    fn stub_lock(&self, lock_body: &str) -> String {
        let dir = self.stub_dir();
        let script = format!("#!/bin/sh\ncase \"$1\" in\nlock) {lock_body} ;;\nesac\nexit 0\n");
        let path = dir.join("backlog");
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        format!("{}:/usr/bin:/bin", dir.to_string_lossy())
    }

    fn plugin_cache(&self) -> PathBuf {
        self.home.join(".claude/plugins/cache/yukineko/backlog")
    }

    fn run(
        &self,
        sub: &str,
        path: &str,
        session_in_payload: &str,
        active: Option<bool>,
        transcript: &str,
    ) -> (i32, String, String) {
        let active_field = match active {
            Some(b) => format!(r#","stop_hook_active":{b}"#),
            None => String::new(),
        };
        let event = if sub == "stop" { "Stop" } else { "PreCompact" };
        let payload = format!(
            r#"{{"hook_event_name":"{event}","session_id":"{session_in_payload}","cwd":"{}","transcript_path":"{transcript}"{active_field}}}"#,
            self.repo.to_string_lossy()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_autoflow"))
            .arg(sub)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("autoflow spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("autoflow runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn stop(&self, path: &str, active: Option<bool>) -> (i32, String, String) {
        self.run("stop", path, &self.session, active, "")
    }
}

fn json(stdout: &str) -> Option<serde_json::Value> {
    serde_json::from_str(stdout.trim()).ok()
}

fn blocks(stdout: &str) -> bool {
    json(stdout)
        .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
        .as_deref()
        == Some("block")
}

fn nonempty_str(stdout: &str, key: &str) -> bool {
    json(stdout)
        .and_then(|v| {
            v.get(key)
                .and_then(|d| d.as_str())
                .map(|s| !s.trim().is_empty())
        })
        .unwrap_or(false)
}

fn assert_visible_block(env: &Env, r: (i32, String, String)) {
    let (code, stdout, stderr) = r;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        blocks(&stdout) && nonempty_str(&stdout, "reason"),
        "undetermined liveness must BLOCK with a reason, not read as a clean stop; stdout: {stdout:?}"
    );
    assert_eq!(
        env.phase().as_deref(),
        Some("record_requested"),
        "undetermined liveness must not latch the phase"
    );
}

fn assert_bounded_allow_logged_no_block(env: &Env, r: (i32, String, String)) {
    let (code, stdout, stderr) = r;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        !blocks(&stdout),
        "bounded path must not block; stdout: {stdout:?}"
    );
    assert_eq!(
        stdout.trim().lines().count(),
        1,
        "bounded path prints exactly one line (ruling 2026-10-27); stdout: {stdout:?}"
    );
    let msg = json(&stdout).unwrap_or_else(|| panic!("stdout not JSON: {stdout:?}"));
    assert!(
        msg.get("decision").is_none(),
        "must not carry decision: {msg}"
    );
    assert_eq!(
        msg["systemMessage"], "autoflow: 判定不能のまま通しました",
        "stdout: {stdout:?}"
    );
    let log = env.home.join(".autoflow").join("state").join("log.jsonl");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let rec: Option<serde_json::Value> = text
        .lines()
        .last()
        .and_then(|l| serde_json::from_str(l).ok());
    assert!(
        rec.as_ref().is_some_and(|v| v["event"] == "bounded_allow"
            && v["what"].as_str().is_some_and(|s| !s.trim().is_empty())
            && v["why"].as_str().is_some_and(|s| !s.trim().is_empty())),
        "bounded allow must be recorded as a bounded_allow line in {} \
         (otherwise it is indistinguishable from a clean pass); log: {text:?}",
        log.display()
    );
    assert_ne!(env.phase().as_deref(), Some("done"));
}

const UNDET_OBJ: &str =
    r#"echo '{"kind":"undetermined","undetermined":true,"reason":"registry unreadable"}'"#;

// ---- (A) first stop, undetermined liveness -> block -------------------------

#[test]
fn a1_lock_exit_nonzero_blocks() {
    let env = Env::new("a1");
    let path = env.stub_lock("exit 1");
    assert_visible_block(&env, env.stop(&path, Some(false)));
}

#[test]
fn a2_lock_garbage_blocks() {
    let env = Env::new("a2");
    let path = env.stub_lock("echo 'garbage not json'; exit 0");
    assert_visible_block(&env, env.stop(&path, Some(false)));
}

#[test]
fn a3_lock_empty_blocks() {
    let env = Env::new("a3");
    let path = env.stub_lock("exit 0");
    assert_visible_block(&env, env.stop(&path, None));
}

#[test]
fn a4_lock_undetermined_object_blocks() {
    let env = Env::new("a4");
    let path = env.stub_lock(UNDET_OBJ);
    assert_visible_block(&env, env.stop(&path, Some(false)));
}

#[cfg(unix)]
#[test]
fn a5_unreadable_plugin_cache_blocks() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("a5");
    let cache = env.plugin_cache();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o111)).unwrap();
    let r = env.stop("/usr/bin:/bin", Some(false));
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_visible_block(&env, r);
}

// ---- (B) no session id ------------------------------------------------------

#[test]
fn b_no_session_id_blocks() {
    let env = Env::new("b");
    let (code, stdout, stderr) = env.run("stop", "/usr/bin:/bin", "", Some(false), "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        blocks(&stdout) && nonempty_str(&stdout, "reason"),
        "no session id is undeterminable and must BLOCK visibly; stdout: {stdout:?}"
    );
}

// ---- (C) stop_hook_active bound ---------------------------------------------

#[test]
fn c1_lock_exit_nonzero_bounded_allow_logged() {
    let env = Env::new("c1");
    let path = env.stub_lock("exit 1");
    assert_bounded_allow_logged_no_block(&env, env.stop(&path, Some(true)));
}

#[test]
fn c2_lock_garbage_bounded_allow_logged() {
    let env = Env::new("c2");
    let path = env.stub_lock("echo 'garbage not json'; exit 0");
    assert_bounded_allow_logged_no_block(&env, env.stop(&path, Some(true)));
}

#[test]
fn c3_no_session_id_bounded_allow_logged() {
    let env = Env::new("c3");
    let r = env.run("stop", "/usr/bin:/bin", "", Some(true), "");
    assert_bounded_allow_logged_no_block(&env, r);
}

// ---- (E) PreCompact ---------------------------------------------------------

fn assert_precompact_visible(env: &Env, r: (i32, String, String)) {
    let (code, stdout, stderr) = r;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        json(&stdout).is_some_and(|v| v.is_object()) && nonempty_str(&stdout, "systemMessage"),
        "undetermined liveness at PreCompact must be visible via systemMessage; stdout: {stdout:?}"
    );
    assert!(
        !env.marker_path().exists(),
        "no resume marker may be written"
    );
}

#[test]
fn e1_precompact_lock_exit_nonzero_visible() {
    let env = Env::new("e1");
    let path = env.stub_lock("exit 1");
    let r = env.run("pre-compact", &path, &env.session, None, "");
    assert_precompact_visible(&env, r);
}

#[test]
fn e2_precompact_lock_garbage_visible() {
    let env = Env::new("e2");
    let path = env.stub_lock("echo 'garbage not json'; exit 0");
    let r = env.run("pre-compact", &path, &env.session, None, "");
    assert_precompact_visible(&env, r);
}

#[test]
fn e3_precompact_no_session_id_visible() {
    let env = Env::new("e3");
    let r = env.run("pre-compact", "/usr/bin:/bin", "", None, "");
    assert_precompact_visible(&env, r);
}

// ---- (F) anti-vacuity controls (must be GREEN before and after) -------------

#[test]
fn control_lock_none_still_latches_done_silently() {
    let env = Env::new("f-none");
    let path = env.stub_lock("echo none");
    let (code, stdout, stderr) = env.stop(&path, Some(false));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.trim().is_empty(), "stdout: {stdout:?}");
    assert_eq!(env.phase().as_deref(), Some("done"));
}

#[test]
fn control_live_driver_stands_down_silently() {
    let env = Env::new("f-live");
    let path = env.stub_lock(
        r#"echo '{"kind":"driver-presence","session_id":"other","pid":1,"project":"/p","acquired_at":0,"heartbeat_at":9,"driver_count":1,"drivers":[{"session_id":"other"}]}'"#,
    );
    let (code, stdout, stderr) = env.stop(&path, Some(false));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.trim().is_empty(), "stdout: {stdout:?}");
    assert_eq!(env.phase().as_deref(), Some("record_requested"));
}

#[test]
fn control_precompact_lock_none_is_silent_no_marker() {
    let env = Env::new("f-pc");
    let path = env.stub_lock("echo none");
    let (code, stdout, stderr) = env.run("pre-compact", &path, &env.session, None, "");
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.trim().is_empty(), "stdout: {stdout:?}");
    assert!(!env.marker_path().exists());
}

#[test]
fn control_explicit_user_stop_wins_over_undetermined_liveness() {
    let env = Env::new("f-userstop");
    let path = env.stub_lock("exit 1");
    let tp = env.home.join("transcript.jsonl");
    let line = |role: &str, text: &str| {
        serde_json::json!({"message":{"role":role,"content":[{"type":"text","text":text}]}})
            .to_string()
    };
    std::fs::write(
        &tp,
        format!(
            "{}\n{}\n{}\n",
            line("user", "continue with the backlog"),
            line("assistant", "working on it"),
            line("user", "ok, please stop for now")
        ),
    )
    .unwrap();
    let (code, stdout, stderr) = env.run(
        "stop",
        &path,
        &env.session,
        Some(false),
        tp.to_str().unwrap(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        !blocks(&stdout),
        "an explicit user stop must still win; stdout: {stdout:?}"
    );
}

#[allow(dead_code)]
fn _unused(_: &Path) {}
