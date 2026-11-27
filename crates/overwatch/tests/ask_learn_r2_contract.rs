#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(needs ruling): the learner's outcome classification must not count a
//! HOOK DENIAL as a user approval.
//!
//! Written by an independent test author, not the implementer (CLAUDE.md
//! §2(a)). Same fixture method as ask_learn_contract.rs: blocks.jsonl lines
//! come from the real blastguard hook, and transcripts carry a tool_use plus
//! tool_result pair.
//!
//! Why this is a fail-open: blastguard logs an interactive Ask, and then
//! ANOTHER PreToolUse hook (or blastguard itself on re-entry) denies the call.
//! The user was never asked, and the tool never ran, but the transcript holds a
//! tool_result. The spec (docs/specs/blastguard-ask-learning.md, component 3)
//! says verbatim: "its tool_result containing \"The user doesn't want to
//! proceed with this tool use\" → Declined; any other tool_result → Approved".
//! The spec does NOT exclude hook denials, so under the spec as written, three
//! hook-denied calls in two sessions promote the shape to a learned Allow, even
//! though no human approved anything. The independent verifier (2026-11-28)
//! flagged this as UNVERIFIED fail-open.
//!
//! The test is `#[ignore]`d as RED(needs ruling): the spec's text currently
//! mandates the opposite, so the implementer cannot make it GREEN without a
//! ruling that amends "any other tool_result → Approved".
//!
//! What this does NOT prove: the exact strings Claude Code writes for every
//! hook-denial variant (the two used here are the ones observed in this
//! repo's sessions: "PreToolUse:Bash hook error: blastguard: …" and
//! "… hook blocking error …"); classification of non-hook tool errors (an
//! errored command IS an approval per spec, see ask_learn_contract.rs).

#[path = "support/blastguard_bin.rs"]
mod blastguard_bin;

use serde_json::{json, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const S1: &str = "7a1e0c55-0001-4d2e-9f00-00000000bbb1";
const S2: &str = "7a1e0c55-0002-4d2e-9f00-00000000bbb2";
const CWD: &str = "/tmp/ask-learn-r2-fixture-cwd";

static TOOL_ID: AtomicU64 = AtomicU64::new(1);

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    blocks_dir: PathBuf,
    projects: PathBuf,
    table: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let f = Fx {
            home: root.join("home"),
            blocks_dir: root.join("blocks"),
            projects: root.join("projects"),
            table: root.join("learned").join("table.json"),
            root,
            _tmp: tmp,
        };
        for d in [
            &f.home,
            &f.blocks_dir,
            &f.projects.join("-tmp-fixture"),
            &f.root.join("approvals"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::create_dir_all(f.table.parent().unwrap()).unwrap();
        f
    }
}

fn tool_input(cmd: &str) -> Value {
    json!({ "command": cmd, "description": "fixture" })
}

fn payload(sid: &str, cmd: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": sid,
        "cwd": CWD,
        "tool_name": "Bash",
        "tool_input": tool_input(cmd),
    })
}

fn bg(f: &Fx, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(blastguard_bin::blastguard_bin());
    c.args(args)
        .env("HOME", &f.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", &f.blocks_dir)
        .env("BLASTGUARD_RELAY_DIR", f.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", f.root.join("approvals"))
        .env(
            "BLASTGUARD_LEARNED_TABLE",
            f.root.join("hook-sees-no-table.json"),
        )
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().expect("spawn blastguard");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn record_ask(f: &Fx, sid: &str, cmd: &str) {
    let out = bg(f, &[], &payload(sid, cmd).to_string());
    let s = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(s.trim())
        .unwrap_or_else(|e| panic!("fixture: no decision ({e}) for {cmd:?}: {s:?}"));
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "ask",
        "fixture drift: {cmd:?} must be an interactive Ask today: {v}"
    );
}

fn shape_key(f: &Fx, cmd: &str) -> String {
    let out = bg(f, &["shape"], &payload(S1, cmd).to_string());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["shape"].as_str().unwrap().to_string()
}

fn append(path: &Path, v: Value) {
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(fh, "{}", serde_json::to_string(&v).unwrap()).unwrap();
}

fn transcript(f: &Fx, sid: &str) -> PathBuf {
    f.projects.join("-tmp-fixture").join(format!("{sid}.jsonl"))
}

/// An Ask that a hook denied: the user was never prompted, the tool never ran.
fn hook_denied(f: &Fx, sid: &str, cmd: &str, content: &str) {
    record_ask(f, sid, cmd);
    let id = format!("toolu_r2fx{:06}", TOOL_ID.fetch_add(1, Ordering::SeqCst));
    append(
        &transcript(f, sid),
        json!({
            "type": "assistant", "sessionId": sid, "cwd": CWD,
            "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": id, "name": "Bash", "input": tool_input(cmd) }
            ]}
        }),
    );
    append(
        &transcript(f, sid),
        json!({
            "type": "user", "sessionId": sid, "cwd": CWD,
            "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": id, "content": content, "is_error": true }
            ]}
        }),
    );
}

fn ask_learn(f: &Fx) -> Output {
    Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .arg("ask-learn")
        .arg("--blocks")
        .arg(f.blocks_dir.join("blocks.jsonl"))
        .arg("--projects")
        .arg(&f.projects)
        .arg("--table")
        .arg(&f.table)
        .arg("--blastguard")
        .arg(blastguard_bin::blastguard_bin())
        .env("HOME", &f.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", &f.blocks_dir)
        .env_remove("BLASTGUARD_LEARNED_TABLE")
        .stdin(Stdio::null())
        .output()
        .expect("spawn overwatch")
}

const HOOK_ERROR: &str = "PreToolUse:Bash hook error: blastguard: this call was made inside a \
subagent, where nobody can answer a permission prompt, so it is denied instead of asked. Do not \
retry it another way.";
const HOOK_BLOCKING: &str = "PreToolUse:Bash [${CLAUDE_PLUGIN_ROOT}/bin/othergate] failed with \
non-blocking status code: hook blocking error from command: denied by othergate";

#[test]
#[ignore = "RED(needs ruling): hook-denied results counted as approval"]
fn hook_denied_tool_results_are_not_approvals() {
    for (label, content) in [
        ("hook error", HOOK_ERROR),
        ("hook blocking error", HOOK_BLOCKING),
    ] {
        let f = Fx::new();
        // 3 records in 2 sessions: exactly the allow threshold if they were
        // (wrongly) counted as approvals.
        hook_denied(&f, S1, r#"rm "$f1""#, content);
        hook_denied(&f, S1, r#"rm "$f2""#, content);
        hook_denied(&f, S2, r#"rm "$f3""#, content);
        let key = shape_key(&f, r#"rm "$f1""#);
        let out = ask_learn(&f);
        assert!(
            out.status.success(),
            "{label}: ask-learn failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let t: Value = match std::fs::read_to_string(&f.table) {
            Ok(s) => serde_json::from_str(&s).unwrap(),
            Err(_) => json!({"version": 1, "entries": []}),
        };
        let hit = t["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["shape"] == key.as_str())
            .cloned();
        assert!(
            hit.as_ref()
                .map(|e| e["decision"] != "allow")
                .unwrap_or(true),
            "{label}: three hook-DENIED calls (no human approval, tool never ran) were \
             counted as approvals and promoted {key} to allow: {hit:?}"
        );
        if let Some(e) = &hit {
            assert_eq!(
                e["approvals"], 0,
                "{label}: hook denials must be excluded, not counted as approvals: {e}"
            );
        }
    }
}
