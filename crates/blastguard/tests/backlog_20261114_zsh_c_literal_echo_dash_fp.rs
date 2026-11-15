#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, p1, observed 2026-11-14): a harmless literal `-c`
//! program containing `echo - foo` (a lone `-` operand to echo) is refused:
//!   "`zsh -c '...'` takes its program from stdin, and that program is not on
//!   the command line — blastguard has no text to read, so it cannot tell
//!   whether anything here is irreversible"
//! although the program IS on the command line. Reproduced on this worktree's
//! build (0.2.540), main / subagent:
//!   zsh -c 'echo - foo'                       ask / deny   <- FP
//!   bash -c 'echo - foo'                      ask / deny   <- FP
//!   zsh -c 'echo - foo; echo -nE a; echo; echo -- x; echo -x y'   ask / deny <- FP
//!   bash -c 'echo - foo; echo -- x'           ask / deny   <- FP
//!   zsh -c 'echo hi'                          allow / allow
//!   zsh -c 'echo -- x'                        allow / allow
//!   zsh -c 'echo -nE a'                       allow / allow
//!   zsh -c 'echo -x y'                        allow / allow
//!   zsh -c 'chmod 777 /etc/hosts'             deny  / deny  (control)
//! so the trigger is the lone `-` operand, not `--`, `-nE`, or `-x`.
//! Suspected cause (not asserted): the -c payload is re-parsed and the bare
//! `-` is read as the stdin marker of a nested shell.
//!
//! Asserted (RED, `#[ignore]`d): the four FP shapes are Allow in both modes.
//! Controls (not ignored): the harmless shapes stay Allow, and the harmful
//! `zsh -c` program (system hosts file made world-writable) stays Deny.
//!
//! DOES NOT PROVE: `-` operands to other commands, `sh -c`, a real `bash -`
//! (shell reading stdin) which must stay refused, or nested -c.
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
        "bg-zshc-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "zsh-c-echo-dash-20261114",
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

const FP_SHAPES: &[&str] = &[
    "zsh -c 'echo - foo'",
    "bash -c 'echo - foo'",
    "zsh -c 'echo - foo; echo -nE a; echo; echo -- x; echo -x y'",
    "bash -c 'echo - foo; echo -- x'",
];

#[test]
#[ignore = "RED: 2026-11-14 shell -c literal program with `echo - foo` is refused as stdin-program — remove the ignore when fixed"]
fn shell_c_literal_echo_lone_dash_is_allow() {
    let home = unique_home("red");
    let mut bad = Vec::new();
    for shape in FP_SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(shape, &home, subagent);
            if d != "allow" {
                bad.push(format!("subagent={subagent} {shape:?} => {d} ({r})"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "expected Allow for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmless_variants_allow_and_harmful_c_program_denies() {
    let home = unique_home("ctl");
    for shape in [
        "zsh -c 'echo hi'",
        "zsh -c 'echo -- x'",
        "zsh -c 'echo -nE a'",
        "zsh -c 'echo -x y'",
    ] {
        for subagent in [false, true] {
            let (d, r) = run_as(shape, &home, subagent);
            assert_eq!(
                d, "allow",
                "control {shape:?} (subagent={subagent}): {d} {r}"
            );
        }
    }
    for subagent in [false, true] {
        let (d, r) = run_as("zsh -c 'chmod 777 /etc/hosts'", &home, subagent);
        assert_eq!(d, "deny", "harmful control (subagent={subagent}): {d} {r}");
    }
}
