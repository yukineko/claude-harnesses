//! RED reproduction (backlog, 2026-11-21): `flow stop-gate` counts, as this
//! driver's remaining work, a pending backlog task whose condukt claim is held
//! by ANOTHER live session's run — a task the /flow skill's claim-skip gate
//! (crates/flow/skills/flow/SKILL.md Step 3 (a): `condukt state is-claimed
//! --hashkey <hashkey>` exit 0 → skip the item) forbids this session to take.
//! The stop is therefore blocked forever: the loop can never satisfy it.
//!
//! Observed live (main 11c04144d, session eff985d3): `backlog next --claim`
//! handed task 1564a3fc (hashkey c7a1bd84c0391373); `condukt state is-claimed
//! --hashkey c7a1bd84c0391373` exited 0 with `{"claimed": true, "holder_run":
//! "flow-62e92d16-..."}`; the session skipped it and `backlog unclaim`ed it.
//! Every later Stop was blocked with `... work remains: pending backlog task
//! 1564a3fc ...`.
//!
//! Written without fixing the defect. The stub protocol mirrors
//! `tests/stop_gate.rs`, plus one key:
//!
//! | `condukt state is-claimed ...` | `ctl/is_claimed.out` | `ctl/is_claimed.code` |
//!
//! DOES NOT PROVE: which hashkey a fix must query (the stub answers any
//! `is-claimed` call alike), nor behaviour with several pending tasks where
//! only some are claimed elsewhere.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const SESSION: &str = "sess-flow-driver-eff985d3";
const OTHER_RUN: &str = "flow-62e92d16-0aef-402b-ad58-39425ad07ded";
const TASK_ID: &str = "1564a3fc";
const HASHKEY: &str = "c7a1bd84c0391373";

const STUB: &str = r#"#!/bin/sh
here=$(cd "$(dirname "$0")" && pwd)
tool=$(basename "$0")
printf '%s\t%s' "$tool" "$(pwd)" >> "$here/calls.log"
for a in "$@"; do printf '\t%s' "$a" >> "$here/calls.log"; done
printf '\n' >> "$here/calls.log"
key=""
case "$tool:$1:$2" in
  backlog:driver:status) key=driver_status ;;
  condukt:state:claims) key=claims ;;
  condukt:state:is-claimed) key=is_claimed ;;
  condukt:circuit:check) key=circuit ;;
esac
if [ -z "$key" ] && [ "$tool:$1" = "backlog:next" ]; then key=next; fi
[ -z "$key" ] && exit 0
[ -f "$here/ctl/$key.out" ] && cat "$here/ctl/$key.out"
code=0
[ -f "$here/ctl/$key.code" ] && code=$(cat "$here/ctl/$key.code")
exit "$code"
"#;

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    stubs: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize tempdir");
        let home = root.join("home");
        let stubs = root.join("stubs");
        let project = root.join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(stubs.join("ctl")).unwrap();
        fs::create_dir_all(&project).unwrap();
        for name in ["backlog", "condukt"] {
            let p = stubs.join(name);
            fs::write(&p, STUB).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Fixture {
            _tmp: tmp,
            home,
            stubs,
            project,
        }
    }

    fn project_str(&self) -> String {
        self.project.to_string_lossy().into_owned()
    }

    fn set(&self, key: &str, out: &str, code: i32) {
        fs::write(self.stubs.join("ctl").join(format!("{key}.out")), out).unwrap();
        fs::write(
            self.stubs.join("ctl").join(format!("{key}.code")),
            code.to_string(),
        )
        .unwrap();
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.stubs.join("calls.log")).unwrap_or_default()
    }

    /// Live driver registration for SESSION, one pending task with HASHKEY,
    /// no claims held by SESSION's own run, circuit continue.
    fn live_driver_with_pending_task(&self) {
        let slug = format!(
            "{:016x}",
            harness_core::hash::fnv1a64(self.project_str().as_bytes())
        );
        let bucket = self.home.join(".backlog").join("drivers").join(slug);
        fs::create_dir_all(&bucket).unwrap();
        let now = unix_now();
        let rec = json!({
            "session_id": SESSION,
            "pid": std::process::id(),
            "project": self.project_str(),
            "registered_at": now,
            "heartbeat_at": now,
        });
        fs::write(
            bucket.join(format!(
                "{:016x}.driver",
                harness_core::hash::fnv1a64(SESSION.as_bytes())
            )),
            rec.to_string(),
        )
        .unwrap();
        let status = json!({
            "active": true,
            "count": 1,
            "drivers": [rec],
            "stale": [],
            "project": self.project_str(),
            "undetermined": false,
        });
        self.set("driver_status", &status.to_string(), 0);
        let task = json!({
            "id": TASK_ID,
            "title": "rollout: pending plugin rollout",
            "hashkey": HASHKEY,
            "status": "pending",
            "priority": 1,
        });
        self.set("next", &format!("{task}\n"), 0);
        // SESSION's own run (`flow-<SESSION>`) holds nothing.
        self.set("claims", "{}\n", 0);
        let circuit = json!({"verdict": "continue", "reason": null, "streak": 0});
        self.set("circuit", &format!("{circuit}\n"), 0);
    }

    fn is_claimed_by_other(&self) {
        // The OTHER session's run holds the condukt claim on HASHKEY.
        let claims = json!({ HASHKEY: { "run_id": OTHER_RUN } });
        self.set("claims", &format!("{claims}\n"), 0);
        let out = json!({"claimed": true, "holder_run": OTHER_RUN});
        self.set("is_claimed", &format!("{out}\n"), 0);
    }

    fn is_not_claimed(&self) {
        self.set("is_claimed", "{\"claimed\": false}\n", 1);
    }

    fn run(&self) -> (Option<i32>, String, String) {
        let payload = json!({
            "session_id": SESSION,
            "cwd": self.project_str(),
            "stop_hook_active": false,
            "hook_event_name": "Stop",
            "transcript_path": "",
        });
        let path = format!(
            "{}:{}",
            self.stubs.display(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into())
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_flow"))
            .arg("stop-gate")
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("HARNESS_SANDBOX_HOME", &self.home)
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("CLAUDE_CONFIG_DIR", self.home.join(".claude"))
            .env("CLAUDE_CODE_SESSION_ID", SESSION)
            .env("PATH", path)
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .env_remove("BACKLOG_DISABLE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn flow stop-gate");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("wait flow stop-gate");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn blocked(stdout: &str) -> bool {
    serde_json::from_str::<Value>(stdout.trim())
        .ok()
        .and_then(|v| v.get("decision").and_then(Value::as_str).map(str::to_owned))
        .as_deref()
        == Some("block")
}

/// RED target: the only pending task is claimed by another live session's
/// condukt run (is-claimed exit 0) and this session holds no claims. The
/// skill forbids this session to take it, so it is not this session's
/// remaining work: the stop must not be blocked.
#[test]
fn stop_is_allowed_when_only_pending_task_is_claimed_by_another_session() {
    let f = Fixture::new();
    f.live_driver_with_pending_task();
    f.is_claimed_by_other();
    let (code, stdout, stderr) = f.run();
    assert_eq!(
        code,
        Some(0),
        "hook mode exits 0\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !blocked(&stdout),
        "a task whose condukt claim is held by another live session ({OTHER_RUN}) is not \
         this driver's work (SKILL Step 3 claim-skip gate), so the stop must not be blocked\n\
         stdout={stdout}\nstderr={stderr}\ncalls:\n{}",
        f.calls()
    );
}

/// Control: identical setup but the task is NOT claimed anywhere
/// (is-claimed exit 1, `"claimed": false`) — real remaining work, so the stop
/// stays blocked. Pins that the RED target is about the claim, not the setup.
#[test]
fn control_stop_is_blocked_when_pending_task_is_unclaimed() {
    let f = Fixture::new();
    f.live_driver_with_pending_task();
    f.is_not_claimed();
    let (code, stdout, stderr) = f.run();
    assert_eq!(
        code,
        Some(0),
        "hook mode exits 0\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        blocked(&stdout),
        "an unclaimed pending task is remaining work: the stop must be blocked\n\
         stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains(TASK_ID),
        "the block reason names the pending task\nstdout={stdout}"
    );
}
