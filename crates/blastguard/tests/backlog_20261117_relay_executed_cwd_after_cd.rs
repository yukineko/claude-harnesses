#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-16, session df7c968f, blastguard 0.2.672 or so; repo
//! rev 57cff59f1).
//!
//! Observation (verbatim from the report): relay `bgr-26ea8713935e` (cwd
//! `/home/hiroyuki_nakayama/src/.harness-worktrees/session-df7c968f`) was run
//! verbatim by main in a Bash call with the same cwd and succeeded (exit 0, the
//! output was obtained). But the queue file
//! (`~/.blastguard/relay/<session>.jsonl`) held only the `queued` event, and
//! the next Stop hook listed it again as `state: not executed in the main
//! session`. The command changes cwd while it runs
//! (`cd /home/.../red-trace && ...; cd $T/w; ...`), and after it ran the Bash
//! tool's cwd had moved to a temporary directory.
//!
//! Hypothesis (stated in the task as unverified, checked here): the
//! PostToolUse side matches on cwd, so when the payload's `cwd` is the
//! post-run directory the match fails. Code read: `relay_mark_executed`
//! (src/main.rs) passes `input.cwd` of the PostToolUse payload to
//! `relay::mark_executed` (src/relay.rs), which requires
//! `e.command == command && e.cwd == cwd` against the `queued` entry's cwd,
//! compared as an opaque string. Nothing else records `executed`.
//!
//! Two readings (this test does not choose between them):
//!  (a) binary defect: the entry is identified by (command, cwd) where the cwd
//!      is read from the PostToolUse payload, which describes the shell's cwd
//!      AFTER the command; a command that `cd`s therefore never matches its
//!      own queued entry, and the main session cannot ever resolve it (only
//!      `relay dismiss` can).
//!  (b) intended: the exact-cwd match is deliberate (user ruling
//!      2026-11-14: the relay re-run must be "same cwd, same string"; nothing
//!      resolves or enters the stored cwd), and a command that moves the
//!      shell is simply one the main session must dismiss with a reason.
//!      Distinguish: the ruling constrains what the main agent re-runs (the
//!      cwd the command STARTS in) and forbids resolving the stored path; it
//!      does not say the PostToolUse payload's post-run cwd is the identity.
//!      If the PostToolUse `cwd` is by contract the post-run cwd, (b) would
//!      make every cd-ing relay command unresolvable, which contradicts the
//!      ruling's goal (the main session can adjudicate it); if the payload's
//!      cwd is the start cwd, the hypothesis is moot and the cause is
//!      elsewhere.
//!
//! Test: a subagent Ask is queued with cwd C; the main session runs the same
//! command (PreToolUse with cwd C), and the PostToolUse hook (`askrelay
//! mark-executed`, formerly `blastguard relay mark-executed`) is fed the same
//! command with `cwd` = a different directory D (the post-run cwd). Asserted:
//! `askrelay stop` no longer lists the id as `not executed in the main
//! session`. CONTROL in the same file: the identical PostToolUse with `cwd` =
//! C marks it executed. Since #1808 the fixture also writes the agent's
//! sidechain (`<root>/transcript/subagents/agent-<AGENT>.jsonl`): without it
//! the row is Refuted and Stop does not list it.
//!
//! DOES NOT PROVE: what `cwd` real Claude Code puts in the PostToolUse payload
//! after a command that `cd`s (that the payload carries the post-run cwd is
//! taken from the report's "the Bash cwd had moved to a temporary directory",
//! not observed in a payload); that the observed `bgr-26ea8713935e` command
//! was exactly this shape (the command here is a stand-in with the same `cd`
//! behaviour); the PostToolUseFailure path (same function, same match); the
//! SendMessage/relayed half.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SESSION: &str = "relay-cd-sess-20261117";
const AGENT: &str = "a0c1d2e3f4a5b6c7d";

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(tag: &str) -> Root {
    let root = std::env::temp_dir().join(format!("bg-relay-cd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["home", "approvals", "relay", "cwd", "after", "other"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
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
    // #1808: provenance is read from the subagent sidechain
    // `<root>/transcript/subagents/agent-<AGENT>.jsonl` (the transcript's
    // stem is `transcript`).
    let sub = root.join("transcript").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        sub.join(format!("agent-{AGENT}.jsonl")),
        format!(
            "{}\n",
            json!({"type":"user","sessionId":SESSION,"agentId":AGENT,"isSidechain":true,
                   "message":{"role":"user","content":"task"}})
        ),
    )
    .unwrap();
    Root(root)
}

/// The relay subcommands moved out of blastguard into the `askrelay` binary
/// (4766abe3c); since 10c8c7371 `blastguard relay <sub>` exits 2 and prints
/// nothing to stdout. `["relay", sub, ..]` therefore runs `askrelay sub ..`;
/// every other argument list is a blastguard PreToolUse call.
fn command_for(args: &[&str]) -> Command {
    match args.split_first() {
        Some((&"relay", rest)) => {
            let mut c = Command::new(askrelay_bin());
            c.args(rest);
            c
        }
        _ => {
            let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
            c.args(args);
            c
        }
    }
}

/// The askrelay binary next to the blastguard one, built with the same
/// profile. Cargo only provides `CARGO_BIN_EXE_*` for this package's own
/// binaries. A failed build panics: a missing counterpart must fail the test,
/// never skip it.
fn askrelay_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let me = PathBuf::from(env!("CARGO_BIN_EXE_blastguard"));
        let dir = me.parent().expect("blastguard binary has a parent dir");
        let exe = dir.join(format!("askrelay{}", std::env::consts::EXE_SUFFIX));
        let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        build.args(["build", "-p", "askrelay", "--bin", "askrelay", "--quiet"]);
        match dir.file_name().and_then(|n| n.to_str()) {
            Some("debug") => {}
            Some("release") => {
                build.arg("--release");
            }
            other => panic!("cannot tell the cargo profile from target dir {other:?}"),
        }
        let o = bounded::run_bounded(&mut build, b"", std::time::Duration::from_secs(600));
        assert!(
            o.status.success() && exe.exists(),
            "could not build the askrelay binary the relay half drives: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        exe
    })
    .clone()
}

fn run(root: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = command_for(args);
    c.env("HOME", root.join("home"))
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

/// A command that is an Ask for a subagent (a `git checkout --` loop) and that
/// moves the shell's cwd while it runs.
fn cd_ask_cmd(root: &Path) -> String {
    format!(
        r#"cd {} && for f in a b; do git checkout -- "$f"; done; cd {}; echo x > {}"#,
        root.join("cwd").display(),
        root.join("after").display(),
        root.join("other").join("f.txt").display()
    )
}

fn pre(root: &Path, cwd: &Path, command: &str, agent: Option<&str>) -> Value {
    let mut p = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
        "cwd": cwd.to_string_lossy(),
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

fn post(root: &Path, cwd: &Path, command: &str) -> Output {
    let p = json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
        "tool_response": { "stdout": "ok", "stderr": "", "interrupted": false },
        "cwd": cwd.to_string_lossy(),
        "session_id": SESSION,
        "transcript_path": root.join("transcript.jsonl").to_string_lossy(),
    });
    run(root, &["relay", "mark-executed"], &p.to_string())
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

fn queue_text(root: &Path) -> String {
    std::fs::read_to_string(root.join("relay").join(format!("{SESSION}.jsonl")))
        .expect("control: the subagent Ask must have been queued")
}

fn queued_id(root: &Path) -> String {
    let q = queue_text(root);
    let ids: Vec<String> = q
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event"] == "queued")
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 1, "control: exactly one queued entry: {q}");
    ids[0].clone()
}

/// Queue the command as a subagent Ask with cwd `<root>/cwd`; return its id.
fn queue_entry(root: &Path, cmd: &str) -> String {
    let v = pre(root, &root.join("cwd"), cmd, Some(AGENT));
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "deny",
        "control: subagent Ask hardened to deny: {v}"
    );
    let id = queued_id(root);
    let before = stop_text(root);
    assert!(
        before.contains(&id) && before.contains("state: not executed in the main session"),
        "control: the fresh entry is listed as not executed: {before}"
    );
    id
}

/// CONTROL: the PostToolUse carrying the queued cwd marks the entry executed.
#[test]
fn control_post_with_queued_cwd_marks_executed() {
    let root = setup("control");
    let cmd = cd_ask_cmd(&root.0);
    let id = queue_entry(&root.0, &cmd);
    // Main session runs the exact command in the same cwd.
    let _ = pre(&root.0, &root.0.join("cwd"), &cmd, None);
    let o = post(&root.0, &root.0.join("cwd"), &cmd);
    assert_eq!(o.status.code(), Some(0), "control: mark-executed: {o:?}");
    let after = stop_text(&root.0);
    assert!(
        !after.contains("state: not executed in the main session"),
        "control: same-cwd PostToolUse must mark {id} executed: {after}\nqueue: {}",
        queue_text(&root.0)
    );
}

// test-weakening-justified: new RED for an unfixed defect (2026-11-17).
#[test]
fn post_with_changed_cwd_after_cd_still_marks_executed() {
    let root = setup("after-cd");
    let cmd = cd_ask_cmd(&root.0);
    let id = queue_entry(&root.0, &cmd);
    // Main session runs the exact command, started in the same cwd; the shell
    // ended up in a different directory, which is what PostToolUse reports.
    let _ = pre(&root.0, &root.0.join("cwd"), &cmd, None);
    let o = post(&root.0, &root.0.join("after"), &cmd);
    assert_eq!(o.status.code(), Some(0), "mark-executed ran: {o:?}");
    let after = stop_text(&root.0);
    assert!(
        !after.contains("state: not executed in the main session"),
        "main ran {id} verbatim (started in the queued cwd, exit 0) but the PostToolUse cwd was \
         the post-run directory, so no `executed` event was written and Stop still says \
         `not executed in the main session`: {after}\nqueue file: {}",
        queue_text(&root.0)
    );
}
