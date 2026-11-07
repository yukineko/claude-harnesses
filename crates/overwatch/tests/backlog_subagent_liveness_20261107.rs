// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07, session c2e3bc17): no subagent registry and no
//! mechanical per-agent answer to "are the subagents running properly?".
//!
//! Observed: asked 「sub agentはきちんと動いているか?」 the main session answered
//! "none stalled" after (a) reading only the 3 newest of 6 subagent transcripts
//! by mtime, (b) counting `ps` rows from other sessions/runs, (c) explaining
//! away a contradicting condukt `failed` with a guess, (d) treating a recent
//! mtime as progress. The user then found and stopped a stalled agent,
//! a56b4006f1b96dc82 (t5 condukt-verifier, haiku, run-20261107-051454-39899).
//! Its turn ended at 20:29:08Z with a text report but NO SubagentHandback;
//! SubagentStop fired anyway, the harness emitted `<status>completed</status>`,
//! and its orchestrator kept "waiting on the t5 verifier" for 28 minutes.
//!
//! User ruling 2026-11-07: 「sub agentを記録し、長期実行のものは確認するような
//! コードをいれられないか?」 — RECORD every subagent, CHECK the long-running.
//!
//! Desired API (designed here, not implemented):
//!
//! * RECORD — `overwatch presence` (the hook that already writes the
//!   per-session registry under `~/.overwatch/<key>/overwatch/`) also records
//!   an Agent launch from its PostToolUse payload (`tool_response.agentId`,
//!   async_launched) — direct or nested (payload `agent_id` = launcher) — and
//!   the SubagentStop event. hooks.json routes `Agent`/`Task` PostToolUse and
//!   `SubagentStop` to it.
//! * CHECK — `overwatch subagents --session <id> --stall-secs <n> --json`
//!   (store selected by cwd) prints `{"clean": bool, "agents": [{"agent_id",
//!   "state"}]}` for EVERY recorded agent of the session, `state` one of
//!   `done` / `progressing` / `stalled` / `query_required` / `undetermined`.
//!   `done` needs a delivered result (SubagentHandback success): SubagentStop
//!   alone is NOT done (observed above). Progress is new tool results in the
//!   agent transcript (entry timestamps), never file mtime or hook
//!   attachments. Exit 0 iff every agent is `done` or `progressing`, exit 1
//!   otherwise (1, not clap's 2, so a usage error cannot pass as a verdict).
//!   An unreadable registry is not clean.
//!
//! Only classification and exit code are asserted (user ruling 2026-11-07: no
//! tests on prose). Written by a disinterested author (CLAUDE.md section
//! 2(a)): no fix exists.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const OTHER_SESSION: &str = "99999999-8888-7777-6666-555555555555";
const STALL_SECS: i64 = 600;

const ORCH: &str = "a0000000000000001"; // progressing, launches nested agents
const PROG: &str = "a0000000000000002"; // progressing (nested under ORCH)
const DONE: &str = "a0000000000000003"; // done: handback delivered (nested)
const STALL_IDLE: &str = "a0000000000000004"; // no new tool result > threshold
const STALL_QUERY: &str = "a0000000000000005"; // only hook attachments recent; query unanswered
const STALL_LOOP: &str = "a0000000000000006"; // repeated identical tool calls
const UNREADABLE: &str = "a0000000000000007"; // transcript is not JSONL
const MISSING: &str = "a0000000000000008"; // recorded launch, no transcript at all
const NO_HANDBACK: &str = "a0000000000000009"; // observed a56b4006 shape
const FOREIGN: &str = "a00000000000000ff"; // belongs to OTHER_SESSION

fn now() -> i64 {
    static N: OnceLock<i64> = OnceLock::new();
    *N.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    })
}

/// `now() + off` as RFC 3339 UTC (the transcript timestamp format).
fn ts(off: i64) -> String {
    let t = now() + off;
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

fn jl(entries: &[Value]) -> String {
    let mut s = String::new();
    for e in entries {
        s.push_str(&e.to_string());
        s.push('\n');
    }
    s
}

fn tool_use(off: i64, id: &str, name: &str, input: Value) -> Value {
    json!({"type": "assistant", "timestamp": ts(off), "isSidechain": true,
        "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": name, "input": input}]}})
}

fn tool_result(off: i64, id: &str, content: &str) -> Value {
    json!({"type": "user", "timestamp": ts(off), "isSidechain": true,
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": content}]}})
}

fn text(off: i64, t: &str) -> Value {
    json!({"type": "assistant", "timestamp": ts(off), "isSidechain": true,
        "message": {"role": "assistant", "content": [{"type": "text", "text": t}]}})
}

fn hook_attachment(off: i64, hook: &str) -> Value {
    json!({"type": "attachment", "timestamp": ts(off), "isSidechain": true,
        "attachment": {"type": "hook_success", "hookName": hook,
            "hookEvent": hook, "content": "", "stdout": "{}\n", "stderr": "",
            "exitCode": 0}})
}

fn bash(off: i64, id: &str, cmd: &str, out: &str) -> Vec<Value> {
    vec![
        tool_use(off, id, "Bash", json!({"command": cmd})),
        tool_result(off + 1, id, out),
    ]
}

fn handback(off: i64) -> Vec<Value> {
    vec![
        tool_use(
            off,
            "toolu_hb",
            "SubagentHandback",
            json!({"message": "report"}),
        ),
        tool_result(
            off + 1,
            "toolu_hb",
            "{\"success\":true,\"message\":\"Report delivered to your caller.\"}",
        ),
        text(off + 2, "Done."),
        hook_attachment(off + 2, "SubagentStop"),
    ]
}

/// A status query from the parent, as recorded in the parent transcript.
fn send_message(off: i64, n: usize, to: &str) -> Vec<Value> {
    let tu = format!("toolu_query_{n:04}");
    vec![
        tool_use(
            off,
            &tu,
            "SendMessage",
            json!({"to": to, "summary": "status check",
                "message": "Status check: reply with your current step"}),
        ),
        tool_result(
            off + 1,
            &tu,
            "{\"success\":true,\"message\":\"Message queued for delivery\"}",
        ),
    ]
}

fn slug(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

struct Fx {
    _t: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Fx {
            _t: t,
            home,
            project,
        }
    }
    fn proj_dir(&self) -> PathBuf {
        self.home.join(".claude/projects").join(slug(&self.project))
    }
    fn parent_path(&self, session: &str) -> PathBuf {
        self.proj_dir().join(format!("{session}.jsonl"))
    }
    fn agent_path(&self, session: &str, id: &str) -> PathBuf {
        self.proj_dir()
            .join(session)
            .join("subagents")
            .join(format!("agent-{id}.jsonl"))
    }
    fn parent(&self, session: &str, entries: &[Value]) {
        let p = self.parent_path(session);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, jl(entries)).unwrap();
    }
    /// Write an agent transcript (mtime = now for every file: a fresh mtime
    /// must not read as progress).
    fn transcript(&self, session: &str, id: &str, body: &[u8]) {
        let p = self.agent_path(session, id);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// Feed one hook payload to `overwatch presence` (the RECORD side).
    fn hook(&self, payload: Value) {
        let mut child = harness_core::git_env::scrub(&mut Command::new(BIN))
            .arg("presence")
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "presence hook failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// PostToolUse of an async Agent launch. `launcher` = Some(agent) for a
    /// nested launch made inside that subagent.
    fn launch(&self, session: &str, n: usize, agent: &str, launcher: Option<&str>) {
        let mut p = json!({
            "session_id": session,
            "transcript_path": self.parent_path(session).to_string_lossy(),
            "cwd": self.project.to_string_lossy(),
            "hook_event_name": "PostToolUse",
            "tool_name": "Agent",
            "tool_input": {"description": format!("task {n}"), "prompt": "work",
                "subagent_type": "general-purpose", "run_in_background": true},
            "tool_response": {"isAsync": true, "status": "async_launched",
                "agentId": agent, "description": format!("task {n}")},
            "tool_use_id": format!("toolu_launch_{n:04}")
        });
        if let Some(l) = launcher {
            p["agent_id"] = json!(l);
            p["agent_type"] = json!("general-purpose");
        }
        self.hook(p);
    }

    fn subagent_stop(&self, session: &str, agent: &str) {
        self.hook(json!({
            "session_id": session,
            "transcript_path": self.parent_path(session).to_string_lossy(),
            "cwd": self.project.to_string_lossy(),
            "hook_event_name": "SubagentStop",
            "stop_hook_active": false,
            "agent_id": agent,
            "agent_type": "general-purpose",
            "agent_transcript_path": self.agent_path(session, agent).to_string_lossy()
        }));
    }

    /// The CHECK side.
    fn check(&self, session: &str) -> (Option<i32>, Option<Value>) {
        let out = harness_core::git_env::scrub(&mut Command::new(BIN))
            .args([
                "subagents",
                "--session",
                session,
                "--stall-secs",
                &STALL_SECS.to_string(),
                "--json",
            ])
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .output()
            .expect("spawn overwatch");
        eprintln!(
            "exit={:?}\nstdout={}\nstderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (
            out.status.code(),
            serde_json::from_slice::<Value>(&out.stdout).ok(),
        )
    }
}

fn states(v: &Value) -> BTreeMap<String, String> {
    v.get("agents")
        .and_then(Value::as_array)
        .expect("`agents` array in --json output")
        .iter()
        .map(|a| {
            (
                a.get("agent_id")
                    .and_then(Value::as_str)
                    .expect("agent_id")
                    .to_string(),
                a.get("state")
                    .and_then(Value::as_str)
                    .expect("state")
                    .to_string(),
            )
        })
        .collect()
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// Progressing orchestrator ORCH with a progressing nested PROG and a done
/// nested DONE, plus an unrelated session whose progressing agent must never
/// be counted. Returns the parent transcript entries so far.
fn healthy(fx: &Fx) -> Vec<Value> {
    fx.launch(SESSION, 0, ORCH, None);
    fx.launch(SESSION, 1, PROG, Some(ORCH));
    fx.launch(SESSION, 2, DONE, Some(ORCH));

    let mut orch = vec![];
    orch.extend(bash(-240, "toolu_o1", "condukt status", "t1 running"));
    orch.extend(bash(-90, "toolu_o2", "git log -1", "abc123 commit"));
    orch.push(hook_attachment(-89, "PostToolUse:Bash"));
    fx.transcript(SESSION, ORCH, jl(&orch).as_bytes());

    let mut prog = vec![];
    prog.extend(bash(-2900, "toolu_p0", "cargo build", "Compiling"));
    prog.extend(bash(-200, "toolu_p1", "cargo test -p x", "3 passed"));
    prog.extend(bash(-60, "toolu_p2", "git diff --stat", "2 files changed"));
    prog.push(hook_attachment(-59, "PostToolUse:Bash"));
    fx.transcript(SESSION, PROG, jl(&prog).as_bytes());

    let mut done = vec![];
    done.extend(bash(-2600, "toolu_d1", "cargo test", "ok"));
    done.extend(handback(-2420));
    fx.transcript(SESSION, DONE, jl(&done).as_bytes());
    fx.subagent_stop(SESSION, DONE);

    fx.parent(OTHER_SESSION, &[]);
    fx.launch(OTHER_SESSION, 0, FOREIGN, None);
    fx.transcript(
        OTHER_SESSION,
        FOREIGN,
        jl(&bash(-30, "toolu_f1", "cargo test", "running")).as_bytes(),
    );

    vec![text(-3100, "dispatching")]
}

#[test]
fn every_recorded_agent_is_classified_and_long_running_is_not_clean() {
    let fx = Fx::new();
    let mut parent = healthy(&fx);

    // STALL_IDLE: last tool result 1500 s ago; last entry is a hook attachment.
    fx.launch(SESSION, 3, STALL_IDLE, None);
    let mut s = bash(-1600, "toolu_s1", "cargo test -p y", "Running");
    s.push(hook_attachment(-1499, "PostToolUse:Bash"));
    fx.transcript(SESSION, STALL_IDLE, jl(&s).as_bytes());

    // STALL_QUERY: only hook attachments are recent, the last tool result is
    // 1400 s old, and a status query sent 900 s ago was never answered.
    fx.launch(SESSION, 4, STALL_QUERY, None);
    parent.extend(send_message(-900, 1, STALL_QUERY));
    let mut q = bash(-1401, "toolu_q1", "sleep 3600", "started");
    q.push(hook_attachment(-120, "PostToolUse:Bash"));
    q.push(hook_attachment(-30, "PostToolUse:Bash"));
    fx.transcript(SESSION, STALL_QUERY, jl(&q).as_bytes());

    // STALL_LOOP: recent, but the same call returning the same result 5 times.
    fx.launch(SESSION, 5, STALL_LOOP, None);
    let mut l = vec![];
    for i in 0..5 {
        l.extend(bash(
            -300 + i * 50,
            &format!("toolu_l{i}"),
            "cat /tmp/out.log",
            "waiting for lock",
        ));
    }
    fx.transcript(SESSION, STALL_LOOP, jl(&l).as_bytes());

    // UNREADABLE: not JSONL at all.
    fx.launch(SESSION, 6, UNREADABLE, None);
    fx.transcript(SESSION, UNREADABLE, &[0xff, 0xfe, b'{', b'\n']);

    // MISSING: recorded launch, but no transcript exists.
    fx.launch(SESSION, 7, MISSING, None);

    fx.parent(SESSION, &parent);

    let (code, v) = fx.check(SESSION);
    let v = v.expect("--json output must parse");
    let mut got = states(&v);
    // A long-running agent with no advance may be either verdict the user
    // ruling allows ("query it, or classify it stalled"); both are non-clean.
    let idle = got.remove(STALL_IDLE);
    assert!(
        matches!(idle.as_deref(), Some("stalled") | Some("query_required")),
        "{STALL_IDLE}: {idle:?}"
    );
    // Equality, not containment: every recorded agent (incl. nested) is
    // listed, and the other session's agent is not.
    assert_eq!(
        got,
        map(&[
            (ORCH, "progressing"),
            (PROG, "progressing"),
            (DONE, "done"),
            (STALL_QUERY, "stalled"),
            (STALL_LOOP, "stalled"),
            (UNREADABLE, "undetermined"),
            (MISSING, "undetermined"),
        ])
    );
    assert_eq!(v.get("clean"), Some(&json!(false)));
    assert_eq!(code, Some(1), "not clean must exit 1");
}

#[test]
fn control_all_done_or_progressing_is_clean() {
    let fx = Fx::new();
    let parent = healthy(&fx);
    fx.parent(SESSION, &parent);

    let (code, v) = fx.check(SESSION);
    let v = v.expect("--json output must parse");
    assert_eq!(
        states(&v),
        map(&[(ORCH, "progressing"), (PROG, "progressing"), (DONE, "done")])
    );
    assert_eq!(v.get("clean"), Some(&json!(true)));
    assert_eq!(code, Some(0));
}

/// A registry that cannot be read is never clean, even when every transcript
/// on disk looks healthy (the transcripts are a partial view without it).
#[test]
fn unreadable_registry_is_not_clean() {
    let fx = Fx::new();
    let parent = healthy(&fx);
    fx.parent(SESSION, &parent);

    // Corrupt every file the RECORD hook wrote (format-agnostic: whatever the
    // registry file is, it lives under the overwatch store in HOME).
    let store = fx.home.join(".overwatch");
    let mut stack = vec![store.clone()];
    let mut corrupted = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("json") {
                std::fs::write(&p, [0xff, 0xfe, b'{']).unwrap();
                corrupted += 1;
            }
        }
    }
    assert!(corrupted > 0, "RECORD hook wrote nothing under {store:?}");

    let (code, v) = fx.check(SESSION);
    let v = v.expect("--json output must parse");
    assert_eq!(v.get("clean"), Some(&json!(false)));
    assert_eq!(code, Some(1));
}

/// The observed a56b4006f1b96dc82 shape: the turn ended with a text report,
/// SubagentStop fired, NO SubagentHandback, and nothing advanced for 28
/// minutes while the launcher waited. SubagentStop is not a delivered result.
#[test]
fn subagent_stop_without_handback_is_not_done() {
    let fx = Fx::new();
    fx.parent(SESSION, &[text(-3000, "dispatching")]);
    fx.launch(SESSION, 1, NO_HANDBACK, None);
    let mut a = bash(-1740, "toolu_n1", "python3 -m pytest -q", "3 passed");
    a.push(text(-1680, "Verification report: pass"));
    a.push(hook_attachment(-1680, "SubagentStop"));
    a.push(hook_attachment(-1680, "SubagentStop"));
    fx.transcript(SESSION, NO_HANDBACK, jl(&a).as_bytes());
    fx.subagent_stop(SESSION, NO_HANDBACK);

    let (code, v) = fx.check(SESSION);
    let v = v.expect("--json output must parse");
    let got = states(&v);
    assert_eq!(got.len(), 1, "{got:?}");
    let st = got.get(NO_HANDBACK).expect("agent listed by id");
    assert!(
        st != "done" && st != "progressing",
        "ended without delivering its result: got {st}"
    );
    assert_eq!(v.get("clean"), Some(&json!(false)));
    assert_eq!(code, Some(1));
}

/// RECORD wiring: the hook that records launches/stops must actually be
/// registered for those events (a registry nobody feeds is an empty one).
#[test]
fn hooks_route_agent_launch_and_subagent_stop_to_overwatch() {
    let raw =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/hooks/hooks.json")).unwrap();
    let h: Value = serde_json::from_str(&raw).unwrap();
    let routes = |event: &str, tool: Option<&str>| -> bool {
        h["hooks"][event].as_array().is_some_and(|groups| {
            groups.iter().any(|g| {
                let m = g.get("matcher").and_then(Value::as_str).unwrap_or("");
                let matches = match tool {
                    None => true,
                    Some(t) => m.is_empty() || m == "*" || m.split('|').any(|x| x == t),
                };
                matches
                    && g["hooks"].as_array().is_some_and(|hs| {
                        hs.iter().any(|x| {
                            x["command"].as_str().is_some_and(|c| {
                                c.starts_with("${CLAUDE_PLUGIN_ROOT}/bin/overwatch ")
                            })
                        })
                    })
            })
        })
    };
    assert!(routes("PostToolUse", Some("Agent")), "PostToolUse Agent");
    assert!(routes("PostToolUse", Some("Task")), "PostToolUse Task");
    assert!(routes("SubagentStop", None), "SubagentStop");
}

/// The existing `overwatch status` scanner drops an idle agent whose last
/// entry is a hook `attachment` (the common real shape): it is listed neither
/// as stalled nor as unknown, so its absence reads as "fine".
#[test]
fn status_scanner_does_not_drop_idle_agent_ending_in_attachment() {
    let fx = Fx::new();
    let mut s = bash(-1600, "toolu_s1", "cargo test -p y", "Running");
    s.push(text(-1590, "report"));
    s.push(hook_attachment(-1590, "SubagentStop"));
    fx.transcript(SESSION, STALL_IDLE, jl(&s).as_bytes());
    let p = fx.agent_path(SESSION, STALL_IDLE);
    let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.set_modified(SystemTime::now() - std::time::Duration::from_secs(1600))
        .unwrap();

    let r = overwatch::stalled::scan(&fx.home, &fx.project, SystemTime::now());
    assert!(
        r.iter().any(|e| e.agent_id == STALL_IDLE),
        "idle agent (no handback, no advance for 26 min) was omitted: {r:?}"
    );
}
