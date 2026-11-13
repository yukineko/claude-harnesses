#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, observed 2026-11-13 at main 5bca9481): a shell
//! program fed to `sh`/`bash` whose text is produced by `printf` escape
//! expansion, or fed through a process substitution `bash < <(echo ...)`, is
//! Allowed even though it runs `chmod 777 /etc/hosts`.
//!
//! - `printf 'chm\x6fd 777 /etc/hosts' | bash`: printf expands `\x6f` to `o`
//!   (also octal `\157`), so bash reads `chmod 777 /etc/hosts`.
//! - `bash < <(echo 'chmod 777 /etc/hosts')`: no escape at all; bash reads the
//!   program from the process substitution on stdin.
//!
//! Found as an out-of-scope finding by the independent round-2 verifier of
//! 040b959b (observed Allow both at the 040b959b worktree and on main
//! 706e25e1). This test was written by the flow-8cca7ef5 orchestrator, not by
//! any implementer.
//!
//! Asserted: every shape is DENY (user ruling: system harm always ends in
//! Deny), main and subagent mode. Control (not ignored): the literal
//! `printf 'chmod 777 /etc/hosts' | bash` is denied.
//!
//! DOES NOT PROVE: other printf escapes (`%b`, `\u`), other producers
//! (`xxd -r`, `base64 -d`), or that other process-substitution shapes are safe.

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
        "bg-printfps-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "printf-procsub-20261113",
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

fn non_deny(shapes: &[(&str, &str)], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut failures = Vec::new();
    for (label, c) in shapes {
        for subagent in [false, true] {
            let (d, r) = run_as(c, &home, subagent);
            println!("[{label}] subagent={subagent} {c:?} -> {d} | {r}");
            if d != "deny" {
                failures.push(format!("[{label}] subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    failures
}

const PRINTF_SHAPES: &[(&str, &str)] = &[
    (
        "printf-hex-bash",
        "printf 'chm\\x6fd 777 /etc/hosts' | bash",
    ),
    ("printf-octal-sh", "printf 'chm\\157d 777 /etc/hosts' | sh"),
];

const PROCSUB_SHAPES: &[(&str, &str)] = &[
    ("procsub-echo-bash", "bash < <(echo 'chmod 777 /etc/hosts')"),
    ("procsub-printf-sh", "sh < <(printf 'chmod 777 /etc/hosts')"),
];

// test-weakening-justified: new RED for an unfixed system-harm fail-open
// (2026-11-13); the ignore is removed in the fixing commit.
#[test]
fn printf_escape_expanded_program_piped_to_shell_is_denied() {
    let failures = non_deny(PRINTF_SHAPES, "printf");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) printf-escape runs were NOT Deny:\n{}",
        failures.len(),
        PRINTF_SHAPES.len() * 2,
        failures.join("\n")
    );
}

// test-weakening-justified: new RED for an unfixed system-harm fail-open
// (2026-11-13); the ignore is removed in the fixing commit.
#[test]
fn shell_program_from_process_substitution_is_denied() {
    let failures = non_deny(PROCSUB_SHAPES, "procsub");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) process-substitution runs were NOT Deny:\n{}",
        failures.len(),
        PROCSUB_SHAPES.len() * 2,
        failures.join("\n")
    );
}

// Control: the literal program piped to a shell is denied.
#[test]
fn control_literal_printf_piped_to_shell_is_denied() {
    let failures = non_deny(
        &[(
            "printf-literal-bash",
            "printf 'chmod 777 /etc/hosts' | bash",
        )],
        "ctl",
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
