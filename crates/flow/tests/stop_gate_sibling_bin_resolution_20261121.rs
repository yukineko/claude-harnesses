//! RED for a `flow stop-gate` defect observed twice on 2026-11-21 in session
//! c1b3e891 (flow 0.2.42 deployed, main 366eab917). Verbatim Stop-hook output:
//!
//! "flow stop-gate: could not determine whether this /flow driver session may
//! stop. Unreadable: driver liveness (`backlog driver status --project
//! /Users/yuki/src/.harness-worktrees/session-c1b3e891`): cannot run backlog
//! driver status --project ...: No such file or directory (os error 2)"
//!
//! From the Bash tool the same command worked (`backlog` resolved to
//! `~/.claude/plugins/cache/yukineko/backlog/0.3.145/bin/backlog`, exit 0,
//! `active: true`). `crates/flow/src/probes.rs` (`Real`, "spawn `backlog` /
//! `condukt` from `PATH`") runs `Command::new("backlog")` / `"condukt"` by
//! bare name; `harness_core::plugin_bin` documents (measured 2026-08-21) that
//! hook processes do NOT inherit the plugin `bin/` dirs on `$PATH`, and other
//! hooks (autoflow, condukt claim/oracle/state) resolve siblings with
//! `harness_core::plugin_bin::resolve` (plugin cache first, `$PATH` second).
//! Result: a live driver with work never gets a determined verdict; every Stop
//! is "undetermined".
//!
//! Readings:
//! (a) defect — the hook must locate the sibling plugin binary independently
//!     of `$PATH` (the plugin cache `rollout-plugins.sh` writes).
//! (b) intended — `$PATH` is the contract and the plugin install must export
//!     the bin dirs to hooks.
//! How to tell: whether the hook environment Claude Code provides carries the
//! plugin bin dirs on `$PATH` — `plugin_bin`'s module doc records it does not
//! (measured), and the observed os error 2 agrees, so (a).
//!
//! Fixture: temp `HOME` (= `HARNESS_SANDBOX_HOME`); shell stubs for
//! `backlog` and `condukt` installed ONLY at the deployed plugin-cache
//! location `$HOME/.claude/plugins/cache/yukineko/<name>/<ver>/bin/<name>`;
//! hook `PATH=/usr/bin:/bin` (no stub on it). The driver is registered and the
//! stubs report a live driver with one pending task, a continuing circuit and
//! no claims — a fully determinable "work remains" state. Asserted: the hook
//! blocks with the DETERMINED work-remains reason (names the task) and never
//! with "No such file or directory" / "could not determine". Control: the same
//! state with the stubs on `PATH` (how `tests/stop_gate.rs` runs) already gets
//! the determined block.
//!
//! Written by an independent test author (not the implementer), CLAUDE.md
//! §2(a).
//!
//! DOES NOT PROVE: which version dir is picked when several exist; behaviour
//! when the cache is unreadable; the real Claude Code hook environment (only
//! a minimal `PATH` standing in for it).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const SESSION: &str = "sess-flow-sibling-bin-20261121";
const TASK_ID: &str = "c0ffee11";
const TASK_TITLE: &str = "drain the sibling-bin queue";

/// Stub body; `@CTL@` is replaced with the absolute ctl dir (the two stubs
/// live in different cache dirs, so the ctl dir cannot be relative).
const STUB: &str = r#"#!/bin/sh
ctl='@CTL@'
tool=$(basename "$0")
key=""
case "$tool:$1:$2" in
  backlog:driver:status) key=driver_status ;;
  condukt:state:claims) key=claims ;;
  condukt:circuit:check) key=circuit ;;
esac
if [ -z "$key" ] && [ "$tool:$1" = "backlog:next" ]; then key=next; fi
[ -z "$key" ] && exit 0
[ -f "$ctl/$key.out" ] && cat "$ctl/$key.out"
code=0
[ -f "$ctl/$key.code" ] && code=$(cat "$ctl/$key.code")
exit "$code"
"#;

struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    ctl: PathBuf,
    path_stubs: PathBuf,
    project: PathBuf,
    transcript: PathBuf,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn write_stub(path: &Path, ctl: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, STUB.replace("@CTL@", &ctl.to_string_lossy())).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let ctl = root.join("ctl");
        let path_stubs = root.join("path-stubs");
        let project = root.join("project");
        for d in [&home, &ctl, &path_stubs, &project] {
            fs::create_dir_all(d).unwrap();
        }
        let cache = home
            .join(".claude")
            .join("plugins")
            .join("cache")
            .join("yukineko");
        for (name, ver) in [("backlog", "0.3.145"), ("condukt", "0.9.1")] {
            write_stub(&cache.join(name).join(ver).join("bin").join(name), &ctl);
            write_stub(&path_stubs.join(name), &ctl);
        }
        let transcript = root.join("transcript.jsonl");
        fs::write(&transcript, "").unwrap();
        let fx = Fx {
            _tmp: tmp,
            home,
            ctl,
            path_stubs,
            project,
            transcript,
        };
        fx.live_with_task();
        fx
    }

    fn project_str(&self) -> String {
        self.project.to_string_lossy().into_owned()
    }

    fn set(&self, key: &str, out: &str, code: i32) {
        fs::write(self.ctl.join(format!("{key}.out")), out).unwrap();
        fs::write(self.ctl.join(format!("{key}.code")), code.to_string()).unwrap();
    }

    /// Registered + live driver, one pending task, no claims, circuit
    /// continue — the same determinable state as `tests/stop_gate.rs`
    /// `live_with_task`.
    fn live_with_task(&self) {
        let slug = format!(
            "{:016x}",
            harness_core::hash::fnv1a64(self.project_str().as_bytes())
        );
        let bucket = self.home.join(".backlog").join("drivers").join(slug);
        fs::create_dir_all(&bucket).unwrap();
        let t = now();
        let rec = json!({
            "session_id": SESSION,
            "pid": std::process::id(),
            "project": self.project_str(),
            "registered_at": t,
            "heartbeat_at": t,
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
            "title": TASK_TITLE,
            "hashkey": format!("hk-{TASK_ID}"),
            "status": "pending",
            "priority": 1,
        });
        self.set("next", &format!("{task}\n"), 0);
        self.set("claims", "{}\n", 0);
        let circuit = json!({"verdict": "continue", "reason": null, "streak": 0});
        self.set("circuit", &format!("{circuit}\n"), 0);
    }

    /// Run `flow stop-gate` once with the given `PATH`.
    fn run(&self, path: &str) -> (Option<i32>, String, String) {
        let payload = json!({
            "session_id": SESSION,
            "cwd": self.project_str(),
            "stop_hook_active": false,
            "hook_event_name": "Stop",
            "transcript_path": self.transcript.to_string_lossy(),
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_flow"))
            .arg("stop-gate")
            .current_dir(&self.project)
            .env_clear()
            .env("HOME", &self.home)
            .env("HARNESS_SANDBOX_HOME", &self.home)
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("CLAUDE_CONFIG_DIR", self.home.join(".claude"))
            .env("CLAUDE_CODE_SESSION_ID", SESSION)
            .env("PATH", path)
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

/// The determined "work remains" block: decision block, reason names the
/// task, and nothing says it could not determine / could not spawn.
fn assert_determined_work_block(code: Option<i32>, stdout: &str, stderr: &str) {
    let dbg = format!("exit={code:?}\nstdout={stdout}\nstderr={stderr}");
    assert_eq!(code, Some(0), "hook mode must exit 0\n{dbg}");
    let v: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e})\n{dbg}"));
    assert_eq!(
        v.get("decision").and_then(Value::as_str),
        Some("block"),
        "expected a block\n{dbg}"
    );
    let reason = v.get("reason").and_then(Value::as_str).unwrap_or("");
    assert!(
        !reason.contains("No such file or directory") && !reason.contains("could not determine"),
        "the sibling binaries exist in the plugin cache; liveness must be determined\n{dbg}"
    );
    assert!(
        reason.contains(TASK_ID) || reason.contains(TASK_TITLE),
        "expected the determined work-remains reason naming the task\n{dbg}"
    );
}

#[test]
fn stop_gate_finds_backlog_in_plugin_cache_without_path() {
    let fx = Fx::new();
    let (code, out, err) = fx.run("/usr/bin:/bin");
    assert_determined_work_block(code, &out, &err);
}

#[test]
fn control_stubs_on_path_give_determined_block() {
    let fx = Fx::new();
    let path = format!("{}:/usr/bin:/bin", fx.path_stubs.display());
    let (code, out, err) = fx.run(&path);
    assert_determined_work_block(code, &out, &err);
}
