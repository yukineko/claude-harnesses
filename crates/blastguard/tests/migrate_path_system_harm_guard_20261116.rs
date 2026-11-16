#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! System-harm guard for the `migrate-path` commit (run-20261116-020031-33040):
//! blastguard's path rules (truncating/append redirect, rm target, protected
//! relative-after-cd, system-file-mutation-unresolved for sed -i / chmod / cp /
//! mv) now read the AST static evaluator (`detect/ast/targets.rs`) to resolve
//! `$VAR` / `for` / `cd` targets. These tests drive the built binary and pin
//! that targets which resolve to SYSTEM paths stay Deny in BOTH modes
//! (main = no `agent_id`, subagent = `agent_id` set), that undetermined targets
//! are never Allow, and the one accepted change (user ruling): a resolved
//! `<proj>/.claude/settings.json` redirect is main Ask / subagent Deny.
//!
//! DOES NOT PROVE: these tests were written AFTER the implementation by an
//! independent author; RED against the pre-migration base was NOT observed, so
//! they do not show that the old string reading would have failed them (nor
//! that the migration is what keeps them Deny). They cover only the literal
//! command spellings listed, not other wrappers / shapes, and not reason texts.
//! The hook reports one reason, so another rule may be the one producing a
//! Deny.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("migrate_path_system_harm_guard_20261116")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join(".claude")).unwrap();
        std::fs::create_dir_all(proj.join("target/debug")).unwrap();
        Fx { base, home, proj }
    }

    fn run(&self, command: &str, subagent: bool) -> (&'static str, String) {
        let command = command.replace("@PROJ@", &self.proj.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": "migrate-path-system-harm-guard-20261116",
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("test-subagent");
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(
            &mut cmd,
            payload.to_string().as_bytes(),
            bounded::HOOK_DEADLINE,
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = verdict(&s);
        let reason = if v == "allow" {
            String::new()
        } else {
            let doc: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
            doc["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };
        (v, reason)
    }
}

fn verdict(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "other"
    }
}

fn pair(tag: &str, command: &str) -> ((&'static str, String), (&'static str, String)) {
    let fx = Fx::new(tag);
    let m = fx.run(command, false);
    let s = fx.run(command, true);
    println!(
        "[{tag}] cmd={command:?}\n  OBSERVED (main, subagent) = ({}, {})\n  main reason={:?}\n  sub reason={:?}",
        m.0, s.0, m.1, s.1
    );
    (m, s)
}

/// Must be Deny in BOTH modes.
fn deny_both(tag: &str, command: &str) {
    let (m, s) = pair(tag, command);
    assert_eq!(
        (m.0, s.0),
        ("deny", "deny"),
        "{tag}: system harm must be Deny/Deny; cmd={command:?}\n main reason={}\n sub reason={}",
        m.1,
        s.1
    );
}

/// Must be a block (Deny or Ask), never Allow, in BOTH modes.
fn not_allow_both(tag: &str, command: &str) {
    let (m, s) = pair(tag, command);
    assert!(
        matches!(m.0, "deny" | "ask") && matches!(s.0, "deny" | "ask"),
        "{tag}: must not be Allow; observed ({}, {}); cmd={command:?}",
        m.0,
        s.0
    );
}

#[test]
fn redirect_truncate_var_etc() {
    deny_both("trunc", "S=/etc && echo x > $S/hosts");
}

#[test]
fn redirect_append_var_etc() {
    deny_both("append", "S=/etc && echo x >> $S/hosts");
}

#[test]
fn rm_rf_var_etc() {
    deny_both("rm_var", "S=/etc && rm -rf $S");
}

#[test]
fn rm_rf_for_loop_etc_usr() {
    deny_both("rm_for", "for d in /etc /usr; do rm -rf $d; done");
}

#[test]
fn cd_etc_then_relative_redirect() {
    deny_both("cd_rel", "cd /etc && echo x > hosts");
}

#[test]
fn sed_i_var_etc_hosts() {
    deny_both("sed_i", "F=/etc/hosts; sed -i s/a/b/ $F");
}

#[test]
fn chmod_var_etc_hosts() {
    deny_both("chmod", "F=/etc/hosts; chmod 777 $F");
}

#[test]
fn cp_into_var_usr_local_bin() {
    deny_both("cp", "D=/usr/local/bin; cp x $D/x");
}

#[test]
fn mv_into_var_usr_local_bin() {
    deny_both("mv", "D=/usr/local/bin; mv x $D/x");
}

#[test]
fn rm_rf_var_tilde() {
    deny_both("rm_tilde", "D=~; rm -rf $D");
}

#[test]
fn rm_rf_var_set_in_both_if_branches_includes_etc() {
    deny_both(
        "rm_if",
        "if true; then S=/etc; else S=/tmp/x; fi; rm -rf $S",
    );
}

#[test]
fn rm_rf_unknown_var_is_not_allow() {
    not_allow_both("rm_unknown", "rm -rf $UNKNOWN");
}

#[test]
fn redirect_to_command_substitution_is_not_allow() {
    not_allow_both("redir_cmdsub", "echo x > $(cmd)");
}

/// After `.` (source) the analysis is Undetermined (user ruling, reading A), so
/// the old reading applies; only "not Allow" is asserted.
#[test]
fn redirect_after_source_is_not_allow() {
    not_allow_both("after_source", ". ./env.sh; S=/etc; echo x > $S/hosts");
}

/// Accepted change (user ruling): resolved project settings.json redirect is
/// main Ask / subagent Deny.
#[test]
fn project_settings_json_redirect_main_ask_subagent_deny() {
    let (m, s) = pair("settings", "S=@PROJ@/.claude/settings.json && echo x > $S");
    assert_eq!(
        (m.0, s.0),
        ("ask", "deny"),
        "settings.json redirect must be (ask, deny)\n main reason={}\n sub reason={}",
        m.1,
        s.1
    );
}
