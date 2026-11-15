#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED B2 (audit 2026-11-13, measured at 09dd0c8c on 2026-11-13).
//!
//! DEFECT: a SUBAGENT (payload carries a non-empty `agent_id`) running
//! `rm -rf proj/target` with cwd inside a Claude session scratchpad
//! (`/private/tmp/claude-502/<project>/<session-uuid>/scratchpad/sbx`) is
//! hardened from Ask to Deny ("recursive rm (-r) is confined to /private/tmp —
//! a safe root ... approve it if that is the tree you meant"), because a
//! subagent cannot answer an Ask. User ruling: deletion blocks only
//! config/asset destruction; scratch in managed places should be allowed.
//!
//! REPRODUCTION NOTE (measured at 09dd0c8c on 2026-11-13): the literal audit
//! shape `rm -rf proj/target` (bare relative) and the absolute spelling are
//! ALREADY Allow. What reproduces is the same existing directory spelled
//! `./proj/target` or `../sbx/proj/target`: Deny "recursive rm (-r) is confined
//! to <sbx> — a safe root for this session ..." (subagent hardening of Ask).
//! The REDs below therefore use those spellings.
//!
//! CONFLICT WITH DOCUMENTED BEHAVIOUR: these REDs conflict with the documented
//! `.`/`..` operand refusal in `crates/blastguard/src/deletion.rs` (module doc:
//! "cannot determine is never an Allow"): a `.`/`..`-spelled operand is
//! deliberately not placed lexically. Whether the ruling should override that is
//! a human decision; this file only records the observed verdict. A possible,
//! UNTESTED alternative cause of the original audit observation is a
//! non-canonical cwd (`/tmp/...` vs `/private/tmp/...`); it is not exercised.
//!
//! The RED asserts verdict != deny. Allow is what the ruling says; Ask is
//! tolerated here only because the defect filed is the hardening to Deny.
//!
//! DOES NOT PROVE: that the verdict is Allow (Ask passes), nor anything about
//! non-scratchpad temp dirs or operands outside the scratchpad. The non-ignored
//! witness pins that the payload really is hardened as a subagent (`git clean -fd`,
//! a command the B2 fix does not touch: Ask without `agent_id`, Deny with it); the system-harm control pins
//! `rm -rf /usr/lib` stays Deny. macOS only: the fixture lives under
//! `/private/tmp/claude-502`.
#![cfg(target_os = "macos")]

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CLAUDE_TMP: &str = "/private/tmp/claude-502";

struct Scratch {
    top: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    created_claude_tmp: bool,
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.top);
        let _ = std::fs::remove_dir_all(&self.home);
        if self.created_claude_tmp {
            // Fails (harmlessly) if another test/session still uses it.
            let _ = std::fs::remove_dir(CLAUDE_TMP);
        }
    }
}

fn scratch(tag: &str) -> Scratch {
    let created_claude_tmp = !Path::new(CLAUDE_TMP).exists();
    let uniq = format!("bgtest-b2-{tag}-{}", std::process::id());
    let top = Path::new(CLAUDE_TMP).join(&uniq);
    let cwd = top.join("3f2c1b0a-1111-4222-8333-444455556666/scratchpad/sbx");
    std::fs::create_dir_all(cwd.join("proj/target/debug")).unwrap();
    std::fs::write(cwd.join("proj/target/debug/x.o"), "x").unwrap();
    let home = std::env::temp_dir().join(format!("bg-b2-home-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    Scratch {
        top,
        home,
        cwd,
        created_claude_tmp,
    }
}

/// PreToolUse hook; returns (decision, reason). Empty stdout with exit 0 =
/// allow; a non-zero exit fails the test (never read as allow).
fn run_as(command: &str, s: &Scratch, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "b2-20261113",
        "cwd": s.cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = "test-subagent".into();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &s.home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("BLASTGUARD_APPROVALS_DIR", s.home.join("approvals"))
        .current_dir(&s.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

fn run(command: &str, s: &Scratch) -> (String, String) {
    run_as(command, s, true)
}

#[test]
fn b2_subagent_rm_rf_dot_slash_inside_scratchpad_is_not_denied() {
    let s = scratch("red");
    let (d, reason) = run("rm -rf ./proj/target", &s);
    assert_ne!(
        d, "deny",
        "subagent `rm -rf ./proj/target` inside the session scratchpad must not be Deny (ruling: scratch in managed places is allowed); reason: {reason}"
    );
}

#[test]
fn b2_subagent_rm_rf_dotdot_spelling_inside_scratchpad_is_not_denied() {
    let s = scratch("red2");
    // `../sbx/proj/target` names the same existing dir as `proj/target`.
    let (d, reason) = run("rm -rf ../sbx/proj/target", &s);
    assert_ne!(
        d, "deny",
        "subagent `rm -rf ../sbx/proj/target` (same dir as proj/target) must not be Deny; reason: {reason}"
    );
}

/// Witness (not ignored): the payload is hardened as a subagent. It uses a
/// command the B2 fix (`rm` operand spelling) does not touch: `git clean -fd`
/// in a git repo inside the scratchpad is a confined Ask for the main session
/// and a Deny with `agent_id`. Observed at HEAD 09dd0c8c: main=ask, sub=deny.
#[test]
fn b2_witness_subagent_hardening_ask_to_deny() {
    let s = scratch("witness");
    let g = Command::new("git")
        .args(["init", "-q"])
        .current_dir(s.cwd.join("proj"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(g.success(), "fixture: git init failed");
    std::fs::write(s.cwd.join("proj/untracked.txt"), "u").unwrap();
    let c = "git -C proj clean -fd";
    let (main_d, main_r) = run_as(c, &s, false);
    let (sub_d, sub_r) = run_as(c, &s, true);
    assert_eq!(main_d, "ask", "main session must be Ask; reason: {main_r}");
    assert_eq!(sub_d, "deny", "subagent must be Deny; reason: {sub_r}");
}

/// Control (not ignored): the bare relative spelling named in the audit is
/// already Allow at HEAD (the literal audit shape did NOT reproduce); pinned so
/// a regression to Deny is caught.
#[test]
fn b2_control_bare_relative_spelling_is_not_denied() {
    let s = scratch("ctl-bare");
    let (d, reason) = run("rm -rf proj/target", &s);
    assert_ne!(
        d, "deny",
        "bare `rm -rf proj/target` must not be Deny; reason: {reason}"
    );
}

/// Control (not ignored): system harm stays Deny in the same subagent payload.
#[test]
fn b2_control_subagent_rm_rf_usr_lib_stays_deny() {
    let s = scratch("ctl");
    let (d, reason) = run("rm -rf /usr/lib", &s);
    assert_eq!(d, "deny", "rm -rf /usr/lib must be Deny; reason: {reason}");
}
