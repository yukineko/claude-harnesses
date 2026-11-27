#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (independent verifier, 2026-11-28): two ways a learned Allow reaches a
//! call the spec says it must never reach (docs/specs/blastguard-ask-learning.md).
//!
//! 1. Protected config through a verb the shape facts do not model. The shape
//!    facts take targets only from redirects and Table B rows, so `touch`,
//!    `vim` and other unmodelled verbs get target class `none`, and
//!    `perl -pi … <file>` gets `undetermined`. The detector itself names the
//!    call a protected-path Ask (`unknown-verb-protected-path`,
//!    `interpreter-operand-protected-path`), but the class is neither
//!    `protected-config` nor `system`, so `Shape::allow_permitted` holds and an
//!    allow entry is honoured. Spec: "protected config (`~/.claude/settings*.json`,
//!    `~/.zshrc` / shell rc files, …) are never auto-Allowed".
//!    `ask_learning_contract::protected_config_never_learned_allowed` only covers
//!    `tee` and `Edit`, whose targets ARE modelled, so it does not catch this.
//!
//! 2. A table holding both an allow and a deny entry for one shape: `lookup`
//!    takes the first match, so the allow wins. The table is untrusted input
//!    (asklearn.rs module doc). "Deny as soon as the shape has ≥1 decline"
//!    means deny must win.
//!
//! What this does NOT prove: that the learner would ever produce such a table
//! from real records (for 1 it would, after 3 approvals in 2 sessions; for 2
//! it cannot, since `learn` keys a BTreeMap). It also does not cover verb-set
//! over-generalisation (`unknown|none|git`), which follows the spec's shape
//! ruling and needs a ruling, not a test.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID_A: &str = "5d0c1f0e-aaaa-4bbb-8ccc-000000000001";

struct Env {
    root: PathBuf,
    home: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-verif-reach-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let e = Env {
        home: root.join("home"),
        root,
    };
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::create_dir_all(e.root.join("approvals")).unwrap();
    e
}

fn spawn(e: &Env, table: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", &e.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
        .env("BLASTGUARD_LEARNED_TABLE", table)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn printed(stdout: &str) -> String {
    if stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let d: Value = serde_json::from_str(stdout.trim()).unwrap();
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap()
        .to_string()
}

fn bash(command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID_A,
        "cwd": "/tmp/x",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

fn shape(e: &Env, p: &Value) -> Value {
    let (code, out, err) = spawn(e, &e.root.join("no-table.json"), &["shape"], &p.to_string());
    assert_eq!(code, 0, "shape: {out} {err}");
    serde_json::from_str(out.trim()).unwrap()
}

fn table(e: &Env, entries: Value) -> PathBuf {
    let p = e.root.join("t.json");
    std::fs::write(&p, json!({"version": 1, "entries": entries}).to_string()).unwrap();
    p
}

fn entry(key: &str, decision: &str, declines: u64) -> Value {
    json!({"shape": key, "decision": decision, "approvals": 50, "declines": declines,
           "sessions": 20, "samples": []})
}

#[test]
fn protected_config_through_unmodelled_verb_is_never_learned_allowed() {
    let e = env("unmodelled");
    for cmd in [
        "touch ~/.claude/settings.json",
        "vim ~/.zshrc",
        "perl -pi -e s/a/b/ ~/.claude/settings.json",
    ] {
        let p = bash(cmd);
        let s = shape(&e, &p);
        // Precondition: today this is an Ask the detector itself files under a
        // protected-path rule.
        assert_eq!(s["decision"], "ask", "{cmd}: {s}");
        assert!(
            s["rule_id"].as_str().unwrap().contains("protected-path"),
            "{cmd}: {s}"
        );
        let key = s["shape"].as_str().unwrap();
        let t = table(&e, json!([entry(key, "allow", 0)]));
        let (_, out, err) = spawn(&e, &t, &[], &p.to_string());
        assert_ne!(
            printed(&out),
            "allow",
            "`{cmd}` writes protected config, which the spec says is never auto-Allowed, \
             but the learned entry for shape {key} allowed it. stderr={err}"
        );
    }
}

#[test]
fn deny_entry_wins_over_an_allow_entry_for_the_same_shape() {
    let e = env("dupkey");
    let p = bash("git push --force origin main");
    let s = shape(&e, &p);
    assert_eq!(s["decision"], "ask", "{s}");
    let key = s["shape"].as_str().unwrap();
    let t = table(&e, json!([entry(key, "allow", 0), entry(key, "deny", 1)]));
    let (_, out, _) = spawn(&e, &t, &[], &p.to_string());
    assert_eq!(
        printed(&out),
        "deny",
        "a shape with a deny entry (≥1 decline) must be denied even when the table also \
         holds an allow entry for it ({key}): {out}"
    );
}
