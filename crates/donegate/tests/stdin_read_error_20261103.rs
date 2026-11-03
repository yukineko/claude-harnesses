#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-11-03 (backlog 8004d80f): a stdin READ ERROR is a failure
//! ("cannot determine"), never an empty or partial `Ok` payload, and a
//! verdict-bearing caller must block or ask on it.
//!
//! `donegate gate` is a Stop gate. Its private `read_stdin` used to discard the
//! read error (`let _ = ...read_to_string(..)`), turning it into an empty
//! payload, which `HookInput::parse` maps to `None` -> "manual CLI mode" ->
//! with no config declared, a quiet allow. The read error is thereby emitted as
//! a clean stop.
//!
//! Expected (harness-core `gate::run::read_stop_payload_guarded`): on a
//! chain-first stop (fresh isolated HOME, so no panic/read-error marker) the
//! process exits 0 with stdout carrying `{"decision":"block","reason":...}`
//! whose reason contains "stdin read failed". Never allow, never manual-CLI
//! mode (exit 1 / stderr-only), never a raw panic (exit 101). Without a
//! session id every such stop blocks (no bounded allow is possible), which the
//! second test pins.
//!
//! Apparatus: the real binary (`CARGO_BIN_EXE_donegate`) is spawned with fd 0
//! opened on a DIRECTORY (`read(2)` -> EISDIR on macOS and Linux). The control
//! runs the same helper with a valid Stop payload piped in (no config, not a
//! git repo) and requires a non-block allow, so the helper is shown able to
//! tell the outcomes apart.
//!
//! Written by a disinterested author (CLAUDE.md section 2(a)).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(self.stdout.trim())
            .ok()
            .or_else(|| {
                self.stdout
                    .lines()
                    .rev()
                    .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
            })
    }
    fn field(&self, k: &str) -> String {
        self.json()
            .as_ref()
            .and_then(|v| v.get(k))
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string()
    }
}

fn scratch_dir() -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("dg-stdin-err-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// `payload`: `None` -> stdin is a directory fd (read fails with EISDIR);
/// `Some(p)` -> stdin is a pipe carrying `p`.
/// `session`: value for `CLAUDE_CODE_SESSION_ID`, or `None` to unset it.
fn run_gate(payload: Option<&str>, session: Option<&str>) -> Run {
    let home = scratch_dir();
    let cwd = home.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_donegate"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("gate")
        .current_dir(&cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".state"))
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("DONEGATE_DISABLE")
        .env_remove("HARNESS_TRUST_ALL")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match session {
        Some(s) => {
            cmd.env("CLAUDE_CODE_SESSION_ID", s);
        }
        None => {
            cmd.env_remove("CLAUDE_CODE_SESSION_ID");
        }
    }
    let out = match payload {
        None => {
            let dir_fd = std::fs::File::open(&home).expect("open temp dir as a file");
            cmd.stdin(Stdio::from(dir_fd));
            cmd.output().expect("run donegate gate")
        }
        Some(p) => {
            cmd.stdin(Stdio::piped());
            let mut child = cmd.spawn().expect("spawn donegate gate");
            child.stdin.take().unwrap().write_all(p.as_bytes()).unwrap();
            child.wait_with_output().expect("wait donegate gate")
        }
    };
    let _ = std::fs::remove_dir_all(&home);
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn assert_stdin_read_block(run: &Run) {
    assert_ne!(
        run.code,
        Some(101),
        "a stdin read error must be resolved to a block, not a raw panic (exit 101); \
         stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );
    assert_eq!(
        run.code,
        Some(0),
        "Stop hook blocks via stdout JSON + exit 0 (not manual-CLI exit 1); \
         stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );
    assert_eq!(
        run.field("decision"),
        "block",
        "an unreadable Stop payload is 'cannot determine' -> must block, never allow; \
         stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );
    let reason = run.field("reason");
    assert!(
        reason.contains("stdin read failed"),
        "the block reason must name the stdin read failure (\"stdin read failed\"); \
         reason={reason:?}"
    );
}

#[test]
fn stdin_read_error_blocks_chain_first_stop_with_session_id() {
    let run = run_gate(None, Some("dg-stdin-err-session"));
    assert_stdin_read_block(&run);
}

#[test]
fn stdin_read_error_blocks_without_session_id() {
    let run = run_gate(None, None);
    assert_stdin_read_block(&run);
}

/// Control: same helper, a valid Stop payload, nothing declared -> allow
/// (no `decision:block`), exit 0.
#[test]
fn control_valid_stop_payload_with_no_config_allows() {
    let payload = serde_json::json!({
        "session_id": "dg-stdin-ctl",
        "hook_event_name": "Stop",
        "stop_hook_active": false,
    })
    .to_string();
    let run = run_gate(Some(&payload), Some("dg-stdin-ctl"));
    assert_eq!(
        run.code,
        Some(0),
        "stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );
    assert_ne!(
        run.field("decision"),
        "block",
        "valid payload with nothing declared must allow; stdout={:?} stderr={:?}",
        run.stdout,
        run.stderr
    );
}
