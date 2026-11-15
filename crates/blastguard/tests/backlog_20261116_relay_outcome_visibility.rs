#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog 2026-11-16): two relay-queue visibility defects, both observed
//! in the live queue `~/.blastguard/relay/06c2bd77-5d52-4545-ba25-a562c43633ab.jsonl`
//! on entry `bgr-a74f09f643a4`.
//!
//! 1. `second_dismissal_does_not_erase_the_first_from_stop_text`
//!    Observed: the entry has two `dismissed` lines (ts 1794764154 "user ruling
//!    2026-11-16: blastguard is being redone ..." and ts 1794764173 "executed
//!    byte-for-byte in the main session; blastguard denied it there too ...").
//!    `relay::load` folds `e.dismissed = Some(reason)`, so the Stop text shows
//!    only the later reason; the first dismissal disappears from every later
//!    Stop although relay.rs's module doc says "every dismissal of the session
//!    is shown by the Stop hook from then on, so a dismissal is never silent".
//!    Reading (a) (asserted): a dismissal is final or append-visible — after a
//!    second `relay dismiss` on the same id, the first reason is still shown by
//!    `relay stop` (whether the second dismiss is refused or both are listed).
//!    Reading (b): "every dismissal" means every dismissed ENTRY, and the latest
//!    reason superseding the earlier one is intended (the full history stays in
//!    the queue file).
//!
//! 2. `main_session_deny_of_the_relayed_command_is_recorded`
//!    Observed: the subagent's command was an Ask (hardened to deny, queued);
//!    the main session re-ran it byte-for-byte and blastguard DENIED it there
//!    ("'> ...' destroys the file's current contents and it is inside a git
//!    work tree, but not one of this session's trees ..."). No `executed` line
//!    was written; the only trace of that attempt is the free-text dismissal
//!    reason. `relay stop` cannot tell "re-run in main and denied" from "never
//!    re-run": both print `state: not executed in the main session`.
//!    Reading (a) (asserted): a main-session PreToolUse that denies the exact
//!    queued command (same cwd) leaves a relay-visible outcome, so the entry's
//!    Stop state is no longer the never-attempted `not executed in the main
//!    session`.
//!    Reading (b): the relay records only what PostToolUse reports; a denied
//!    re-run is the agent's to explain via `relay dismiss --reason`, which is
//!    what happened at ts 1794764173.
//!
//! DOES NOT PROVE: what real Claude Code fires after a PreToolUse deny
//! (whether PostToolUseFailure runs is not observed here — the live queue only
//! shows no `executed` line followed); which process/agent wrote each dismissal
//! (no owner field exists and none is asserted); anything about the
//! two-processes-one-session sharing of the queue.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SESSION: &str = "relay-outcome-sess-20261116";
const AGENT: &str = "a0c1d2e3f4a5b6c7d";

fn setup(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("bg-relay-outcome-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["home", "approvals", "relay", "cwd", "other"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // The session transcript names the subagent, so the entry is not a forged
    // one (see backlog_20261116_relay_forged_session_entry.rs).
    std::fs::write(
        root.join("transcript.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"type":"user","sessionId":SESSION,"message":{"role":"user","content":"hi"}}),
            json!({"type":"assistant","sessionId":SESSION,"agentId":AGENT,"isSidechain":true,
                   "message":{"role":"assistant","content":"working"}})
        ),
    )
    .unwrap();
    root
}

fn run(root: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", root.join("home"))
        // TMPDIR is a safe root; keep it off `other/` so `other/` is not this
        // session's tree (as the sibling worktree was in the observation).
        .env("TMPDIR", root.join("home"))
        .env("BLASTGUARD_RELAY_DIR", root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", root.join("approvals"))
        .env("CLAUDE_CODE_SESSION_ID", "sessEnvIgnored")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE)
}

fn pre(root: &Path, command: &str, agent: Option<&str>) -> Value {
    let mut p = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
        "cwd": root.join("cwd").to_string_lossy(),
        "session_id": SESSION,
        "transcript_path": root.join("transcript.jsonl").to_string_lossy(),
    });
    if let Some(a) = agent {
        p["agent_id"] = json!(a);
        p["agent_type"] = json!("general-purpose");
    }
    let o = run(root, &[], &p.to_string());
    serde_json::from_slice(&o.stdout).unwrap_or(Value::Null)
}

fn stop_text(root: &Path) -> String {
    let p = json!({
        "hook_event_name": "Stop",
        "session_id": SESSION,
        "cwd": root.join("cwd").to_string_lossy(),
        "transcript_path": root.join("transcript.jsonl").to_string_lossy(),
        "stop_hook_active": false,
    });
    let o = run(root, &["relay", "stop"], &p.to_string());
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn queued_id(root: &Path) -> String {
    let q = std::fs::read_to_string(root.join("relay").join(format!("{SESSION}.jsonl")))
        .expect("control: the subagent Ask must have been queued");
    let ids: Vec<String> = q
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event"] == "queued")
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 1, "control: exactly one queued entry: {q}");
    ids[0].clone()
}

fn ask_cmd(root: &Path) -> String {
    format!(
        r#"for f in a b; do git checkout -- "$f"; done; echo x > {}"#,
        root.join("other").join("f.txt").display()
    )
}

#[test]
// test-weakening-justified: new RED for an unfixed defect (backlog pending, 2026-11-16); the ignore is removed in the fixing commit.
fn second_dismissal_does_not_erase_the_first_from_stop_text() {
    let root = setup("dismiss2");
    let v = pre(&root, &ask_cmd(&root), Some(AGENT));
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "deny",
        "control: subagent Ask hardened to deny: {v}"
    );
    let id = queued_id(&root);
    const R1: &str = "FIRST-DISMISSAL-REASON user ruling: not adjudicated here";
    const R2: &str = "SECOND-DISMISSAL-REASON executed in main, denied there too";
    let d1 = run(&root, &["relay", "dismiss", &id, "--reason", R1], "");
    assert_eq!(d1.status.code(), Some(0), "control: first dismiss: {d1:?}");
    let s1 = stop_text(&root);
    // test-weakening-justified: user ruling 2026-11-16 「表示をやめてほしい」 — dismissed entries are no longer listed on Stop
    assert!(
        !s1.contains("FIRST-DISMISSAL-REASON") && !s1.contains(&id),
        "dismissed entry listed on Stop: {s1}"
    );
    let _d2 = run(&root, &["relay", "dismiss", &id, "--reason", R2], "");
    let s2 = stop_text(&root);
    // test-weakening-justified: user ruling 2026-11-16 「表示をやめてほしい」 — dismissed entries are no longer listed on Stop
    assert!(
        !s2.contains("FIRST-DISMISSAL-REASON") && !s2.contains("SECOND-DISMISSAL-REASON"),
        "dismissed reasons listed on Stop: {s2}"
    );
    // Re-anchored property (b82e882a): the second dismissal does not erase the
    // first — both reasons stay in the queue file.
    let q = std::fs::read_to_string(root.join("relay").join(format!("{SESSION}.jsonl"))).unwrap();
    assert!(
        q.contains("FIRST-DISMISSAL-REASON") && q.contains("SECOND-DISMISSAL-REASON"),
        "queue file lost a dismissal reason: {q}"
    );
}

#[test]
// test-weakening-justified: new RED for an unfixed defect (backlog pending, 2026-11-16); the ignore is removed in the fixing commit.
fn main_session_deny_of_the_relayed_command_is_recorded() {
    let root = setup("maindeny");
    let cmd = ask_cmd(&root);
    // Subagent: target absent -> the redirect is a plain create, the
    // `git checkout -- "$f"` loop is an Ask, hardened to deny and queued.
    let v = pre(&root, &cmd, Some(AGENT));
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "deny",
        "control: subagent Ask hardened to deny: {v}"
    );
    let id = queued_id(&root);
    let before = stop_text(&root);
    assert!(
        before.contains(&id) && before.contains("state: not executed in the main session"),
        "control: before any main-session attempt the entry is listed as not executed: {before}"
    );
    // Meanwhile the target comes to exist as a tracked file of a git tree that
    // is not this session's (in the observation the subagent wrote it with
    // the Write tool between the queue and the main-session re-run).
    let other = root.join("other");
    std::fs::write(other.join("f.txt"), "x\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "f.txt"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "x",
        ],
    ] {
        let s = Command::new("git")
            .arg("-C")
            .arg(&other)
            .args(&args)
            .status()
            .unwrap();
        assert!(s.success(), "control: git {args:?}");
    }
    // Main session re-runs the EXACT command in the same cwd: denied there.
    let m = pre(&root, &cmd, None);
    assert_eq!(
        m["hookSpecificOutput"]["permissionDecision"], "deny",
        "control: the main session's byte-for-byte re-run is denied: {m}"
    );
    let after = stop_text(&root);
    assert!(
        !after.contains("state: not executed in the main session"),
        "the main session re-ran {id} byte-for-byte and blastguard denied it ({}), but the relay \
         still describes it exactly as a never-attempted entry: {after}",
        m["hookSpecificOutput"]["permissionDecisionReason"]
    );
}
