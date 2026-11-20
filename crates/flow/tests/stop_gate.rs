//! Integration tests for `flow stop-gate`, the Stop hook that keeps a
//! registered /flow driver session from stopping while work remains.
//!
//! User ruling 2026-11-21: 「flowは問題解決がおわるまで止まることがないようにすること。
//! 問題解決とはbacklogなりissuesなりがなくなるまですすめること。」 The only
//! legitimate stop reason for a live driver with remaining work is the condukt
//! circuit breaker.
//!
//! Written by an independent test author (not the implementer), CLAUDE.md §2(a).
//!
//! # Hermeticity
//!
//! Every run gets a fresh tempdir `HOME` (also `HARNESS_SANDBOX_HOME` and the
//! XDG dirs), so the real `~/.backlog`, `~/.condukt` and `~/.claude` are never
//! read or written.
//!
//! # Stub protocol (for the implementer)
//!
//! `backlog` and `condukt` are shell stubs placed FIRST on `PATH`. They are
//! controlled by files in `<stubdir>/ctl/` (resolved relative to the stub's own
//! location, so they work even if the hook scrubs the child environment):
//!
//! | invocation                        | stdout file              | exit-code file            |
//! |-----------------------------------|--------------------------|---------------------------|
//! | `backlog driver status ...`       | `ctl/driver_status.out`  | `ctl/driver_status.code`  |
//! | `backlog next ...`                | `ctl/next.out`           | `ctl/next.code`           |
//! | `condukt state claims ...`        | `ctl/claims.out`         | `ctl/claims.code`         |
//! | `condukt circuit check ...`       | `ctl/circuit.out`        | `ctl/circuit.code`        |
//!
//! A missing `.out` prints nothing; a missing `.code` exits 0. Any other
//! subcommand prints nothing and exits 0. Every invocation appends one line
//! `<tool>\t<cwd>\t<argv...>` to `<stubdir>/calls.log`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const SESSION: &str = "sess-flow-driver-0001";
const OTHER_SESSION: &str = "sess-someone-else-9999";
const TASK_ID: &str = "a1b2c3d4";
const TASK_TITLE: &str = "fix the frobnicator";

// ─── fixture ────────────────────────────────────────────────────────────────

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    stubs: PathBuf,
    project: PathBuf,
    transcript: PathBuf,
}

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
  condukt:circuit:check) key=circuit ;;
esac
if [ -z "$key" ] && [ "$tool:$1" = "backlog:next" ]; then key=next; fi
[ -z "$key" ] && exit 0
[ -f "$here/ctl/$key.out" ] && cat "$here/ctl/$key.out"
code=0
[ -f "$here/ctl/$key.code" ] && code=$(cat "$here/ctl/$key.code")
exit "$code"
"#;

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
        let transcript = root.join("transcript.jsonl");
        fs::write(&transcript, "").unwrap();
        Fixture {
            _tmp: tmp,
            home,
            stubs,
            project,
            transcript,
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

    fn drivers_bucket(&self) -> PathBuf {
        // Any bucket name works: the hook must scan every subdir. Use a
        // slug-shaped name (16 lowercase hex) as backlog's project_slug does.
        let slug = format!(
            "{:016x}",
            harness_core::hash::fnv1a64(self.project_str().as_bytes())
        );
        self.home.join(".backlog").join("drivers").join(slug)
    }

    fn driver_path(&self, session: &str) -> PathBuf {
        self.drivers_bucket().join(format!(
            "{:016x}.driver",
            harness_core::hash::fnv1a64(session.as_bytes())
        ))
    }

    fn register(&self, session: &str) {
        fs::create_dir_all(self.drivers_bucket()).unwrap();
        let now = unix_now();
        let rec = json!({
            "session_id": session,
            "pid": std::process::id(),
            "project": self.project_str(),
            "registered_at": now,
            "heartbeat_at": now,
        });
        fs::write(self.driver_path(session), rec.to_string()).unwrap();
    }

    /// `backlog driver status` reports `session` as a live driver.
    fn status_live(&self, session: &str) {
        let now = unix_now();
        let out = json!({
            "active": true,
            "count": 1,
            "drivers": [{
                "session_id": session,
                "pid": std::process::id(),
                "project": self.project_str(),
                "registered_at": now,
                "heartbeat_at": now,
            }],
            "stale": [],
            "project": self.project_str(),
            "undetermined": false,
        });
        self.set("driver_status", &out.to_string(), 0);
    }

    fn status_stale(&self, session: &str) {
        let old = unix_now() - 100_000;
        let out = json!({
            "active": false,
            "count": 0,
            "drivers": [],
            "stale": [{
                "session_id": session,
                "pid": std::process::id(),
                "project": self.project_str(),
                "registered_at": old,
                "heartbeat_at": old,
            }],
            "project": self.project_str(),
            "undetermined": false,
        });
        self.set("driver_status", &out.to_string(), 0);
    }

    fn status_undetermined(&self) {
        let out = json!({
            "active": false,
            "count": 0,
            "drivers": [],
            "stale": [],
            "project": self.project_str(),
            "undetermined": true,
        });
        self.set("driver_status", &out.to_string(), 0);
    }

    fn next_task(&self) {
        let out = json!({
            "id": TASK_ID,
            "title": TASK_TITLE,
            "hashkey": format!("hk-{TASK_ID}"),
            "status": "pending",
            "priority": 1,
        });
        self.set("next", &format!("{out}\n"), 0);
    }

    fn next_none(&self) {
        self.set("next", "no pending tasks\n", 0);
    }

    fn next_unjudgeable(&self) {
        self.set("next", "", 1);
    }

    fn claims_empty(&self) {
        self.set("claims", "{}\n", 0);
    }

    fn claims_json(&self, v: Value) {
        self.set("claims", &format!("{v}\n"), 0);
    }

    fn circuit_continue(&self) {
        let out = json!({"verdict": "continue", "reason": null, "streak": 0});
        self.set("circuit", &format!("{out}\n"), 0);
    }

    fn circuit_trip(&self) {
        let out = json!({"verdict": "trip", "reason": "failure_streak", "streak": 5});
        self.set("circuit", &format!("{out}\n"), 1);
    }

    /// Fully determinable state with remaining work (case 3).
    fn live_with_task(&self) {
        self.register(SESSION);
        self.status_live(SESSION);
        self.next_task();
        self.claims_empty();
        self.circuit_continue();
    }

    fn run(&self, stop_hook_active: bool) -> Outcome {
        let payload = json!({
            "session_id": SESSION,
            "cwd": self.project_str(),
            "stop_hook_active": stop_hook_active,
            "hook_event_name": "Stop",
            "transcript_path": self.transcript.to_string_lossy(),
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
        Outcome {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Outcome {
    /// The stdout JSON object, if stdout carries one.
    fn json(&self) -> Option<Value> {
        let t = self.stdout.trim();
        if t.is_empty() {
            return None;
        }
        serde_json::from_str(t).ok()
    }

    fn blocked(&self) -> bool {
        self.json()
            .map(|v| v.get("decision").and_then(Value::as_str) == Some("block"))
            .unwrap_or(false)
    }

    fn reason(&self) -> String {
        self.json()
            .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default()
    }

    fn system_message(&self) -> String {
        self.json()
            .and_then(|v| {
                v.get("systemMessage")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default()
    }

    fn dbg(&self) -> String {
        format!(
            "exit={:?}\nstdout={}\nstderr={}",
            self.code, self.stdout, self.stderr
        )
    }

    fn assert_exit0(&self) {
        assert_eq!(self.code, Some(0), "hook mode must exit 0\n{}", self.dbg());
    }

    fn assert_block(&self) {
        self.assert_exit0();
        assert!(
            self.blocked(),
            "expected {{\"decision\":\"block\"}}\n{}",
            self.dbg()
        );
        assert!(
            !self.reason().trim().is_empty(),
            "block must carry a non-empty reason\n{}",
            self.dbg()
        );
    }

    fn assert_allow(&self) {
        self.assert_exit0();
        assert!(
            !self.stdout.contains("\"block\""),
            "expected allow (no block decision)\n{}",
            self.dbg()
        );
        // Stdout is either empty or a JSON object (e.g. systemMessage), never
        // free text that the harness cannot parse.
        if !self.stdout.trim().is_empty() {
            assert!(
                self.json().is_some_and(|v| v.is_object()),
                "non-empty allow stdout must be a JSON object\n{}",
                self.dbg()
            );
        }
    }
}

// ─── 1, 2: not a driver ─────────────────────────────────────────────────────

#[test]
fn not_a_driver_no_registry_allows_without_invoking_tools() {
    let f = Fixture::new();
    // Even with work present, a non-driver session is not gated.
    f.status_live(SESSION);
    f.next_task();
    f.claims_empty();
    f.circuit_continue();
    let o = f.run(false);
    o.assert_allow();
    assert!(
        f.calls().is_empty(),
        "no stub may be invoked for a non-driver session; calls:\n{}",
        f.calls()
    );
}

#[test]
fn only_another_sessions_registration_allows_without_invoking_tools() {
    let f = Fixture::new();
    f.register(OTHER_SESSION);
    f.status_live(OTHER_SESSION);
    f.next_task();
    f.claims_empty();
    f.circuit_continue();
    let o = f.run(false);
    o.assert_allow();
    assert!(
        f.calls().is_empty(),
        "no stub may be invoked when only another session is registered; calls:\n{}",
        f.calls()
    );
}

// ─── 3, 7: live driver with pending work ────────────────────────────────────

#[test]
fn live_driver_with_pending_task_blocks_and_says_what_to_do() {
    let f = Fixture::new();
    f.live_with_task();
    let o = f.run(false);
    o.assert_block();
    let r = o.reason();
    assert!(
        r.contains(TASK_ID) || r.contains(TASK_TITLE),
        "reason must name the pending task (id or title)\n{}",
        o.dbg()
    );
    assert!(
        r.contains("flow"),
        "reason must tell the driver to continue /flow\n{}",
        o.dbg()
    );
    assert!(
        r.contains("circuit") || r.contains("unregister"),
        "reason must name the legitimate ways out (circuit breaker / unregister)\n{}",
        o.dbg()
    );
}

#[test]
fn live_driver_with_pending_task_blocks_on_reentry_too() {
    // stop_hook_active=true with a determined "work remains" must still block:
    // the bounded allow is only for cannot-determine.
    let f = Fixture::new();
    f.live_with_task();
    f.run(false).assert_block();
    f.run(true).assert_block();
}

#[test]
fn circuit_check_is_invoked_with_flow_run_session_and_no_compaction_axis() {
    let f = Fixture::new();
    f.live_with_task();
    let o = f.run(false);
    o.assert_block();
    let calls = f.calls();
    let circuit: Vec<Vec<&str>> = calls
        .lines()
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|cols| {
            cols.first() == Some(&"condukt")
                && cols.get(2) == Some(&"circuit")
                && cols.get(3) == Some(&"check")
        })
        .collect();
    assert!(
        !circuit.is_empty(),
        "condukt circuit check must be called; calls:\n{calls}"
    );
    let run_id = format!("flow-{SESSION}");
    for argv in &circuit {
        let args = &argv[2..];
        let has_pair = |flag: &str, val: &str| {
            args.windows(2).any(|w| w[0] == flag && w[1] == val)
                || args.iter().any(|a| *a == format!("{flag}={val}"))
        };
        assert!(
            has_pair("--run", &run_id),
            "missing --run {run_id}: {args:?}"
        );
        assert!(
            has_pair("--session", SESSION),
            "missing --session {SESSION}: {args:?}"
        );
        assert!(
            args.contains(&"--no-compaction-axis"),
            "missing --no-compaction-axis: {args:?}"
        );
    }
}

// ─── 4, 5: claims ───────────────────────────────────────────────────────────

#[test]
fn live_driver_no_pending_but_own_flow_claim_blocks() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_live(SESSION);
    f.next_none();
    let now = unix_now();
    f.claims_json(json!({
        format!("hk-{TASK_ID}"): {
            "run_id": format!("flow-{SESSION}"),
            "session_id": SESSION,
            "pid": std::process::id(),
            "claimed_at": now,
            "heartbeat_at": now,
        }
    }));
    f.circuit_continue();
    f.run(false).assert_block();
}

#[test]
fn live_driver_no_pending_and_only_other_runs_claims_allows() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_live(SESSION);
    f.next_none();
    let now = unix_now();
    f.claims_json(json!({
        "hk-zzz": {
            "run_id": "run-20261121-000000-1",
            "session_id": OTHER_SESSION,
            "pid": 1,
            "claimed_at": now,
            "heartbeat_at": now,
        },
        "crates/foo/src/lib.rs": {
            "run_id": format!("flow-{OTHER_SESSION}"),
            "session_id": OTHER_SESSION,
            "pid": 1,
            "claimed_at": now,
            "heartbeat_at": now,
        }
    }));
    f.circuit_continue();
    f.run(false).assert_allow();
}

// ─── 6: circuit trip is the only way out ────────────────────────────────────

#[test]
fn pending_work_but_circuit_tripped_allows_with_system_message() {
    let f = Fixture::new();
    f.live_with_task();
    f.circuit_trip();
    let o = f.run(false);
    o.assert_allow();
    assert!(
        o.system_message().to_lowercase().contains("circuit"),
        "systemMessage must mention the circuit trip\n{}",
        o.dbg()
    );
}

// ─── 8: stale registration ──────────────────────────────────────────────────

#[test]
fn stale_registration_allows() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_stale(SESSION);
    f.next_task();
    f.claims_empty();
    f.circuit_continue();
    f.run(false).assert_allow();
}

// ─── 9: undetermined, bounded ───────────────────────────────────────────────

fn assert_undetermined_block(o: &Outcome) {
    o.assert_block();
    let r = o.reason().to_lowercase();
    assert!(
        r.contains("determine"),
        "undetermined block reason must say it could not determine\n{}",
        o.dbg()
    );
}

#[test]
fn backlog_next_unjudgeable_blocks_first_then_bounded_allow() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_live(SESSION);
    f.next_unjudgeable();
    f.claims_empty();
    f.circuit_continue();
    let first = f.run(false);
    assert_undetermined_block(&first);
    let second = f.run(true);
    second.assert_allow();
    assert!(
        !second.system_message().trim().is_empty(),
        "bounded allow must be explicit (systemMessage)\n{}",
        second.dbg()
    );
}

#[test]
fn claims_exit_nonzero_blocks_first_stop() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_live(SESSION);
    f.next_none();
    f.set("claims", "", 3);
    f.circuit_continue();
    assert_undetermined_block(&f.run(false));
}

#[test]
fn driver_status_undetermined_blocks_first_stop() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_undetermined();
    f.next_none();
    f.claims_empty();
    f.circuit_continue();
    assert_undetermined_block(&f.run(false));
}

#[test]
fn driver_status_nonzero_exit_blocks_first_stop() {
    let f = Fixture::new();
    f.register(SESSION);
    f.set("driver_status", "", 2);
    f.next_none();
    f.claims_empty();
    f.circuit_continue();
    assert_undetermined_block(&f.run(false));
}

#[test]
fn backlog_next_garbage_output_blocks_first_stop() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_live(SESSION);
    f.set("next", "{not json at all\n", 0);
    f.claims_empty();
    f.circuit_continue();
    assert_undetermined_block(&f.run(false));
}

#[test]
fn claims_garbage_output_blocks_first_stop() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_live(SESSION);
    f.next_none();
    f.set("claims", "<<garbage>>\n", 0);
    f.circuit_continue();
    assert_undetermined_block(&f.run(false));
}

// ─── 10: circuit abnormal ───────────────────────────────────────────────────

#[test]
fn circuit_abnormal_exit_with_pending_work_blocks_first_stop() {
    let f = Fixture::new();
    f.live_with_task();
    f.set("circuit", "", 2);
    f.run(false).assert_block();
}

#[test]
fn circuit_garbage_output_with_pending_work_blocks_first_stop() {
    let f = Fixture::new();
    f.live_with_task();
    f.set("circuit", "%%% not json %%%\n", 0);
    f.run(false).assert_block();
}

// ─── 11: a determined outcome resets the bounded state ──────────────────────

#[test]
fn determined_outcome_resets_bounded_undetermined_state() {
    let f = Fixture::new();
    f.register(SESSION);
    f.status_live(SESSION);
    f.next_unjudgeable();
    f.claims_empty();
    f.circuit_continue();
    assert_undetermined_block(&f.run(false));

    // Now everything is determinable and work exists: re-entry must BLOCK,
    // not take the bounded allow left over from the undetermined stop.
    f.next_task();
    let determined = f.run(true);
    determined.assert_block();
    assert!(
        determined.reason().contains(TASK_ID) || determined.reason().contains(TASK_TITLE),
        "determined block must name the task\n{}",
        determined.dbg()
    );

    // The streak was reset by the determined run, so a fresh undetermined
    // re-entry is the FIRST undetermined stop again and must block.
    f.next_unjudgeable();
    assert_undetermined_block(&f.run(true));
}

// ─── 12: unreadable driver record ───────────────────────────────────────────

#[test]
fn driver_record_is_a_directory_blocks_first_stop() {
    let f = Fixture::new();
    fs::create_dir_all(f.driver_path(SESSION)).unwrap();
    f.status_live(SESSION);
    f.next_task();
    f.claims_empty();
    f.circuit_continue();
    assert_undetermined_block(&f.run(false));
}

#[test]
fn driver_record_garbage_json_blocks_first_stop() {
    let f = Fixture::new();
    fs::create_dir_all(f.drivers_bucket()).unwrap();
    fs::write(f.driver_path(SESSION), "{{{ definitely not json").unwrap();
    f.status_live(SESSION);
    f.next_task();
    f.claims_empty();
    f.circuit_continue();
    assert_undetermined_block(&f.run(false));
}

// ─── 13: plugin wiring ──────────────────────────────────────────────────────

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn hooks_json_registers_stop_gate() {
    let p = crate_dir().join("hooks").join("hooks.json");
    let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let v: Value = serde_json::from_str(&text).expect("hooks.json must be valid JSON");
    let stop = v
        .get("hooks")
        .and_then(|h| h.get("Stop"))
        .and_then(Value::as_array)
        .expect("hooks.Stop must be an array");
    let found = stop.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hs| {
                hs.iter().any(|h| {
                    h.get("type").and_then(Value::as_str) == Some("command")
                        && h.get("command").and_then(Value::as_str)
                            == Some("${CLAUDE_PLUGIN_ROOT}/bin/flow stop-gate")
                })
            })
    });
    assert!(
        found,
        "no Stop hook running `${{CLAUDE_PLUGIN_ROOT}}/bin/flow stop-gate`:\n{text}"
    );
}

#[test]
fn flow_launcher_exists() {
    let p = crate_dir().join("bin").join("flow");
    assert!(p.is_file(), "launcher {} must exist", p.display());
}
