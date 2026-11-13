#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-13: an assignment inside a `{ ...; }` brace group that is the right
//! operand of `&&`/`||` stays CONDITIONAL even though a `;` appears before it.
//!
//! Inside braces the `;` does not end the enclosing and-or list. Measured in
//! real bash on 2026-11-13:
//! `bash -c 'false && { true; RM=/bin/echo; }; echo "brace=[$RM]"'` printed
//! `brace=[]`; with `true &&` it printed `brace-true=[/bin/echo]`. Whether the
//! assignment runs is a run-time fact (the exit status), so blastguard must not
//! resolve the variable statically: every case here must be NOT Allow.
//!
//! Each test first pins a sensitivity control — the same assignment with no
//! and-or list / brace group — to Allow, proving the resolver WOULD resolve the
//! variable otherwise (so "not Allow" is not just "the expansion was never
//! understood").
//!
//! DOES NOT PROVE: the exact Ask/Deny split (only "not Allow" is pinned, with
//! the observed verdict printed), nested brace groups, subshells `( ... )`,
//! `$(...)`-nested cases, or any non-Bash tool path.

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

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("brace_group_guard_20261113")
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

    fn out_log(&self) -> String {
        self.proj.join("scratch/out.log").display().to_string()
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "brace-group-guard-20261113",
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

#[track_caller]
fn assert_control_allow(f: &Fx, command: &str) {
    let v = f.bash(command);
    assert_eq!(
        v, "allow",
        "sensitivity control `{command}` must be Allow (resolver resolves it); got {v}"
    );
}

#[track_caller]
fn assert_not_allow(f: &Fx, command: &str) -> &'static str {
    let v = f.bash(command);
    assert_ne!(v, "allow", "`{command}` must NOT be Allow");
    assert_ne!(v, "other", "`{command}`: unrecognised hook output");
    v
}

#[test]
fn b1_false_and_brace_group_assignment_redirect() {
    let f = Fx::new("b1");
    let p = f.out_log();
    assert_control_allow(&f, &format!("L={p}; echo hi > $L"));
    assert_not_allow(&f, &format!("false && {{ true; L={p}; }}; echo hi > $L"));
}

#[test]
fn b2_true_and_brace_group_assignment_redirect() {
    let f = Fx::new("b2");
    let p = f.out_log();
    assert_control_allow(&f, &format!("L={p}; echo hi > $L"));
    assert_not_allow(&f, &format!("true && {{ true; L={p}; }}; echo hi > $L"));
}

#[test]
fn b3_command_word_from_brace_group_assignment() {
    let f = Fx::new("b3");
    assert_control_allow(&f, "RM=/bin/echo; $RM -rf /some/path");
    let v = assert_not_allow(&f, "false && { true; RM=/bin/echo; }; $RM -rf /some/path");
    println!("b3 observed verdict: {v} (expected ask)");
}

#[test]
fn b4_false_or_brace_group_assignment_redirect() {
    let f = Fx::new("b4");
    let p = f.out_log();
    assert_control_allow(&f, &format!("L={p}; echo hi > $L"));
    assert_not_allow(&f, &format!("false || {{ true; L={p}; }}; echo hi > $L"));
}

#[test]
fn b5_newline_formatted_brace_group() {
    let f = Fx::new("b5");
    let p = f.out_log();
    assert_control_allow(&f, &format!("L={p}\necho hi > $L"));
    assert_not_allow(&f, &format!("false && {{\nL={p}\n}}\necho hi > $L"));
}
