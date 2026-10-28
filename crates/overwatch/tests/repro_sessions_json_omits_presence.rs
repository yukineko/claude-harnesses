// 統合テスト: テスト内の unwrap/expect/panic は意図的な assert（production 側は workspace の deny のまま）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `overwatch sessions --json` omits presence-registered live
//! sessions (registered only through the hooks.json commands, no
//! `overwatch begin`) and prints `[]` while one is live.
//!
//! Contract (user ruling 2026-10-27): `sessions --json` must include
//! presence-registered live sessions, or else report undetermined. It must
//! never print `[]` while one is live.
//!
//! Black-box: sessions are registered ONLY by running the commands in
//! `crates/overwatch/hooks/hooks.json` (with `${CLAUDE_PLUGIN_ROOT}/bin/overwatch`
//! replaced by the built binary) with Claude Code hook payloads on stdin.
//! Every child runs with `HOME` = a tempdir sandbox and `PATH=/usr/bin:/bin`.
//! The output shape is deliberately NOT pinned (array or object both allowed).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const PLUGIN_BIN: &str = "${CLAUDE_PLUGIN_ROOT}/bin/overwatch";

struct Sandbox {
    _t: TempDir,
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn sandbox() -> Sandbox {
    let t = TempDir::new().unwrap();
    let root = t.path().canonicalize().unwrap();
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("README"), "x\n").unwrap();
    git(&repo, &["add", "README"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    Sandbox {
        _t: t,
        root,
        home,
        repo,
    }
}

fn base_cmd(sb: &Sandbox, cwd: &Path) -> Command {
    let mut c = Command::new("sh");
    c.current_dir(cwd)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDE_PLUGIN_ROOT", env!("CARGO_MANIFEST_DIR"))
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID");
    c
}

/// Commands configured in hooks.json for `event`; for tool events only the
/// entries whose `matcher` regex matches `tool` (no/empty matcher = all).
fn hook_commands(event: &str, tool: Option<&str>) -> Vec<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let mut out = Vec::new();
    let Some(entries) = v["hooks"][event].as_array() else {
        return out;
    };
    for e in entries {
        let m = e.get("matcher").and_then(Value::as_str).unwrap_or("");
        if let (Some(tool), false) = (tool, m.is_empty()) {
            let re = regex::Regex::new(&format!("^(?:{m})$")).unwrap();
            if !re.is_match(tool) {
                continue;
            }
        }
        for h in e["hooks"].as_array().cloned().unwrap_or_default() {
            if h["type"] == "command" {
                out.push(h["command"].as_str().unwrap().replace(PLUGIN_BIN, BIN));
            }
        }
    }
    out
}

fn fire(sb: &Sandbox, event: &str, tool: Option<&str>, payload: &Value) {
    let cwd = PathBuf::from(payload["cwd"].as_str().unwrap());
    let cmds = hook_commands(event, tool);
    assert!(!cmds.is_empty(), "hooks.json has no {event} command");
    for cmd in cmds {
        let mut child = base_cmd(sb, &cwd)
            .arg("-c")
            .arg(&cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(payload.to_string().as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{event} hook command `{cmd}` exited {:?}\nstdout:{}\nstderr:{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn register(sb: &Sandbox, sid: &str, transcript: &Path, cwd: &Path) {
    fire(
        sb,
        "SessionStart",
        None,
        &json!({"session_id":sid,"transcript_path":transcript,"cwd":cwd,
                "hook_event_name":"SessionStart","source":"startup"}),
    );
    let file = cwd.join("README");
    fire(
        sb,
        "PostToolUse",
        Some("Edit"),
        &json!({"session_id":sid,"transcript_path":transcript,"cwd":cwd,
                "hook_event_name":"PostToolUse","tool_name":"Edit",
                "tool_input":{"file_path":file,"old_string":"a","new_string":"b"},
                "tool_response":{"filePath":file,"success":true}}),
    );
}

fn write_transcript(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "{\"type\":\"user\"}\n").unwrap();
    touch_now(path);
}

fn touch_now(path: &Path) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now()).unwrap();
}

fn run_sessions_json(sb: &Sandbox, cwd: &Path) -> (Output, String) {
    let out = Command::new(BIN)
        .current_dir(cwd)
        .env("HOME", &sb.home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OVERWATCH_RUN_ID")
        .stdin(Stdio::null())
        .args(["sessions", "--json"])
        .output()
        .unwrap();
    let raw = String::from_utf8_lossy(&out.stdout).to_string();
    (out, raw)
}

/// Depth-first search for a JSON object that has `session_id == sid`.
fn find_session<'a>(v: &'a Value, sid: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => {
            if m.get("session_id").and_then(Value::as_str) == Some(sid) {
                return Some(v);
            }
            m.values().find_map(|x| find_session(x, sid))
        }
        Value::Array(a) => a.iter().find_map(|x| find_session(x, sid)),
        _ => None,
    }
}

/// Proves: a session registered only via hooks.json SessionStart +
/// PostToolUse(Edit) (no `overwatch begin`) with a fresh transcript appears in
/// `overwatch sessions --json` stdout: the stdout is JSON, contains an object
/// with `session_id` == the id, and that object has `liveness` == "live".
/// The pre-hook control proves the id cannot come from anywhere but the hooks.
///
/// Does NOT prove: the top-level shape (array vs object, deliberately free),
/// the text (non-`--json`) rendering, stale/ended handling, or real Claude
/// Code payloads (these are synthetic).
#[test]
fn sessions_json_lists_presence_registered_live_session() {
    let sb = sandbox();
    let sid = "sess-roster-live-3f9a2b71";
    let transcript = sb.root.join("transcripts").join(format!("{sid}.jsonl"));
    write_transcript(&transcript);

    // Control (anti-vacuity): before any hook the id appears nowhere.
    let (_, raw0) = run_sessions_json(&sb, &sb.repo);
    assert!(
        !raw0.contains(sid),
        "session id visible before any hook ran:\n{raw0}"
    );

    register(&sb, sid, &transcript, &sb.repo);
    touch_now(&transcript);

    let (out, raw) = run_sessions_json(&sb, &sb.repo);
    assert!(
        raw.contains(sid),
        "sessions --json omits the presence-registered live session {sid} \
         (exit {:?}):\nstdout:{raw}\nstderr:{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("sessions --json stdout is not JSON ({e}):\n{raw}"));
    let e = find_session(&v, sid)
        .unwrap_or_else(|| panic!("no object with session_id={sid} in:\n{raw}"));
    assert_eq!(
        e["liveness"], "live",
        "fresh-transcript session must be live:\n{e}"
    );
}

/// Proves: after a hook-registered session's presence `*.json` file(s) (those
/// under `~/.overwatch/**/overwatch/` mentioning the id) are corrupted,
/// `overwatch sessions --json`:
///   (a) does not print a bare `[]` (stdout trimmed != "[]"), AND
///   (b) signals undetermined active_sessions: EITHER exits non-zero, OR its
///       stdout contains both the strings "undetermined" and "active_sessions".
/// The "at least one file corrupted" assertion keeps it from passing vacuously.
///
/// Does NOT prove: the exact JSON location of the marker, the reason wording,
/// permission-denied (vs corrupt) registries, or non-`.json` registries.
#[test]
fn corrupt_presence_is_undetermined_not_empty_array() {
    let sb = sandbox();
    let sid = "sess-roster-corrupt-5e6d";
    let transcript = sb.root.join("transcripts").join("c.jsonl");
    write_transcript(&transcript);
    register(&sb, sid, &transcript, &sb.repo);
    touch_now(&transcript);

    let store_base = sb.home.join(".overwatch");
    let mut corrupted = Vec::new();
    let mut stack = vec![store_base.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "json")
                && p.components().any(|c| c.as_os_str() == "overwatch")
                && std::fs::read_to_string(&p).is_ok_and(|s| s.contains(sid))
            {
                std::fs::write(&p, "{not json").unwrap();
                corrupted.push(p);
            }
        }
    }
    assert!(
        !corrupted.is_empty(),
        "no *.json file under {} mentions {sid}: the hooks never registered it",
        store_base.display()
    );

    let (out, raw) = run_sessions_json(&sb, &sb.repo);
    assert_ne!(
        raw.trim(),
        "[]",
        "corrupt presence registry ({corrupted:?}) rendered as a bare [] (exit {:?})",
        out.status.code()
    );
    let signalled =
        !out.status.success() || (raw.contains("undetermined") && raw.contains("active_sessions"));
    assert!(
        signalled,
        "corrupt presence registry ({corrupted:?}) not signalled as undetermined \
         active_sessions (exit {:?}):\nstdout:{raw}\nstderr:{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}
