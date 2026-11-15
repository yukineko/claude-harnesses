#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 040b959b: a line continuation (backslash + LF, "BN") the shell
//! removes must not hide a system-harm command, and removing it must not turn
//! a harmless command into a refusal.
//!
//! `system_harm_with_bn_is_deny` pins the user's standing ruling that a
//! system-harm shape ends in Deny (never Ask) in the main session and in a
//! subagent: the shapes of
//! `tests/backlog_20261113_toplevel_line_continuation.rs` plus BN inside double
//! quotes, after `;`, inside `$(…)`, inside a double-quoted `sh -c` payload,
//! and in a shell program on stdin (a piped literal, a `bash -s` here-document).
//!
//! `harmless_bn_commands_keep_their_decision` pins the decision of harmless
//! commands holding a BN (including a BN inside single quotes, which the shell
//! keeps, one at the end of a comment, which continues nothing, harmless shell
//! programs on stdin, and a here-document DATA body written to a file) to the
//! decision observed for them BEFORE the fix (Allow).
//!
//! DOES NOT PROVE: that shapes not listed here are judged correctly; which
//! reading produced the Deny.

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
        "bg-bn-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "line-continuation-deny-20261113",
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

const HARM: &[&str] = &[
    "python3 -c \\\n\"open('/etc/hosts','w')\"",
    "pyth\\\non3 -c \"open('/etc/hosts','w')\"",
    "chm\\\nod 777 /etc/hosts",
    "ch\\\nmod 777 /etc/hosts",
    "chmod 777 /e\\\ntc/hosts",
    "chmod 777 \\\n/etc/hosts",
    "python3 -c o\\\npen\\(\\'/etc/hosts\\',\\'w\\'\\)",
    "bash -c 'chm\\\nod 777 /etc/hosts'",
    "env chm\\\nod 777 /etc/hosts",
    "timeout 5 chm\\\nod 777 /etc/hosts",
    // BN inside double quotes is removed too.
    "chmod 777 \"/e\\\ntc/hosts\"",
    "\"chm\\\nod\" 777 /etc/hosts",
    // After a separator, inside a substitution, in a double-quoted payload.
    "true; chm\\\nod 777 /etc/hosts",
    "echo $(chm\\\nod 777 /etc/hosts)",
    "sh -c \"chm\\\nod 777 /etc/hosts\"",
    // A shell program on stdin: piped literal, `-s` here-document.
    "echo 'chm\\\nod 777 /etc/hosts' | bash",
    "bash -s <<'EOF'\nchm\\\nod 777 /etc/hosts\nEOF",
];

#[test]
fn system_harm_with_bn_is_deny() {
    let home = unique_home("harm");
    let mut failures = Vec::new();
    for cmd in HARM {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            println!("subagent={subagent} {cmd:?} -> {d} | {r}");
            if d != "deny" {
                failures.push(format!("subagent={subagent} {cmd:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} system-harm BN runs were not Deny:\n{}",
        failures.len(),
        HARM.len() * 2,
        failures.join("\n")
    );
}

/// Harmless commands holding a BN, each Allow before the fix (observed with
/// this test's harness: the first eight at main 50474876, the stdin-program
/// and data here-document ones with the src of 817e3fb1).
const HARMLESS: &[&str] = &[
    "ls \\\n-la",
    "cargo test \\\n-p foo",
    "git status \\\n--short",
    "echo 'a\\\nb'",
    "echo \"a\\\nb\"",
    // Single quotes keep the pair: echo prints the text, nothing runs.
    "echo 'chm\\\nod 777 /etc/hosts'",
    // A comment's trailing backslash continues nothing.
    "echo a # note \\\nls",
    "printf '%s\\n' a \\\n  b \\\n  c",
    // Harmless shell programs on stdin.
    "bash <<< 'ls \\\n-la'",
    "bash <<'EOF'\nls \\\n-la\nEOF",
    "echo 'ls \\\n-la' | sh",
    // A here-document DATA body: written to a file, not run.
    "cat <<'EOF' > x.txt\nchm\\\nod 777 /etc/hosts\nEOF",
];

#[test]
fn harmless_bn_commands_keep_their_decision() {
    let home = unique_home("ok");
    let mut failures = Vec::new();
    for cmd in HARMLESS {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            println!("subagent={subagent} {cmd:?} -> {d} | {r}");
            if d != "allow" {
                failures.push(format!("subagent={subagent} {cmd:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} harmless BN run(s) changed from Allow:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
