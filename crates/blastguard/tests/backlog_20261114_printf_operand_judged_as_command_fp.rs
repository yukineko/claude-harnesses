#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 at main a473ba71): `printf` whose
//! ARGUMENT text merely mentions a destructive-looking command line is refused
//! with "`printf` is not a command blastguard recognises, and what follows it
//! parses as a destructive command line — blastguard cannot tell whether
//! `printf` runs it". `printf` is a shell builtin that prints its operands;
//! it never executes them.
//!
//! Live hit: the flow-8cca7ef5 orchestrator appending a `[[spec]]` binding to
//! .specguard/spec-docs.toml with `printf '...' "...find -exec sh -c..." >> file`
//! was denied in a subagent.
//!
//! Asserted: the printf-append whose double-quoted operand also contains the
//! escaped text `\$PY -c, \\\"\$PY\\\" -c, ` gets the same main-session
//! decision as its twin without that text (both mention `find -exec sh -c` and
//! `os.chmod on /etc/hosts`). Control (not ignored): the twin is Allow.
//! Bisect at a473ba71: the twin alone, `printf '%s\n' "find -exec sh -c ..."`
//! and `printf '%s\n' "\$PY -c, \\\"\$PY\\\" -c stays"` are each Allow; only the
//! combination is Ask.
//!
//! RED written by the flow-8cca7ef5 orchestrator.
//!
//! DOES NOT PROVE: which operand words trigger it; other print builtins.

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
        "bg-pfp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "printf-operand-fp-20261114",
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

/// The operand only differs from HARMLESS by `\$PY -c, \\\"\$PY\\\" -c, `
/// (shell text that prints `$PY -c, \"$PY\" -c, `).
const MENTIONS: &str = r##"printf '\n[[spec]]\nreason = "%s"\n' "comment-bearing \$PY -c, \\\"\$PY\\\" -c, find -exec sh -c and ANSI-C programs calling os.chmod on /etc/hosts stay not-Allow." >> notes.toml"##;
const HARMLESS: &str = r##"printf '\n[[spec]]\nreason = "%s"\n' "comment-bearing programs, find -exec sh -c and ANSI-C programs calling os.chmod on /etc/hosts stay not-Allow." >> notes.toml"##;

#[test]
#[ignore = "RED: printf operand text mentioning a command line is judged as executed (backlog p1 FP)"]
fn printf_operand_text_is_not_judged_as_a_command() {
    let home = unique_home("fp");
    let (d_m, r_m) = run_as(MENTIONS, &home, false);
    let (d_h, r_h) = run_as(HARMLESS, &home, false);
    assert_eq!(
        d_m, d_h,
        "printf mentioning a command got {d_m} ({r_m}); harmless twin got {d_h} ({r_h})"
    );
}

#[test]
fn control_harmless_printf_append_is_allowed() {
    let home = unique_home("ctl");
    let (d, r) = run_as(HARMLESS, &home, false);
    assert_eq!(d, "allow", "control: {d} {r}");
}
