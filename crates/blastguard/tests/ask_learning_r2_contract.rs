#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED(ask-learning r2): shape granularity and self-grant protection for the
//! learned-table overlay. Written by an independent test author, not the
//! implementer (CLAUDE.md §2(a)). Black-box: every test spawns the built
//! binary (same isolation as ask_learning_contract.rs).
//!
//! Binding user rulings, 2026-11-28 (on top of docs/specs/blastguard-ask-learning.md):
//!   R1  Shape verbs include the subcommand and the destructive flags, not just
//!       the basename: `git push --force` / `git branch -D` / `git add -A` must
//!       not share a key with `git status` / `git push` / `git branch`.
//!   R2  Inline interpreter programs (python3/perl/ruby/node/bash/sh … with a
//!       heredoc body, -c or -e) carry a hash of the program body in the shape.
//!       Only repeats of the same body are learned; a changed body Asks again.
//!
//! Self-grant (tests `self_grant_*`): the learned table, promotions.jsonl and
//! status.json under `~/.blastguard/learned/` are treated as protected config,
//! so writes or deletes there are never Allowed. NOTE: this is an INFERENCE.
//! No ruling names that directory. It combines (a) the spec's protected-config
//! ruling, which lists "plugin cache bins/hooks" (files that decide what the
//! gate does), and (b) the autonomy self-grant rule: an agent must not be able
//! to grant itself permissions. A Bash/Write call that rewrites the learned
//! table would grant the agent arbitrary future Allows. If the user rules
//! otherwise, these tests are re-anchored, not deleted.
//!
//! The key string format is NOT pinned here. Tests compare keys for
//! (in)equality and check that the subcommand token appears in the key. The
//! only structural use of the format is `verb_part`, which takes the text after
//! the second `|`, the spec's canonical `"<rule_id>|<class>|<verbs>"`. That
//! check makes sure R1 distinctness comes from the verbs, not only from a
//! differing rule_id.
//!
//! What these tests do NOT prove: that the learner records the finer shapes
//! (the learner recomputes shapes via `blastguard shape`, so it inherits them);
//! that every destructive flag of every verb is modelled (only the listed forms
//! are checked); decline generalisation (not ruled, deliberately untested).

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID_A: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000a1";
const SID_B: &str = "5d0c1f0e-aaaa-4bbb-8ccc-0000000000b2";
const CWD: &str = "/tmp/x";

struct Env {
    root: PathBuf,
    home: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-asklearn-r2-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let e = Env {
        home: root.join("home"),
        root,
    };
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::create_dir_all(e.root.join("approvals")).unwrap();
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
        .env("BLASTGUARD_BLOCK_LOG_DIR", e.root.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", e.root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", e.root.join("approvals"))
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

fn no_table(e: &Env) -> PathBuf {
    e.root.join("no-table.json")
}

/// Printed hook decision; empty stdout is Allow.
fn decide(e: &Env, table: &Path, p: &Value) -> String {
    let r = spawn(e, table, &[], &p.to_string());
    if r.stdout.trim().is_empty() {
        return "allow".to_string();
    }
    let d: Value = serde_json::from_str(r.stdout.trim()).unwrap_or_else(|x| {
        panic!(
            "stdout not JSON ({x}): {:?} stderr={:?}",
            r.stdout, r.stderr
        )
    });
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision in {d}"))
        .to_string()
}

fn bash_as(sid: &str, command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": sid,
        "cwd": CWD,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

fn bash(command: &str) -> Value {
    bash_as(SID_A, command)
}

fn shape(e: &Env, p: &Value) -> Value {
    let r = spawn(e, &no_table(e), &["shape"], &p.to_string());
    assert_eq!(
        r.code, 0,
        "`blastguard shape` must exit 0: stdout={:?} stderr={:?}",
        r.stdout, r.stderr
    );
    serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|x| panic!("shape stdout not JSON ({x}): {:?}", r.stdout))
}

fn key(e: &Env, p: &Value) -> String {
    shape(e, p)["shape"].as_str().unwrap().to_string()
}

/// Text after the second `|` of the spec's canonical
/// `"<rule_id>|<class>|<verbs>"`.
fn verb_part(k: &str) -> &str {
    k.splitn(3, '|').nth(2).unwrap_or("")
}

fn table(e: &Env, name: &str, entries: Vec<Value>) -> PathBuf {
    let p = e.root.join(name);
    std::fs::write(&p, json!({"version": 1, "entries": entries}).to_string()).unwrap();
    p
}

fn allow_entry(k: &str) -> Value {
    json!({"shape": k, "decision": "allow", "approvals": 50, "declines": 0,
           "sessions": 20, "samples": []})
}

// ------------------------------------------------------------------ R1

/// (dangerous form, subcommand token it must carry)
const DANGEROUS: &[(&str, Option<&str>)] = &[
    ("git push --force origin main", Some("push")),
    ("git branch -D main", Some("branch")),
    ("git add -A", Some("add")),
    ("git reset --hard", Some("reset")),
    ("git clean -fdx", Some("clean")),
    ("rm -rf x", None),
];

/// (benign form, subcommand token it must carry)
const BENIGN: &[(&str, Option<&str>)] = &[
    ("git push origin x", Some("push")),
    ("git branch -d x", Some("branch")),
    ("git status", Some("status")),
    ("rm x", None),
];

#[test]
fn r1_dangerous_forms_have_shape_keys_distinct_from_benign_forms() {
    let e = env("r1keys");
    let mut bad = vec![];
    let keyed = |cmd: &str| (cmd.to_string(), key(&e, &bash(cmd)));
    let dangerous: Vec<_> = DANGEROUS.iter().map(|(c, t)| (keyed(c), *t)).collect();
    let benign: Vec<_> = BENIGN.iter().map(|(c, t)| (keyed(c), *t)).collect();
    for ((cmd, k), tok) in dangerous.iter().chain(benign.iter()) {
        if let Some(tok) = tok {
            if !verb_part(k).contains(tok) {
                bad.push(format!(
                    "`{cmd}`: shape {k:?} does not carry subcommand `{tok}` in its verbs (R1)"
                ));
            }
        }
    }
    for ((dc, dk), _) in &dangerous {
        for ((bc, bk), _) in &benign {
            if dk == bk {
                bad.push(format!("`{dc}` and `{bc}` share shape {dk:?} (R1)"));
            } else if verb_part(dk) == verb_part(bk) && same_program(dc, bc) {
                bad.push(format!(
                    "`{dc}` ({dk:?}) and `{bc}` ({bk:?}) differ only outside the verbs; \
                     the destructive form's verbs must differ (R1)"
                ));
            }
        }
    }
    assert!(bad.is_empty(), "R1 violations:\n{}", bad.join("\n"));
}

/// Only compare verb parts of commands that run the same subcommand
/// (`git push` vs `git push --force`, `rm` vs `rm -rf`), so the verb-level check
/// targets exactly "same subcommand, destructive flag added".
fn same_program(a: &str, b: &str) -> bool {
    let ha: Vec<&str> = a.split_whitespace().take(2).collect();
    let hb: Vec<&str> = b.split_whitespace().take(2).collect();
    if ha[0] != hb[0] {
        return false;
    }
    ha[0] != "git" || ha.get(1) == hb.get(1)
}

#[test]
fn r1_allow_learned_for_benign_forms_does_not_cover_dangerous_forms() {
    let e = env("r1overlay");
    let entries: Vec<Value> = BENIGN
        .iter()
        .map(|(c, _)| allow_entry(&key(&e, &bash(c))))
        .collect();
    let t = table(&e, "benign-allow.json", entries);
    let mut bad = vec![];
    for (cmd, _) in DANGEROUS {
        let p = bash(cmd);
        let before = decide(&e, &no_table(&e), &p);
        let after = decide(&e, &t, &p);
        if before != "allow" && after == "allow" {
            bad.push(format!(
                "`{cmd}`: {before} without a table, but Allow once the BENIGN forms' shapes \
                 ({}) are learned allow. shape={}",
                BENIGN
                    .iter()
                    .map(|(c, _)| *c)
                    .collect::<Vec<_>>()
                    .join(", "),
                key(&e, &p)
            ));
        }
    }
    assert!(bad.is_empty(), "R1 overlay leaks:\n{}", bad.join("\n"));
}

/// Two Ask'd destructive git forms with DIFFERENT subcommands must not share a
/// key either: a shape learned from repeated `git branch -D <topic>` approvals
/// must not Allow `git push --force origin main`.
#[test]
fn r1_destructive_forms_of_different_subcommands_do_not_share_a_key() {
    let e = env("r1cross");
    let mut bad = vec![];
    let keys: Vec<(&str, String)> = DANGEROUS
        .iter()
        .map(|(c, _)| (*c, key(&e, &bash(c))))
        .collect();
    for (i, (ca, ka)) in keys.iter().enumerate() {
        for (cb, kb) in &keys[i + 1..] {
            if ka == kb {
                bad.push(format!("`{ca}` and `{cb}` share shape {ka:?} (R1)"));
            }
        }
    }
    assert!(bad.is_empty(), "R1 violations:\n{}", bad.join("\n"));
}

#[test]
fn r1_allow_learned_for_branch_force_delete_does_not_allow_force_push() {
    let e = env("r1crossoverlay");
    let push = bash("git push --force origin main");
    assert_eq!(
        decide(&e, &no_table(&e), &push),
        "ask",
        "fixture: force push must Ask today"
    );
    let learned = key(&e, &bash("git branch -D old-topic"));
    let t = table(&e, "branch.json", vec![allow_entry(&learned)]);
    assert_ne!(
        decide(&e, &t, &push),
        "allow",
        "an allow learned from `git branch -D old-topic` (shape {learned:?}) must not \
         Allow `git push --force origin main` (shape {:?}) (R1)",
        key(&e, &push)
    );
}

#[test]
fn r1_git_status_allow_does_not_cover_git_branch_force_delete() {
    let e = env("r1status");
    let t = table(
        &e,
        "status.json",
        vec![allow_entry(&key(&e, &bash("git status")))],
    );
    let p = bash("git branch -D main");
    assert_ne!(decide(&e, &no_table(&e), &p), "allow", "precondition");
    assert_ne!(
        decide(&e, &t, &p),
        "allow",
        "an allow learned for `git status` must not Allow `git branch -D main` (R1). \
         status shape={} branch shape={}",
        key(&e, &bash("git status")),
        key(&e, &p)
    );
}

// ------------------------------------------------------------------ R2

/// (label, body A command, body B command). A and B do the SAME operation and
/// differ only in an identifier inside the program body, so rule_id, target
/// class and verbs are identical. Only a body hash can tell them apart, which
/// is exactly what R2 requires.
fn r2_cases() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "python3 heredoc",
            "python3 - <<'EOF'\nimport os\nopen(os.environ['OUT_A'], 'w').write('x')\nEOF".into(),
            "python3 - <<'EOF'\nimport os\nopen(os.environ['OUT_B'], 'w').write('x')\nEOF".into(),
        ),
        (
            "python3 -c",
            r#"python3 -c 'import os; open(os.environ["OUT_A"], "w").write("x")'"#.into(),
            r#"python3 -c 'import os; open(os.environ["OUT_B"], "w").write("x")'"#.into(),
        ),
        (
            "perl -e",
            r#"perl -e 'open(F, ">", $ENV{OUT_A}); print F "x"'"#.into(),
            r#"perl -e 'open(F, ">", $ENV{OUT_B}); print F "x"'"#.into(),
        ),
        (
            "bash -c",
            r#"bash -c 'rm "$OUT_A"'"#.into(),
            r#"bash -c 'rm "$OUT_B"'"#.into(),
        ),
    ]
}

#[test]
fn r2_inline_program_body_is_part_of_the_shape() {
    let e = env("r2keys");
    let mut bad = vec![];
    for (label, a, b) in r2_cases() {
        let ka = key(&e, &bash_as(SID_A, &a));
        let ka2 = key(&e, &bash_as(SID_B, &a));
        let kb = key(&e, &bash(&b));
        if ka != ka2 {
            bad.push(format!(
                "{label}: the same body gave different shapes {ka:?} / {ka2:?}"
            ));
        }
        if ka == kb {
            bad.push(format!("{label}: different bodies share shape {ka:?} (R2)"));
        }
    }
    assert!(bad.is_empty(), "R2 violations:\n{}", bad.join("\n"));
}

/// A linked git worktree in a fresh tempdir under `/private/tmp/claude-askrules`
/// (`std::env::temp_dir()` is under `/var/folders`, classed `system`). Its
/// `git add -A` is an Allow-eligible Ask (ask_learning_contract.rs
/// `LEARNABLE_CMD`), used here as the liveness control of the learned table.
fn linked_worktree() -> (tempfile::TempDir, PathBuf) {
    std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
    let d = tempfile::Builder::new()
        .prefix("r2-live-")
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
    (d, wt)
}

/// Re-anchored 2026-11-28 (verifier round 6) to the ruling "an interpreter run
/// (python3/perl/ruby/node, and `bash -c` / `sh -c` bodies) is never
/// Allow-eligible": the property is now STRICTER than R2's "only the same body
/// is learned". An allow entry learned for body A leaves body A itself an Ask,
/// and body B an Ask. Controls: both bodies Ask without a table (the table is
/// otherwise never consulted), and the same table is LIVE (an eligible shape's
/// own entry in it is honoured), so the Ask of A is not a table that was never
/// read.
#[test]
fn r2_allow_learned_for_one_body_leaves_another_body_ask() {
    let e = env("r2overlay");
    let (_live_dir, live_wt) = linked_worktree();
    let live = json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID_A,
        "cwd": live_wt,
        "tool_name": "Bash",
        "tool_input": { "command": "git add -A" },
    });
    assert_eq!(
        decide(&e, &no_table(&e), &live),
        "ask",
        "fixture: the liveness control `git add -A` in a linked worktree must Ask without a table"
    );
    let k_live = key(&e, &live);
    let mut bad = vec![];
    for (label, a, b) in r2_cases() {
        let pa = bash(&a);
        let pb = bash(&b);
        // Control: both bodies are Asks without a table.
        for (which, cmd, p) in [("A", &a, &pa), ("B", &b, &pb)] {
            let d = decide(&e, &no_table(&e), p);
            assert_eq!(
                d, "ask",
                "fixture: {label} {which} must Ask without a table: {cmd}"
            );
        }
        let ka = key(&e, &pa);
        let t = table(&e, "r2.json", vec![allow_entry(&ka), allow_entry(&k_live)]);
        // Liveness control: the table is read and honoured for an eligible shape.
        let got_live = decide(&e, &t, &live);
        if got_live != "allow" {
            bad.push(format!(
                "{label}: control failed, the eligible shape {k_live:?} with its own allow \
                 entry in the same table is {got_live}"
            ));
        }
        let got_a = decide(&e, &t, &pa);
        if got_a != "ask" {
            bad.push(format!(
                "{label}: body A is {got_a} under its OWN learned allow entry; an interpreter \
                 run is never Allow-eligible (ruling 2026-11-28). A={ka:?}"
            ));
        }
        let got_b = decide(&e, &t, &pb);
        if got_b != "ask" {
            bad.push(format!(
                "{label}: body B is {got_b} under an allow learned for body A; it must stay \
                 an Ask. A={ka:?} B={:?}",
                key(&e, &pb)
            ));
        }
    }
    assert!(bad.is_empty(), "R2 overlay leaks:\n{}", bad.join("\n"));
}

// ------------------------------------------------------------------ self-grant

fn write_tool(path: &Path) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SID_A,
        "cwd": CWD,
        "tool_name": "Write",
        "tool_input": { "file_path": path.to_string_lossy(), "content": "{\"version\":1,\"entries\":[]}" },
    })
}

fn self_grant_payloads(e: &Env) -> Vec<(String, Value)> {
    let learned = e.home.join(".blastguard").join("learned");
    let mut v: Vec<(String, Value)> = [
        "echo x > ~/.blastguard/learned/table.json",
        "echo '{}' >> ~/.blastguard/learned/promotions.jsonl",
        "echo '{}' > ~/.blastguard/learned/status.json",
        "rm -rf ~/.blastguard/learned",
        "rm ~/.blastguard/learned/table.json",
    ]
    .iter()
    .map(|c| (format!("Bash `{c}`"), bash(c)))
    .collect();
    v.push((
        "Write table.json".into(),
        write_tool(&learned.join("table.json")),
    ));
    v.push((
        "Write promotions.jsonl".into(),
        write_tool(&learned.join("promotions.jsonl")),
    ));
    v
}

#[test]
fn self_grant_control_protected_classification_follows_sandbox_home() {
    // Control: with HOME pointed at the sandbox, a known protected-config path
    // under it is still non-Allow. Without this, a passing self-grant test
    // could come from HOME being ignored, not from a real protection.
    let e = env("sgcontrol");
    for p in [
        bash("echo x > ~/.zshrc"),
        write_tool(&e.home.join(".claude").join("settings.json")),
    ] {
        assert_ne!(
            decide(&e, &no_table(&e), &p),
            "allow",
            "protected config under the sandbox HOME must not be Allowed: {p}"
        );
    }
}

#[test]
fn self_grant_writes_to_learned_dir_are_never_allowed() {
    let e = env("sgplain");
    let mut bad = vec![];
    for (label, p) in self_grant_payloads(&e) {
        let d = decide(&e, &no_table(&e), &p);
        if d == "allow" {
            bad.push(format!("{label}: Allow (shape {:?})", key(&e, &p)));
        }
    }
    assert!(
        bad.is_empty(),
        "writes/deletes under ~/.blastguard/learned/ grant future Allows (self-grant); \
         they must be protected config, never Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn self_grant_learned_allow_entry_cannot_unlock_the_learned_dir() {
    let e = env("sgtable");
    let payloads = self_grant_payloads(&e);
    let entries = payloads
        .iter()
        .map(|(_, p)| allow_entry(&key(&e, p)))
        .collect();
    let t = table(&e, "sg.json", entries);
    let mut bad = vec![];
    for (label, p) in &payloads {
        let d = decide(&e, &t, p);
        if d == "allow" {
            bad.push(format!("{label}: Allow (shape {:?})", key(&e, p)));
        }
    }
    assert!(
        bad.is_empty(),
        "an allow entry for a self-grant shape must never be honoured (protected config):\n{}",
        bad.join("\n")
    );
}
