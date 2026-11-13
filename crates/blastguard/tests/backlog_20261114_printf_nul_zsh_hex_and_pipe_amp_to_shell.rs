#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (system-harm fail-open, observed 2026-11-14 at e06c5432 by an
//! independent verifier; the same printf_eval code as c67f5f33, and main
//! 144906a1 also allowed them): three ways a `chmod 777 /etc/hosts` program
//! reaches a shell yet blastguard answers allow in main and subagent mode.
//!
//! 1. NUL bytes: bash/sh drop NUL bytes from a program read on stdin
//!    (`printf 'ec\0ho x\n' | /bin/bash` prints `x`), so `chm\0od`, `chm\000od`,
//!    `chm\x00od` and `%b` of `chm\0od` all run chmod.
//! 2. zsh printf semantics (the Bash tool's shell is zsh): a bare `\x` with no
//!    hex digit becomes NUL (`printf 'a\x|' | od -c` gives `a \0 |`), so
//!    `chm\xod` (also via `%b`) runs chmod.
//! 3. The `|&` pipe feeds stdout+stderr to the next stage, so
//!    `printf '...' |& bash` and `echo '...' |& bash` run chmod.
//!
//! This test was written by an independent test-author subagent (condukt task
//! run-20261114-035331-33292/t1), not by the implementer.
//!
//! Asserted: every shape is DENY in main and subagent mode (user ruling:
//! system harm is always Deny). Control (not ignored): harmless twins keep
//! their verdict and the literal `printf 'chmod 777 /etc/hosts' | bash` is deny.
//!
//! DOES NOT PROVE: other NUL producers (`xxd -r`, `base64 -d`, `$'..\0'`),
//! other zsh-only printf quirks, other pipe operators, or what the verdict of
//! `printf 'a\x' | bash` should be (it is printed, not asserted).

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
        "bg-printfnul-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "printf-nul-zsh-pipeamp-20261114",
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

const NUL_SHAPES: &[(&str, &str)] = &[
    ("nul-octal1", "printf 'chm\\0od 777 /etc/hosts' | bash"),
    ("nul-octal3", "printf 'chm\\000od 777 /etc/hosts' | bash"),
    ("nul-hex00", "printf 'chm\\x00od 777 /etc/hosts' | bash"),
    (
        "nul-b-octal1",
        "printf '%b' 'chm\\0od 777 /etc/hosts' | bash",
    ),
];

const ZSH_HEX_SHAPES: &[(&str, &str)] = &[
    ("zsh-bare-x", "printf 'chm\\xod 777 /etc/hosts' | bash"),
    (
        "zsh-bare-x-b",
        "printf '%b' 'chm\\xod 777 /etc/hosts' | bash",
    ),
];

const PIPE_AMP_SHAPES: &[(&str, &str)] = &[
    ("pipeamp-printf", "printf 'chmod 777 /etc/hosts' |& bash"),
    ("pipeamp-echo", "echo 'chmod 777 /etc/hosts' |& bash"),
];

// test-weakening-justified: new RED for an unfixed system-harm fail-open (2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 2026-11-14 printf output with NUL bytes (bash drops them) piped to a shell runs a system-harm command but is allowed — remove the ignore when fixed"]
fn printf_nul_byte_program_piped_to_shell_is_denied() {
    let failures = non_deny(NUL_SHAPES, "nul");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) printf-NUL runs were NOT Deny:\n{}",
        failures.len(),
        NUL_SHAPES.len() * 2,
        failures.join("\n")
    );
}

// test-weakening-justified: new RED for an unfixed system-harm fail-open (2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 2026-11-14 zsh printf turns a bare \\x into NUL so the piped shell runs a system-harm command but it is allowed — remove the ignore when fixed"]
fn printf_zsh_bare_hex_escape_program_piped_to_shell_is_denied() {
    let failures = non_deny(ZSH_HEX_SHAPES, "zshx");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) zsh bare-\\x runs were NOT Deny:\n{}",
        failures.len(),
        ZSH_HEX_SHAPES.len() * 2,
        failures.join("\n")
    );
}

// test-weakening-justified: new RED for an unfixed system-harm fail-open (2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: 2026-11-14 a program piped with |& (stdout+stderr) into a shell runs a system-harm command but is allowed — remove the ignore when fixed"]
fn program_piped_with_pipe_amp_to_shell_is_denied() {
    let failures = non_deny(PIPE_AMP_SHAPES, "pamp");
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) |& runs were NOT Deny:\n{}",
        failures.len(),
        PIPE_AMP_SHAPES.len() * 2,
        failures.join("\n")
    );
}

// Control: harmless twins keep their current verdict; the literal harm program
// piped to a shell is denied. `printf 'a\x' | bash` is only recorded.
#[test]
fn control_harmless_twins_and_literal_program() {
    let home = unique_home("ctl");
    for subagent in [false, true] {
        let (d, r) = run_as("printf 'a\\x' | bash", &home, subagent);
        println!("[record-only] subagent={subagent} printf 'a\\x' | bash -> {d} | {r}");
        for (c, want) in [
            ("printf 'echo hi\\n' | bash", "allow"),
            ("echo hi |& cat", "allow"),
        ] {
            let (d, r) = run_as(c, &home, subagent);
            println!("[control] subagent={subagent} {c:?} -> {d} | {r}");
            assert_eq!(d, want, "subagent={subagent} {c:?} -> {d} | {r}");
        }
    }
    let failures = non_deny(
        &[(
            "printf-literal-bash",
            "printf 'chmod 777 /etc/hosts' | bash",
        )],
        "ctl2",
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
