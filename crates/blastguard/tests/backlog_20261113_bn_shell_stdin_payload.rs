#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (040b959b scope, observed 2026-11-13): a shell program fed to
//! `bash`/`sh` on STDIN (here-string `<<<` or here-document) whose system-harm
//! command contains a backslash + LF ("BN") line continuation is Allowed.
//!
//! The shell that reads the program from stdin removes BN exactly as it does
//! for `bash -c '...'` (observed by the independent 040b959b verifier with a
//! harmless `ech\<LF>o` fed to /bin/bash and /bin/sh), so
//! `bash <<< 'chm\<LF>od 777 /etc/hosts'` runs `chmod 777 /etc/hosts`.
//! Found by the independent (sonnet) verifier of 040b959b at worktree b13b0834
//! and on main 706e25e1 (pre-existing); this test was written by the
//! flow-8cca7ef5 orchestrator, not by the 040b959b implementer.
//!
//! Asserted: every BN shape is DENY (user ruling: system harm always ends in
//! Deny), in main and subagent mode. Control (not ignored): the same programs
//! without BN are deny.
//!
//! DOES NOT PROVE: shapes not listed here; other stdin feeds (pipes, `<` from a
//! file); which internal stage applies the BN-removed reading.

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
        "bg-bnstdin-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "bn-shell-stdin-payload-20261113",
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

/// (label, command with BN, same command without BN).
const SHAPES: &[(&str, &str, &str)] = &[
    (
        "bash-herestring-sq",
        "bash <<< 'chm\\\nod 777 /etc/hosts'",
        "bash <<< 'chmod 777 /etc/hosts'",
    ),
    (
        "sh-herestring-sq",
        "sh <<< 'chm\\\nod 777 /etc/hosts'",
        "sh <<< 'chmod 777 /etc/hosts'",
    ),
    (
        "bash-heredoc-quoted",
        "bash <<'EOF'\nchm\\\nod 777 /etc/hosts\nEOF",
        "bash <<'EOF'\nchmod 777 /etc/hosts\nEOF",
    ),
    (
        "bash-heredoc-unquoted",
        "bash <<EOF\nchm\\\nod 777 /etc/hosts\nEOF",
        "bash <<EOF\nchmod 777 /etc/hosts\nEOF",
    ),
    (
        "bash-heredoc-unquoted-bn-in-path",
        "bash <<EOF\nchmod 777 /e\\\ntc/hosts\nEOF",
        "bash <<EOF\nchmod 777 /etc/hosts\nEOF",
    ),
];

// test-weakening-justified: new RED for an unfixed system-harm fail-open
// (040b959b scope, 2026-11-13); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 040b959b 2026-11-13 BN in a shell program fed to bash/sh on stdin is allowed — remove the ignore when fixed"]
fn bn_in_shell_program_on_stdin_is_denied() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for (label, bn, _) in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(bn, &home, subagent);
            println!("[{label}] subagent={subagent} {bn:?} -> {d} | {r}");
            if d != "deny" {
                failures.push(format!("[{label}] subagent={subagent} {bn:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) BN stdin-program runs were NOT Deny:\n{}",
        failures.len(),
        SHAPES.len() * 2,
        failures.join("\n")
    );
}

// Control: without BN every program is denied.
#[test]
fn control_same_programs_without_bn_are_denied() {
    let home = unique_home("ctl");
    for (label, _, plain) in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(plain, &home, subagent);
            assert_eq!(
                d, "deny",
                "[{label}] subagent={subagent} {plain:?} -> {d} | {r}"
            );
        }
    }
}
