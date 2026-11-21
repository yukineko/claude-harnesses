//! RED (independent test author, 2026-11-22, observed at main 6ae161ee7 after
//! condukt run run-20261122-020202-63344): ba09708a made an `ACK_PAGER=` /
//! `ACK_PAGER_COLOR=` assignment PREFIX on the `ack` segment drop ack's
//! read-only exemption for a protected operand, but the same variable EXPORTED
//! in an earlier segment of the same command line still reaches ack (ack reads
//! `$ENV{ACK_PAGER}`), and that form is documented as not covered
//! (`crates/blastguard/src/detect/readonly.rs`, `env_selects_pager` docstring:
//! "NOT covered here ... `export ACK_PAGER=sh; ack …`").
//!
//! Passing means: `export ACK_PAGER=sh; ack pat .githooks/pre-commit` and the
//! `&&` spelling are NOT Allow, for both a main session and a subagent.
//!
//! DOES NOT PROVE: real ack behaviour (nothing is executed); inherited
//! environments from outside the command line; `.ackrc` / `ACKRC`; `declare -x`
//! / `typeset -x` / `set -a` spellings; which layer should own the fix.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
        "bg-ackpagerexport-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "ack-pager-exported-20261122",
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
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

fn expect(cmd: &str, tag: &str, want_allow: bool) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let d = decide(cmd, &home, subagent);
        let is_allow = d == "allow";
        if is_allow != want_allow || (!is_allow && d != "ask" && d != "deny") {
            bad.push(format!("subagent={subagent} => {d}"));
        }
    }
    assert!(
        bad.is_empty(),
        "expected {} for {cmd:?}:\n{}",
        if want_allow { "Allow" } else { "not Allow" },
        bad.join("\n")
    );
}

#[test]
#[ignore = "RED(a700901f): ACK_PAGER exported in an earlier segment keeps ack's read-only exemption"]
fn exported_ack_pager_then_semicolon_ack_protected_operand_is_not_allow() {
    let mut failures = Vec::new();
    for (cmd, tag) in [
        ("export ACK_PAGER=sh; ack pat .githooks/pre-commit", "semi"),
        (
            "export ACK_PAGER=sh && ack pat .githooks/pre-commit",
            "andand",
        ),
    ] {
        if let Err(e) = std::panic::catch_unwind(|| expect(cmd, tag, false)) {
            failures.push(
                e.downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| format!("{cmd:?}: panicked")),
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn control_plain_ack_protected_operand_stays_allow() {
    expect("ack pat .githooks/pre-commit", "plain", true);
}
