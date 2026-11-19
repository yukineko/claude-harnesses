#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14, blastguard 0.2.581, MAIN mode):
//! `L=/etc/hosts; x=$(L=/tmp/x; rm -f $L)` is deny/deny with reason "this rm
//! deletes from a system directory: `$L)` ...". Same for
//! `L=/etc/hosts; L=/tmp/x y=$(rm -f $L)` (bash applies `L=/tmp/x` before
//! expanding y's value) and `f() { L=/tmp/x; }; L=/etc/hosts; x=$(f; rm -f $L)`
//! (f rebinds L inside the substitution subshell). In bash all three delete
//! /tmp/x, not /etc/hosts.
//!
//! Two readings: (1) real FP: the substitution body rebinds L before use, so the
//! outer binding L=/etc/hosts is not the value the rm sees; denying on it is a
//! wrong resolution; (2) intended: any `$L` that was ever bound to a system path
//! is conservatively denied regardless of later rebinding. Tell them apart: the
//! control without the rebind (`L=/etc/hosts; x=$(rm -f $L)`) must be deny
//! (system harm); if the rebind variants stay deny, the gate resolves L from the
//! outer scope ignoring in-body rebinding (reading 1). Ask would be acceptable
//! (value not literally known), Deny is not.
//!
//! Asserted: MAIN-mode verdict != "deny" for the three rebind variants; control
//! deny asserted un-ignored.
//!
//! DOES NOT PROVE: subagent mode, rebinding that still lands on a system path
//! (must stay Deny), other substitution forms (backticks, process substitution),
//! exact reason text.
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
        "bg-varmultisub-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "var-multi-statement-subst-20261114",
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

#[test]
fn rm_in_substitution_body_rebinding_var_is_not_denied() {
    let home = unique_home("red1");
    let cmd = "L=/etc/hosts; x=$(L=/tmp/x; rm -f $L)";
    let (d, r) = run_as(cmd, &home, false);
    assert_ne!(d, "deny", "must not deny; cmd={cmd:?} reason={r}");
}

#[test]
fn rm_in_substitution_with_prefix_assignment_rebind_is_not_denied() {
    let home = unique_home("red2");
    let cmd = "L=/etc/hosts; L=/tmp/x y=$(rm -f $L)";
    let (d, r) = run_as(cmd, &home, false);
    assert_ne!(d, "deny", "must not deny; cmd={cmd:?} reason={r}");
}

#[test]
fn rm_in_substitution_with_function_rebind_is_not_denied() {
    let home = unique_home("red3");
    let cmd = "f() { L=/tmp/x; }; L=/etc/hosts; x=$(f; rm -f $L)";
    let (d, r) = run_as(cmd, &home, false);
    assert_ne!(d, "deny", "must not deny; cmd={cmd:?} reason={r}");
}

#[test]
fn control_rm_in_substitution_without_rebind_is_deny() {
    let home = unique_home("ctl");
    let cmd = "L=/etc/hosts; x=$(rm -f $L)";
    let (d, r) = run_as(cmd, &home, false);
    assert_eq!(
        d, "deny",
        "control must stay deny (system harm); cmd={cmd:?} reason={r}"
    );
}

// ---- Added 2026-11-19 (run-20261119-072406-79324 t1) ------------------------
//
// Hypothesis under test: the segment's own `known_path_harm` pass reads the
// FIRST command of an opening substitution (`x=$(` split off by
// `split_substitution_opening_assignments`) and resolves its `$L` against the
// enclosing line with no rebind guard, while `known_path_harm_in_substitutions`
// (later statements of the same body) refuses a name the segment rebinds.
//
// DOES NOT PROVE: which internal pass produces the verdict (only the hook's
// final decision is observed), subagent mode, backtick / process
// substitution forms, the exact reason text.

/// RED: the flagless spelling of the first-statement rebind (`$L)` resolved
/// through the outer `L=/etc/hosts`). Not `rm -rf`: a recursive rm whose
/// operand the outer segment cannot place is a shape Deny ("recursive rm (-r)
/// can delete an entire directory tree") independent of any rebinding.
#[test]
fn rm_in_substitution_first_statement_after_rebind_is_not_denied() {
    let home = unique_home("red4");
    let cmd = "L=/etc/hosts; y=$(L=/tmp/x; rm $L)";
    let (d, r) = run_as(cmd, &home, false);
    assert_ne!(d, "deny", "must not deny; cmd={cmd:?} reason={r}");
}

/// Asymmetry witness: the same rebind when the `rm` is NOT the first
/// statement of the body (only the substitutions pass reaches it).
#[test]
fn rm_in_substitution_later_statement_after_rebind_is_not_denied() {
    let home = unique_home("wit");
    let cmd = "L=/etc/hosts; x=$(echo a; L=/tmp/x; rm -f $L)";
    let (d, r) = run_as(cmd, &home, false);
    assert_ne!(d, "deny", "must not deny; cmd={cmd:?} reason={r}");
}

/// Control: a PREFIX assignment does not change the expansion of its own
/// command's words — bash expands `$L` (inherited `/etc/hosts`) before it
/// applies `L=/tmp/x` to `rm`'s environment, so this removes /etc/hosts.
#[test]
fn control_prefix_assignment_on_rm_in_substitution_is_deny() {
    let home = unique_home("ctl2");
    let cmd = "L=/etc/hosts; x=$(L=/tmp/x rm -f $L)";
    let (d, r) = run_as(cmd, &home, false);
    assert_eq!(
        d, "deny",
        "control must stay deny (system harm); cmd={cmd:?} reason={r}"
    );
}

/// Control: a rebind that lands ON a system path stays a system-harm Deny.
#[test]
fn control_rebind_to_system_path_in_substitution_is_deny() {
    let home = unique_home("ctl3");
    let cmd = "L=/tmp/x; x=$(L=/etc/hosts; rm -f $L)";
    let (d, r) = run_as(cmd, &home, false);
    assert_eq!(
        d, "deny",
        "control must stay deny (system harm); cmd={cmd:?} reason={r}"
    );
}
