#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(ask-learning): blastguard's read-only use of the learned table, and the
//! `blastguard shape` subcommand.
//!
//! Spec: docs/specs/blastguard-ask-learning.md (user ruling 2026-11-28),
//! component 2. Written by an agent that does NOT implement it (CLAUDE.md
//! §2(a)). Black-box: every test spawns the built binary.
//!
//! The shape KEY of a command is computed by code that does not exist yet, so
//! no test hard-codes one: every key is obtained from `blastguard shape` for
//! the same payload. Consequently, until `shape` exists every test here fails
//! at that first step (`unknown subcommand `shape``, exit 2) — that is the
//! expected RED.
//!
//! Isolation (same as block_record_20261114.rs): HOME, BLASTGUARD_BLOCK_LOG_DIR,
//! BLASTGUARD_RELAY_DIR, BLASTGUARD_APPROVALS_DIR point into a per-test temp
//! dir; CLAUDE_CODE_SESSION_ID / BLASTGUARD_ASK are removed;
//! BLASTGUARD_LEARNED_TABLE is set explicitly in every run (to a missing path
//! when the test wants "no table"), so the real ~/.blastguard/learned/table.json
//! can never leak in.
//!
//! What these tests do NOT prove: that the learner (overwatch) writes the
//! table blastguard reads (see crates/overwatch/tests/ask_learn_*.rs); that
//! the target-class classification is right for any path class other than
//! the ones exercised (Undetermined via `rm "$f"`, Worktree via
//! `LEARNABLE_CMD` for the learned-Allow tests, a shell rc file, and a
//! system path); that learned decisions survive approval memory / the repeat
//! ledger (those are isolated away here).

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Ask in the main interactive session today (block_record_20261114.rs
/// `b_main_session_ask_records_decision_kind_ask` pins it); hardened to Deny
/// in a subagent.
const ASK_CMD: &str = r#"rm "$f""#;
/// An Ask whose shape IS Allow-eligible under the positive allow-list
/// (askrules/builtin.rs: class `worktree`, `git` with the modelled subcommand
/// `add`, no `?`). `ASK_CMD` is `s4-undetermined` and can never be
/// learned-Allowed any more, so the tests of the learned ALLOW path use this
/// one (re-anchored 2026-11-28 by the round-5 verifier; observed shape
/// `git-whole-tree-stage|s4-worktree|git add -A`, Ask without a table,
/// hardened to Deny in a subagent; every test re-checks the Ask as a control).
///
/// `LEARNABLE_CMD` runs with the returned cwd: a linked git worktree created
/// in a fresh tempdir under `/private/tmp/claude-askrules`
/// (`std::env::temp_dir()` is under `/var/folders`, classed `system`). A
/// scratchpad `rm -rf` was tried first and rejected: with this file's HOME
/// (under `$TMPDIR`) the detector Allows it, so it is no Ask here.
const LEARNABLE_CMD: &str = "git add -A";

fn learnable() -> (tempfile::TempDir, String) {
    std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
    let d = tempfile::Builder::new()
        .prefix("contract-learnable-")
        .tempdir_in("/private/tmp/claude-askrules")
        .unwrap();
    let main = d.path().join("repo");
    std::fs::create_dir_all(&main).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(&main)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "i"]);
    // fixture re-anchored 2026-11-28 (leader ruling): fixed container / scratchpad positions match the real layout
    let wt = d.path().join(".harness-worktrees").join("wt");
    std::fs::create_dir_all(d.path().join(".harness-worktrees")).unwrap();
    git(&["worktree", "add", "-q", wt.to_str().unwrap()]);
    let cwd = wt.to_str().unwrap().to_string();
    (d, cwd)
}
/// System harm: Deny everywhere.
const HARM_CMD: &str = "rm -rf /etc";

const SID_A: &str = "5d0c1f0e-aaaa-4bbb-8ccc-000000000001";

struct Env {
    root: PathBuf,
    home: PathBuf,
    log_dir: PathBuf,
    relay: PathBuf,
    approvals: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-asklearn-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let e = Env {
        home: root.join("home"),
        log_dir: root.join("blocklog"),
        relay: root.join("relay"),
        approvals: root.join("approvals"),
        root,
    };
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::create_dir_all(&e.approvals).unwrap();
    e
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn spawn(e: &Env, table: &Path, args: &[&str], stdin: &str) -> Run {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", &e.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", &e.log_dir)
        .env("BLASTGUARD_RELAY_DIR", &e.relay)
        .env("BLASTGUARD_APPROVALS_DIR", &e.approvals)
        .env("BLASTGUARD_LEARNED_TABLE", table)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("CLAUDE_PROJECT_DIR")
        // Interactive main session: the only place an Ask is printed as Ask.
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE);
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn hook(e: &Env, table: &Path, payload: &Value) -> Run {
    spawn(e, table, &[], &payload.to_string())
}

fn printed(r: &Run) -> String {
    if r.stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let d: Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|x| panic!("stdout not JSON ({x}): {:?}", r.stdout));
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision in {d}"))
        .to_string()
}

fn reason(r: &Run) -> String {
    if r.stdout.trim().is_empty() {
        return String::new();
    }
    let d: Value = serde_json::from_str(r.stdout.trim()).unwrap();
    d["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn bash(cwd: &str, command: &str, agent: Option<&str>) -> Value {
    let mut v = json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID_A,
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    if let Some(a) = agent {
        v["agent_id"] = json!(a);
        v["agent_type"] = json!("general-purpose");
    }
    v
}

/// `blastguard shape` for `payload`. Panics (the expected RED) while the
/// subcommand does not exist.
fn shape_of(e: &Env, table: &Path, payload: &Value) -> Value {
    let r = spawn(e, table, &["shape"], &payload.to_string());
    assert_eq!(
        r.code, 0,
        "`blastguard shape` must exit 0. stdout={:?} stderr={:?}",
        r.stdout, r.stderr
    );
    let v: Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|x| panic!("`blastguard shape` stdout not JSON ({x}): {:?}", r.stdout));
    assert!(v["decision"].is_string(), "decision: {v}");
    assert!(v["rule_id"].is_string(), "rule_id: {v}");
    assert!(v["shape"].is_string(), "shape: {v}");
    assert!(v["system_harm"].is_boolean(), "system_harm: {v}");
    v
}

fn key_of(e: &Env, payload: &Value) -> String {
    let none = e.root.join("no-table.json");
    shape_of(e, &none, payload)["shape"]
        .as_str()
        .unwrap()
        .to_string()
}

fn write_table(e: &Env, name: &str, entries: Value) -> PathBuf {
    let p = e.root.join(name);
    std::fs::write(
        &p,
        serde_json::to_string(&json!({ "version": 1, "entries": entries })).unwrap(),
    )
    .unwrap();
    p
}

fn entry(key: &str, decision: &str, approvals: u64, declines: u64, sessions: u64) -> Value {
    json!({
        "shape": key,
        "decision": decision,
        "approvals": approvals,
        "declines": declines,
        "sessions": sessions,
        "samples": [ASK_CMD],
    })
}

fn records(e: &Env) -> Vec<Value> {
    match std::fs::read_to_string(e.log_dir.join("blocks.jsonl")) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect(),
        Err(_) => vec![],
    }
}

// ------------------------------------------------------------ `shape`

#[test]
fn shape_subcommand_reports_ask_command() {
    let e = env("shape-ask");
    let none = e.root.join("no-table.json");
    // Control: today's verdict for the same payload is Ask.
    let ctl = hook(&e, &none, &bash("/tmp/x", ASK_CMD, None));
    assert_eq!(printed(&ctl), "ask", "fixture drift: {}", ctl.stdout);

    let v = shape_of(&e, &none, &bash("/tmp/x", ASK_CMD, None));
    assert_eq!(v["decision"], "ask", "{v}");
    assert_eq!(v["system_harm"], false, "{v}");
    let rule = v["rule_id"].as_str().unwrap();
    assert!(!rule.is_empty(), "{v}");
    let key = v["shape"].as_str().unwrap();
    let parts: Vec<&str> = key.split('|').collect();
    assert_eq!(
        parts.len(),
        3,
        "key must be <rule_id>|<class>|<verbs>: {key}"
    );
    assert_eq!(parts[0], rule, "first key segment is the rule_id: {key}");
    assert!(!parts[1].is_empty(), "class segment empty: {key}");
    assert_eq!(parts[2], "rm", "verb segment of `rm \"$f\"`: {key}");
}

#[test]
fn shape_verbs_are_basenames_and_tool_name() {
    let e = env("shape-verbs");
    let k = key_of(&e, &bash("/tmp/x", r#"/bin/rm "$f""#, None));
    assert_eq!(k.rsplit('|').next(), Some("rm"), "basename of /bin/rm: {k}");
    let w = json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID_A,
        "cwd": "/tmp/x",
        "tool_name": "Write",
        "tool_input": { "file_path": "/etc/hosts", "content": "x" },
    });
    let k = key_of(&e, &w);
    assert_eq!(
        k.rsplit('|').next(),
        Some("Write"),
        "non-Bash verb is the tool name: {k}"
    );
}

#[test]
fn shape_subcommand_reports_system_harm() {
    let e = env("shape-harm");
    let none = e.root.join("no-table.json");
    let v = shape_of(&e, &none, &bash("/tmp/x", HARM_CMD, None));
    assert_eq!(v["decision"], "deny", "{v}");
    assert_eq!(v["system_harm"], true, "{v}");
}

#[test]
fn shape_subcommand_is_not_affected_by_the_table() {
    let e = env("shape-table");
    let p = bash("/tmp/x", ASK_CMD, None);
    let key = key_of(&e, &p);
    let t = write_table(&e, "allow.json", json!([entry(&key, "allow", 9, 0, 5)]));
    let v = shape_of(&e, &t, &p);
    assert_eq!(
        v["decision"], "ask",
        "shape must not consult the table: {v}"
    );
    assert_eq!(v["shape"], key.as_str());
    let t = write_table(&e, "deny.json", json!([entry(&key, "deny", 0, 1, 1)]));
    assert_eq!(shape_of(&e, &t, &p)["decision"], "ask");
    assert!(
        records(&e).is_empty(),
        "`shape` is not a decision and must not append to blocks.jsonl: {:?}",
        records(&e)
    );
}

// ------------------------------------------------------------ learned allow / deny

#[test]
fn learned_allow_turns_ask_into_allow_in_main() {
    let e = env("allow-main");
    let (_fx, cwd) = learnable();
    let p = bash(&cwd, LEARNABLE_CMD, None);
    let none = e.root.join("no-table.json");
    // Control: without the table the same payload is an Ask.
    let ctl = hook(&e, &none, &p);
    assert_eq!(
        printed(&ctl),
        "ask",
        "fixture drift: stdout={} stderr={} shape={}",
        ctl.stdout,
        ctl.stderr,
        shape_of(&e, &none, &p)
    );
    let key = key_of(&e, &p);
    let t = write_table(&e, "t.json", json!([entry(&key, "allow", 7, 0, 4)]));
    let r = hook(&e, &t, &p);
    assert_eq!(r.code, 0);
    assert_eq!(
        printed(&r),
        "allow",
        "stdout={} stderr={}",
        r.stdout,
        r.stderr
    );
}

#[test]
fn learned_allow_applies_in_subagent_mode() {
    let e = env("allow-sub");
    let none = e.root.join("no-table.json");
    let (_fx, cwd) = learnable();
    let sub = bash(&cwd, LEARNABLE_CMD, Some("agent-1"));
    // Control: the main-session verdict is an Ask (so the subagent Deny below
    // is the hardening of an Ask, not a detector Deny) ...
    let main = hook(&e, &none, &bash(&cwd, LEARNABLE_CMD, None));
    assert_eq!(printed(&main), "ask", "fixture drift: {}", main.stdout);
    // ... and today a subagent Ask is hardened to Deny.
    let ctl = hook(&e, &none, &sub);
    assert_eq!(printed(&ctl), "deny", "fixture drift: {}", ctl.stdout);
    let key = key_of(&e, &bash(&cwd, LEARNABLE_CMD, None));
    let t = write_table(&e, "t.json", json!([entry(&key, "allow", 7, 0, 4)]));
    let r = hook(&e, &t, &sub);
    assert_eq!(
        printed(&r),
        "allow",
        "stdout={} stderr={}",
        r.stdout,
        r.stderr
    );
}

#[test]
fn learned_deny_turns_ask_into_deny() {
    let e = env("deny-main");
    let p = bash("/tmp/x", ASK_CMD, None);
    let key = key_of(&e, &p);
    let t = write_table(&e, "t.json", json!([entry(&key, "deny", 2, 1, 2)]));
    let r = hook(&e, &t, &p);
    assert_eq!(printed(&r), "deny", "{}", r.stdout);
    let why = reason(&r);
    assert!(
        why.contains("learned"),
        "reason must say it was learned: {why}"
    );
    assert!(
        why.contains(&key),
        "reason must name the shape {key}: {why}"
    );
}

#[test]
fn learned_allow_is_logged_with_shape_and_kind() {
    let e = env("allow-log");
    let (_fx, cwd) = learnable();
    let p = bash(&cwd, LEARNABLE_CMD, None);
    // Control (in its own sandbox, so this test's block log only holds the
    // learned run): without the table the same payload is an Ask.
    let c = env("allow-log-ctl");
    let ctl = hook(&c, &c.root.join("no-table.json"), &p);
    assert_eq!(printed(&ctl), "ask", "fixture drift: {}", ctl.stdout);
    let key = key_of(&e, &p);
    let t = write_table(&e, "t.json", json!([entry(&key, "allow", 7, 0, 4)]));
    let r = hook(&e, &t, &p);
    assert_eq!(printed(&r), "allow", "{}", r.stdout);
    let recs = records(&e);
    assert_eq!(
        recs.len(),
        1,
        "exactly one record for the learned decision: {recs:?}"
    );
    let rec = &recs[0];
    assert_eq!(rec["decision"], "allow", "{rec}");
    assert_eq!(rec["decision_kind"], "learned", "{rec}");
    assert_eq!(rec["original_decision"], "ask", "{rec}");
    assert_eq!(rec["shape"], key.as_str(), "{rec}");
    assert_eq!(rec["command"], LEARNABLE_CMD, "{rec}");
    let why = rec["reason"].as_str().unwrap_or("");
    assert!(
        why.contains("approved 7 times across 4 sessions, 0 declines") && why.contains(&key),
        "learned allow reason (spec wording) must carry counts and shape: {why}"
    );
}

#[test]
fn learned_deny_is_logged_with_kind() {
    let e = env("deny-log");
    let p = bash("/tmp/x", ASK_CMD, None);
    let key = key_of(&e, &p);
    let t = write_table(&e, "t.json", json!([entry(&key, "deny", 2, 1, 2)]));
    let r = hook(&e, &t, &p);
    assert_eq!(printed(&r), "deny");
    let recs = records(&e);
    assert_eq!(recs.len(), 1, "{recs:?}");
    assert_eq!(recs[0]["decision"], "deny");
    assert_eq!(recs[0]["decision_kind"], "learned", "{}", recs[0]);
    assert_eq!(recs[0]["original_decision"], "ask", "{}", recs[0]);
    assert_eq!(recs[0]["shape"], key.as_str());
}

#[test]
fn no_table_keeps_ask_and_records_shape() {
    let e = env("no-table");
    let p = bash("/tmp/x", ASK_CMD, None);
    let key = key_of(&e, &p);
    let none = e.root.join("no-table.json");
    let r = hook(&e, &none, &p);
    assert_eq!(printed(&r), "ask", "{}", r.stdout);
    let recs = records(&e);
    assert_eq!(recs.len(), 1, "{recs:?}");
    assert_eq!(recs[0]["decision_kind"], "ask", "{}", recs[0]);
    assert_eq!(
        recs[0]["shape"],
        key.as_str(),
        "every record gains shape: {}",
        recs[0]
    );
}

#[test]
fn other_shapes_entry_does_not_apply() {
    let e = env("other-shape");
    let p = bash("/tmp/x", ASK_CMD, None);
    let key = key_of(&e, &p);
    let other = format!("{key},zzz");
    let t = write_table(&e, "t.json", json!([entry(&other, "allow", 9, 0, 5)]));
    let r = hook(&e, &t, &p);
    assert_eq!(printed(&r), "ask", "{}", r.stdout);
}

#[test]
fn corrupt_table_keeps_ask_with_note() {
    let e = env("corrupt");
    let p = bash("/tmp/x", ASK_CMD, None);
    let key = key_of(&e, &p);
    // The corrupt file even contains a would-be allow entry for this shape.
    let garbage = e.root.join("garbage.json");
    std::fs::write(
        &garbage,
        format!("{{\"version\":1,\"entries\":[{{\"shape\":\"{key}\",\"decision\":\"allow\""),
    )
    .unwrap();
    let wrong = write_table(&e, "wrong.json", json!([]));
    std::fs::write(
        &wrong,
        serde_json::to_string(&json!({"version": 99, "entries": [entry(&key, "allow", 9, 0, 5)]}))
            .unwrap(),
    )
    .unwrap();
    let dir = e.root.join("dir-table.json");
    std::fs::create_dir_all(&dir).unwrap();
    for t in [&garbage, &wrong, &dir] {
        let r = hook(&e, t, &p);
        assert_eq!(printed(&r), "ask", "{}: {}", t.display(), r.stdout);
        let why = reason(&r).to_lowercase();
        assert!(
            why.contains("learned table"),
            "{}: reason must note the learned table could not be read: {why}",
            t.display()
        );
    }
}

// ------------------------------------------------------------ hard guards

#[test]
fn deny_is_never_overridden_by_learned_allow() {
    let e = env("harm");
    let p = bash("/tmp/x", HARM_CMD, None);
    let key = key_of(&e, &p);
    let t = write_table(&e, "t.json", json!([entry(&key, "allow", 50, 0, 20)]));
    for payload in [p.clone(), bash("/tmp/x", HARM_CMD, Some("agent-1"))] {
        let r = hook(&e, &t, &payload);
        assert_eq!(printed(&r), "deny", "{}", r.stdout);
    }
}

#[test]
fn protected_config_never_learned_allowed() {
    let e = env("protected");
    let home = e.home.to_str().unwrap().to_string();
    let payloads = [
        bash("/tmp/x", &format!("echo x | tee -a {home}/.zshrc"), None),
        json!({
            "hook_event_name": "PreToolUse",
            "session_id": SID_A,
            "cwd": "/tmp/x",
            "tool_name": "Edit",
            "tool_input": {
                "file_path": format!("{home}/.claude/settings.json"),
                "old_string": "a",
                "new_string": "b",
            },
        }),
    ];
    for p in payloads {
        let key = key_of(&e, &p);
        let t = write_table(&e, "t.json", json!([entry(&key, "allow", 50, 0, 20)]));
        let r = hook(&e, &t, &p);
        assert_ne!(
            printed(&r),
            "allow",
            "protected config must never be learned-Allowed ({key}): {}",
            r.stdout
        );
    }
}
