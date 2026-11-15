#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-13 false positive: a line of a QUOTED here-doc body that starts with
//! a backtick is read as a command word, when a command follows the
//! terminator.
//!
//! Observed on the deployed 0.2.496 binary (main and subagent mode):
//! - deny: `git commit -q -F - <<'EOF'` / x / `` `detect` absent`` / `EOF` /
//!   `echo rc=$?`, with reason "the command word ``detect`` is an expansion
//!   whose value only exists at run time".
//! - allow: the same command with nothing after `EOF`.
//! - allow: the backtick mid-line ("add `detect` now") with a trailing
//!   `echo rc=$?`.
//!
//! The body of a quoted here-doc (`<<'EOF'`) is never expanded by bash: it is
//! data passed on stdin, not a command line. A subagent's commit was denied by
//! this, so its commit message had to be rewritten.
//!
//! RED: `r1`/`r2` expect Allow and fail at 9a66ea5e. Controls: `c1` (nothing
//! after the terminator) is Allow at HEAD; `c2` (a real top-level backtick
//! command word, outside any here-doc) must stay NOT Allow, so a fix that
//! simply stops looking at backticks is caught.
//!
//! DOES NOT PROVE: unquoted here-docs (where backticks DO expand), `<<-`
//! tab-stripped bodies, or non-git commands reading the here-doc.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::PathBuf;
use std::process::{Command, Stdio};

const BT: char = '`';

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("quoted_heredoc_body_backtick_fp_20261113")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        std::fs::create_dir_all(proj.join(".githooks")).unwrap();
        std::fs::write(proj.join(".githooks/pre-commit"), "#!/bin/sh\n").unwrap();
        Fx { base, home, proj }
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "quoted-heredoc-body-backtick-fp-20261113",
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
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
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = verdict(&s);
        println!("[{command}] => {v} {:?}", s.trim());
        v
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

#[test]
#[ignore = "RED 2026-11-13: quoted here-doc body line starting with a backtick is read as a command word"]
fn r1_line_starting_with_backtick_then_trailing_command_is_allowed() {
    let f = Fx::new("r1");
    let cmd = format!("git commit -q -F - <<'EOF'\nx\n{BT}detect{BT} absent\nEOF\necho rc=$?");
    assert_eq!(f.bash(&cmd), "allow", "quoted here-doc body is data: {cmd}");
}

#[test]
#[ignore = "RED 2026-11-13: quoted here-doc body line starting with a backtick is read as a command word"]
fn r2_observed_subagent_commit_message_shape_is_allowed() {
    let f = Fx::new("r2");
    let cmd = format!(
        "git commit -q -F - <<'EOF'\ntest(fde): RED repro tests\n\n(R0: \"host is not macOS\" evidence refusal; R1-R8: {BT}begin --poc{BT} /\n{BT}detect{BT} / {BT}spec{BT} absent).\n\nEOF\necho rc=$?; git log --oneline -1; git status --short"
    );
    assert_eq!(f.bash(&cmd), "allow", "quoted here-doc body is data: {cmd}");
}

#[test]
fn c1_control_nothing_after_terminator_is_allowed() {
    let f = Fx::new("c1");
    let cmd = format!("git commit -q -F - <<'EOF'\nx\n{BT}detect{BT} absent\nEOF");
    assert_eq!(f.bash(&cmd), "allow", "control must be Allow: {cmd}");
}

#[test]
fn c2_control_top_level_backtick_command_word_stays_not_allowed() {
    let f = Fx::new("c2");
    let cmd = format!("echo start\n{BT}detect{BT} absent\necho rc=$?");
    let v = f.bash(&cmd);
    assert_ne!(
        v, "allow",
        "a real backtick command word must not be Allow: {cmd}"
    );
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}
