#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Relay row provenance (backlog #1808, 2026-11-18). Independent tests written
//! ahead of the fix by an agent that does not implement it.
//!
//! Agreed design: the Stop / PostToolUse payload's `transcript_path` is the
//! trust root. A relay row is
//!
//! * Verified — `row.session_id` equals the session of the file it sits in AND
//!   `row.agent_id` is among that session's subagent transcripts
//!   (`<dir>/<session-stem>/subagents/agent-<agent_id>.jsonl`, observed naming
//!   on darwin 2026-11-18, each line carrying `agentId` and `sessionId`);
//! * Refuted — a session mismatch, or an agent_id determinably absent;
//! * Undetermined — the transcript / subagents dir cannot be read.
//!
//! Verified blocks Stop with today's run procedure; Refuted does not block;
//! Undetermined blocks WITHOUT the byte-for-byte run procedure. `mark-*` only
//! act on Verified rows; `dismiss` refuses (exit 2) a row whose session_id
//! differs from its file's session. Two Claude processes sharing one session
//! id see each other's subagents as Verified (user ruling 2026-11-17).
//!
//! Fixtures write the queue file directly (the format of
//! `harness_core::relay::Event`) so the row's own `session_id` can disagree
//! with its file; the real `askrelay` binary (and, for mark-denied, the real
//! blastguard PreToolUse) is driven for every verdict except
//! `two_processes_sharing_a_session_both_rows_verified`, which asserts the
//! `harness_core::relay::provenance` classification directly (see its doc).
//!
//! What these tests do NOT prove: that a forged row cannot name a REAL
//! subagent of the session (an attacker who knows a live agent_id still gets
//! Verified); that Claude Code writes this layout on other platforms.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

const SESSION: &str = "1808aaaa-0000-4000-8000-000000000001";
const OTHER_SESSION: &str = "1808bbbb-0000-4000-8000-000000000002";
const AGENT_A: &str = "a1808000000000001";
const AGENT_B: &str = "a1808000000000002";
const ABSENT_AGENT: &str = "a1808ffffffffffff";
const CMD: &str = r#"for f in a b; do git checkout -- "$f"; done"#;
const PROCEDURE_MARK: &str = "byte-for-byte";

struct Fx {
    root: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let root =
            std::env::temp_dir().join(format!("askrelay-prov-1808-{name}-{}", std::process::id()));
        if root.exists() {
            // A previous run may have left a chmod 000 dir behind.
            let _ = std::process::Command::new("chmod")
                .args(["-R", "u+rwx"])
                .arg(&root)
                .status();
            let _ = std::fs::remove_dir_all(&root);
        }
        for d in ["home", "relay", "cwd", "projects"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Fx { root }
    }

    fn cwd(&self) -> String {
        self.root.join("cwd").to_string_lossy().into_owned()
    }

    /// The main transcript of `SESSION`, plus a sidechain per agent under
    /// `<projects>/<SESSION>/subagents/agent-<id>.jsonl`.
    fn transcript(&self, agents: &[&str]) -> PathBuf {
        let main = self.root.join("projects").join(format!("{SESSION}.jsonl"));
        std::fs::write(
            &main,
            format!(
                "{}\n",
                json!({"type":"user","sessionId":SESSION,"message":{"role":"user","content":"hi"}})
            ),
        )
        .unwrap();
        let sub = self.root.join("projects").join(SESSION).join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        for a in agents {
            std::fs::write(
                sub.join(format!("agent-{a}.jsonl")),
                format!(
                    "{}\n",
                    json!({"parentUuid":null,"isSidechain":true,"agentId":a,"type":"user",
                           "sessionId":SESSION,"message":{"role":"user","content":"task"}})
                ),
            )
            .unwrap();
            std::fs::write(
                sub.join(format!("agent-{a}.meta.json")),
                json!({"agentType":"general-purpose"}).to_string(),
            )
            .unwrap();
        }
        main
    }

    fn subagents_dir(&self) -> PathBuf {
        self.root.join("projects").join(SESSION).join("subagents")
    }

    /// `<relay>/<SESSION>.jsonl`: the file of `SESSION`.
    fn queue_file(&self) -> PathBuf {
        self.root.join("relay").join(format!("{SESSION}.jsonl"))
    }

    /// Append a `queued` row with the given row-level session_id / agent_id
    /// to the queue file of `SESSION`.
    fn queue_row(&self, id: &str, row_session: &str, agent: &str) {
        self.queue_row_cmd(id, row_session, agent, CMD);
    }

    /// [`Fx::queue_row`] with an explicit `command`.
    fn queue_row_cmd(&self, id: &str, row_session: &str, agent: &str, command: &str) {
        self.append_line(&json!({
            "event": "queued",
            "id": id,
            "ts": 1,
            "session_id": row_session,
            "agent_id": agent,
            "cwd": self.cwd(),
            "command": command,
            "reason": "blastguard: Ask hardened to Deny inside a subagent",
        }));
    }

    /// Append an `executed` event for `id` directly (the fixture bypasses
    /// `mark-executed`, which itself only marks Verified rows).
    fn queue_executed(&self, id: &str) {
        self.append_line(&json!({"event": "executed", "id": id, "ts": 2, "exit_code": 0}));
    }

    fn append_line(&self, line: &Value) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.queue_file())
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    /// Run `askrelay mark-relayed` with a main-session SendMessage that names
    /// `id`.
    fn send_naming(&self, transcript: &Path, id: &str) -> Output {
        let p = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "SendMessage",
            "tool_input": { "to": "worker", "message": format!("relay {id}: exit 0") },
            "cwd": self.cwd(),
            "session_id": SESSION,
            "transcript_path": transcript.to_string_lossy(),
        });
        self.run(&["relay", "mark-relayed"], &p.to_string())
    }

    /// Run blastguard's main-session PreToolUse on `command` (the only caller
    /// of `relay::mark_denied`); returns the printed permission decision.
    fn main_pretooluse(&self, transcript: &Path, command: &str) -> (Output, String) {
        let p = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": self.cwd(),
            "session_id": SESSION,
            "transcript_path": transcript.to_string_lossy(),
        });
        let o = self.run(&[], &p.to_string());
        let d = serde_json::from_slice::<Value>(&o.stdout)
            .ok()
            .and_then(|v| {
                v["hookSpecificOutput"]["permissionDecision"]
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default();
        (o, d)
    }

    fn queue_text(&self) -> String {
        std::fs::read_to_string(self.queue_file()).unwrap()
    }

    fn run(&self, args: &[&str], stdin: &str) -> Output {
        let mut c = bounded::command_for(args);
        c.env("HOME", self.root.join("home"))
            .env("BLASTGUARD_RELAY_DIR", self.root.join("relay"))
            .env("BLASTGUARD_APPROVALS_DIR", self.root.join("home"))
            .env("CLAUDE_CODE_SESSION_ID", "sessEnvIgnored")
            .env("CLAUDECODE", "1")
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE)
    }

    /// Run `askrelay stop`; `None` transcript omits `transcript_path`.
    fn stop(&self, transcript: Option<&Path>) -> (Output, Option<Value>) {
        let mut p = json!({
            "hook_event_name": "Stop",
            "session_id": SESSION,
            "cwd": self.cwd(),
            "stop_hook_active": false,
        });
        if let Some(t) = transcript {
            p["transcript_path"] = json!(t.to_string_lossy());
        }
        let o = self.run(&["relay", "stop"], &p.to_string());
        let v = serde_json::from_slice::<Value>(&o.stdout).ok();
        (o, v)
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::process::Command::new("chmod")
            .args(["-R", "u+rwx"])
            .arg(&self.root)
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn blocked(v: &Option<Value>) -> bool {
    v.as_ref().is_some_and(|d| d["decision"] == "block")
}

fn reason(v: &Option<Value>) -> String {
    v.as_ref()
        .and_then(|d| d["reason"].as_str())
        .unwrap_or("")
        .to_string()
}

fn show(o: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

// ---------------------------------------------------------------- RED

#[test]
fn refuted_session_mismatch_row_does_not_block_stop() {
    let fx = Fx::new("mismatch-stop");
    // AGENT_A is a real subagent of SESSION: only the session mismatch refutes.
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000001", OTHER_SESSION, AGENT_A);
    let (o, v) = fx.stop(Some(&t));
    assert!(
        o.status.success(),
        "askrelay stop must exit 0: {}",
        show(&o)
    );
    assert!(
        !blocked(&v),
        "row with session_id {OTHER_SESSION} in the file of {SESSION} (Refuted) made Stop block: {}",
        show(&o)
    );
}

#[test]
fn undetermined_missing_transcript_path_blocks_without_run_procedure() {
    let fx = Fx::new("undet-nopath");
    let _ = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000002", SESSION, AGENT_A);
    let (o, v) = fx.stop(None);
    assert!(
        blocked(&v),
        "Undetermined provenance must still block: {}",
        show(&o)
    );
    let r = reason(&v);
    assert!(
        r.contains("bgr-180800000002"),
        "block must name the id: {r}"
    );
    assert!(
        !r.contains(PROCEDURE_MARK),
        "Undetermined provenance must not carry the run-it-{PROCEDURE_MARK} procedure: {r}"
    );
}

#[test]
fn undetermined_unreadable_subagents_dir_blocks_without_run_procedure() {
    let fx = Fx::new("undet-unreadable");
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000003", SESSION, AGENT_A);
    let sub = fx.subagents_dir();
    let st = std::process::Command::new("chmod")
        .arg("000")
        .arg(&sub)
        .status()
        .unwrap();
    assert!(st.success(), "chmod 000 failed");
    // Control: the fixture really is unreadable to this process (fails when
    // the tests run as root, which would make the test vacuous).
    assert!(
        std::fs::read_dir(&sub).is_err(),
        "fixture: {} must be unreadable",
        sub.display()
    );
    let (o, v) = fx.stop(Some(&t));
    assert!(
        blocked(&v),
        "Undetermined provenance must still block: {}",
        show(&o)
    );
    let r = reason(&v);
    assert!(
        r.contains("bgr-180800000003"),
        "block must name the id: {r}"
    );
    assert!(
        !r.contains(PROCEDURE_MARK),
        "Undetermined provenance must not carry the run-it-{PROCEDURE_MARK} procedure: {r}"
    );
}

#[test]
fn dismiss_refuses_row_whose_session_differs_from_its_file() {
    let fx = Fx::new("dismiss-mismatch");
    let _ = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000004", OTHER_SESSION, AGENT_A);
    let o = fx.run(
        &["relay", "dismiss", "bgr-180800000004", "--reason", "test"],
        "",
    );
    let text = fx.queue_text();
    assert_eq!(
        o.status.code(),
        Some(2),
        "dismiss of a session-mismatched row must exit 2: {}\nqueue:\n{text}",
        show(&o)
    );
    assert!(
        !text.contains("\"dismissed\""),
        "the mismatched row must stay undismissed; queue:\n{text}"
    );
}

#[test]
fn mark_executed_does_not_mark_refuted_row() {
    let fx = Fx::new("mark-refuted");
    // The subagents dir is readable and holds AGENT_A only: ABSENT_AGENT is
    // determinably absent.
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000005", SESSION, ABSENT_AGENT);
    let post = json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": CMD },
        "tool_response": { "exit_code": 0 },
        "cwd": fx.cwd(),
        "session_id": SESSION,
        "transcript_path": t.to_string_lossy(),
    })
    .to_string();
    let o = fx.run(&["relay", "mark-executed"], &post);
    assert!(o.status.success(), "mark-executed exits 0: {}", show(&o));
    let text = fx.queue_text();
    assert!(
        !text.contains("\"executed\""),
        "a Refuted row (agent {ABSENT_AGENT} absent from {SESSION}'s subagents) was marked executed; queue:\n{text}"
    );
}

// ---------------------------------------------------------------- CONTROL

#[test]
fn verified_row_blocks_stop_with_run_procedure() {
    let fx = Fx::new("verified");
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000006", SESSION, AGENT_A);
    let (o, v) = fx.stop(Some(&t));
    assert!(blocked(&v), "a Verified row must block Stop: {}", show(&o));
    let r = reason(&v);
    assert!(
        r.contains("bgr-180800000006"),
        "block must name the id: {r}"
    );
    assert!(
        r.contains(PROCEDURE_MARK),
        "a Verified row keeps the run procedure: {r}"
    );
}

/// Two Claude processes (harness-5a, harness-9e) share SESSION and its one
/// relay file; AGENT_A was spawned by one, AGENT_B by the other. Both
/// sidechains live under SESSION, so both rows classify as Verified (user
/// ruling 2026-11-17).
///
/// Observable chosen: the classification itself,
/// `harness_core::relay::provenance` over the rows `relay::load` folds from
/// the queue file and `SessionAgents::of` the shared transcript. It is the
/// narrowest one available: the Stop text also carries whatever per-process
/// filtering is layered on top (per-process PID attribution, a separate spec,
/// is expected to show each process only its own rows), and `mark-executed`
/// marks one row per call and couples to command/cwd matching. The
/// classification takes no process input: both processes hand it the same
/// (queue file, session, transcript_path), so one evaluation covers both.
///
/// DOES NOT PROVE: what either process's Stop lists; that the binaries call
/// `provenance` (the other tests in this file drive the binaries).
#[test]
fn two_processes_sharing_a_session_both_rows_verified() {
    use harness_core::relay::{self, Provenance, Queue, SessionAgents};
    let fx = Fx::new("two-proc");
    let t = fx.transcript(&[AGENT_A, AGENT_B]);
    fx.queue_row("bgr-18080000000a", SESSION, AGENT_A);
    fx.queue_row("bgr-18080000000b", SESSION, AGENT_B);
    let entries = match relay::load(&fx.queue_file()) {
        Ok(Queue::Known(v)) => v,
        other => panic!("control: the queue file must fold: {other:?}"),
    };
    let agents = SessionAgents::of(Some(&t));
    for id in ["bgr-18080000000a", "bgr-18080000000b"] {
        let e = entries
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("control: {id} queued: {entries:?}"));
        assert_eq!(
            relay::provenance(e, SESSION, &agents),
            Provenance::Verified,
            "{id} (agent {:?}, a sidechain of {SESSION}) must be Verified",
            e.agent_id
        );
    }
}

// ------------------------------------------- mark-relayed / mark-denied

/// The row sits in SESSION's file but names OTHER_SESSION (Refuted by the
/// session mismatch alone: AGENT_A is a real subagent of SESSION). It is
/// already executed, so the only thing keeping mark-relayed from marking it
/// is provenance.
#[test]
fn mark_relayed_does_not_mark_session_mismatched_row() {
    let fx = Fx::new("relayed-mismatch");
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000008", OTHER_SESSION, AGENT_A);
    fx.queue_executed("bgr-180800000008");
    let o = fx.send_naming(&t, "bgr-180800000008");
    assert!(o.status.success(), "mark-relayed exits 0: {}", show(&o));
    let text = fx.queue_text();
    assert!(
        !text.contains("\"relayed\""),
        "a Refuted row (session_id {OTHER_SESSION} in the file of {SESSION}) was marked relayed; \
         queue:\n{text}"
    );
}

/// CONTROL for the above: the same shape with a matching session is marked.
#[test]
fn control_mark_relayed_marks_verified_executed_row() {
    let fx = Fx::new("relayed-ok");
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000009", SESSION, AGENT_A);
    fx.queue_executed("bgr-180800000009");
    let o = fx.send_naming(&t, "bgr-180800000009");
    assert!(o.status.success(), "mark-relayed exits 0: {}", show(&o));
    let text = fx.queue_text();
    assert!(
        text.contains("\"relayed\""),
        "control: a Verified executed row named by SendMessage must be marked relayed; \
         queue:\n{text}"
    );
}

/// A command blastguard Denies in the main session (measured `deny` for the
/// main session in subagent_ask_relay_20261114.rs `e_verdicts_unchanged`).
const DENY_CMD: &str = "git reset --hard";

/// mark-denied runs inside blastguard's main-session PreToolUse Deny. It
/// carries no transcript, so only the session mismatch is checked there (the
/// agent_id is not): a row whose session_id differs from its file's must not
/// get a denial recorded.
#[test]
fn mark_denied_does_not_mark_session_mismatched_row() {
    let fx = Fx::new("denied-mismatch");
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row_cmd("bgr-18080000000c", OTHER_SESSION, AGENT_A, DENY_CMD);
    let (o, d) = fx.main_pretooluse(&t, DENY_CMD);
    assert_eq!(
        d,
        "deny",
        "control: {DENY_CMD} is a main-session Deny: {}",
        show(&o)
    );
    let text = fx.queue_text();
    assert!(
        !text.contains("\"denied\""),
        "a Refuted row (session_id {OTHER_SESSION} in the file of {SESSION}) got a denial \
         recorded; queue:\n{text}\n{}",
        show(&o)
    );
    assert!(
        !String::from_utf8_lossy(&o.stdout).contains("bgr-18080000000c"),
        "the deny must not claim to be the re-run of the Refuted row: {}",
        show(&o)
    );
}

/// CONTROL for the above: the same Deny on a matching-session row records it.
#[test]
fn control_mark_denied_marks_matching_session_row() {
    let fx = Fx::new("denied-ok");
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row_cmd("bgr-18080000000d", SESSION, AGENT_A, DENY_CMD);
    let (o, d) = fx.main_pretooluse(&t, DENY_CMD);
    assert_eq!(
        d,
        "deny",
        "control: {DENY_CMD} is a main-session Deny: {}",
        show(&o)
    );
    let text = fx.queue_text();
    assert!(
        text.contains("\"denied\""),
        "control: a matching-session row must get the denial recorded; queue:\n{text}\n{}",
        show(&o)
    );
}

#[test]
fn dismiss_of_matching_row_still_works() {
    let fx = Fx::new("dismiss-ok");
    let t = fx.transcript(&[AGENT_A]);
    fx.queue_row("bgr-180800000007", SESSION, AGENT_A);
    let o = fx.run(
        &[
            "relay",
            "dismiss",
            "bgr-180800000007",
            "--reason",
            "declined",
        ],
        "",
    );
    assert_eq!(o.status.code(), Some(0), "dismiss: {}", show(&o));
    assert!(
        fx.queue_text().contains("\"dismissed\""),
        "dismissal must be recorded"
    );
    let (o, v) = fx.stop(Some(&t));
    assert!(
        !blocked(&v),
        "a dismissed row no longer blocks: {}",
        show(&o)
    );
}
