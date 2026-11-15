#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 at rev 5dbcdb444 in the worktree
//! of task b3569da5; base = b0b4ac6e3 src): the "compound closed but the raw
//! count of compound-keyword words in range exceeds the opener count -> Ask"
//! insurance in `crates/blastguard/src/detect/payload/compound_readers.rs`
//! fires on opener words (`if`/`while`/`until`/`for`/`select`/`case`) that sit
//! in ARGUMENT position (e.g. `echo if`), which open nothing.
//!
//! Observed with the built hook binaries (main / subagent):
//!   shape                                                       base         5dbcdb444
//!   ls | while read f; do echo "$f" | grep -q if && echo y; done  allow/allow  ask/deny
//!   ls | until false; do echo until; break; done                allow/allow  ask/deny
//!   ls | while read f; do echo while; done                      allow/allow  ask/deny
//!   ls | while read f; do echo until; done                      allow/allow  ask/deny
//!   ls | while read f; do echo for; done                        allow/allow  ask/deny
//!   ls | while read f; do echo select; done                     allow/allow  ask/deny
//!   ls | case x in x) echo case;; esac                          allow/allow  ask/deny
//!   ls | while read f; do echo if; done; ls                     allow/allow  ask/deny
//!   ls | while read f; do echo if if if; done                   allow/allow  ask/deny
//!   echo 'ls' | if true; then echo if; fi; bash                 allow/allow  ask/deny
//!     (the trailing `bash` runs after `fi`, so it does not read the pipe)
//! Controls observed at 5dbcdb444:
//!   echo 'ls' | { if true; then echo; fi; bash; }   allow/allow (FP from
//!     16b45552f, fixed by 5dbcdb444 — regression guard)
//!   git ls-files | while read f; do echo done; done allow/allow
//!   ls | while read f; do echo 'if'; done           allow/allow
//!   HARM | while read f; do echo if; bash; done             deny/deny
//!   HARM | if true; then echo while; bash; fi               deny/deny
//!   HARM | if true; then ! if true; then echo; fi; bash; fi deny/deny
//!   (HARM = a program that makes /etc/hosts world-writable; base was
//!   allow/allow on these three, i.e. they are the fail-open side the
//!   insurance must keep closed.)
//!
//! Asserted:
//!   - RED (not ignored): every benign shape above is Allow in main AND
//!     subagent mode.
//!   - Control (not ignored): the three benign controls stay Allow; the three
//!     HARM shapes stay Deny in both modes (an argument-position opener must
//!     not blind the gate to a later in-compound `bash` that reads the pipe).
//!
//! DOES NOT PROVE: double-quoted opener words (`echo "if"`), opener words
//! inside `$(...)`/backticks, heredoc bodies, `[[ ... ]]`/`function` keywords,
//! other harmful programs, other readers (`sh`, `zsh`, `eval`), or that the
//! fix counts openers by real grammar position rather than by a narrower
//! heuristic that happens to cover these exact strings.
//!
//! RED written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-kwarg-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pipe-compound-kwarg-fp-20261114",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

const HARM: &str = "echo 'chmod 777 /etc/hosts' | ";

/// Benign: opener words only in argument position. Ask/deny at 5dbcdb444.
const FP_SHAPES: &[&str] = &[
    r#"ls | while read f; do echo "$f" | grep -q if && echo y; done"#,
    "ls | until false; do echo until; break; done",
    "ls | while read f; do echo while; done",
    "ls | while read f; do echo until; done",
    "ls | while read f; do echo for; done",
    "ls | while read f; do echo select; done",
    "ls | case x in x) echo case;; esac",
    "ls | while read f; do echo if; done; ls",
    "ls | while read f; do echo if if if; done",
    "echo 'ls' | if true; then echo if; fi; bash",
];

/// Benign controls already Allow at 5dbcdb444.
const ALLOW_CONTROLS: &[&str] = &[
    "echo 'ls' | { if true; then echo; fi; bash; }",
    "git ls-files | while read f; do echo done; done",
    "ls | while read f; do echo 'if'; done",
];

/// Suffixes appended to HARM: the in-compound `bash` reads the pipe.
const DENY_CONTROLS: &[&str] = &[
    "while read f; do echo if; bash; done",
    "if true; then echo while; bash; fi",
    "if true; then ! if true; then echo; fi; bash; fi",
];

fn mismatches(cmds: &[String], want: &str, tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != want {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

fn owned(shapes: &[&str]) -> Vec<String> {
    shapes.iter().map(|s| s.to_string()).collect()
}

#[test]
fn opener_word_in_argument_position_is_not_a_compound_opener() {
    let bad = mismatches(&owned(FP_SHAPES), "allow", "red");
    assert!(
        bad.is_empty(),
        "expected Allow for every benign shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_benign_compound_readers_stay_allow() {
    let bad = mismatches(&owned(ALLOW_CONTROLS), "allow", "ctl-allow");
    assert!(
        bad.is_empty(),
        "control shapes must stay Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harm_into_compound_with_argument_opener_is_deny() {
    let cmds: Vec<String> = DENY_CONTROLS.iter().map(|s| format!("{HARM}{s}")).collect();
    let bad = mismatches(&cmds, "deny", "ctl-deny");
    assert!(
        bad.is_empty(),
        "harmful shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}
