#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-13 FAIL-OPEN: a write or delete inside a shell FUNCTION BODY is
//! judged with the variable values at DEFINITION time, but bash expands the
//! body when the function is CALLED.
//!
//! Observed on the deployed 0.2.496 binary:
//! - allow: `L=<safe>; f() { echo x > $L; }; L=/etc/hosts; f` (also the
//!   multi-line spelling). `bash -c` on throwaway temp paths confirms the
//!   redirect writes the value assigned AFTER the definition.
//! - allow: `f() { rm -f $L; }; L=/etc/hosts; f`, and even
//!   `f() { rm -f $L; }; f` — while a bare `rm -f $L` is denied ("this rm
//!   operand `$L` cannot be resolved to a path").
//! - deny (correct): `L=/etc/hosts; echo x > $L`; `L=/etc/hosts; rm -f $L`;
//!   `f() { echo x > /etc/hosts; }; f`.
//!
//! The suspected opposite direction — an ASSIGNMENT inside the body
//! (`L=<safe>; f() { L=/etc/hosts; }; f; echo x > $L`) — did NOT fail open:
//! it is denied (single-line: `$L` unresolved; multi-line: system write).
//!
//! RED: `r1`..`r4` expect NOT Allow and fail at 1a519d22. Controls: `c1`
//! (`L=<safe>; echo x > $L`) is Allow, so the fixture's safe path really is
//! allowed and the RED is about the late binding, not the path; `c2`/`c3`
//! show the same write/delete outside a function body is refused.
//!
//! DOES NOT PROVE: `function f { ...; }` spelling, subshell bodies
//! (`f() ( ... )`), or functions called through `eval` / `command`.

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
            .join("function_body_late_binding_fail_open_20261113")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        Fx { base, home, proj }
    }

    fn safe(&self) -> String {
        self.proj.join("scratch/out.txt").display().to_string()
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "function-body-late-binding-fail-open-20261113",
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

    fn assert_not_allowed(&self, cmd: &str, why: &str) {
        let v = self.bash(cmd);
        assert_ne!(v, "allow", "{why}: {cmd}");
        assert_ne!(v, "other", "unrecognised hook output: {cmd}");
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
#[ignore = "RED 2026-11-13: function-body redirect judged with definition-time $L; bash writes /etc/hosts"]
fn r1_redirect_in_body_after_reassignment_is_not_allowed() {
    let f = Fx::new("r1");
    let cmd = format!("L={}; f() {{ echo x > $L; }}; L=/etc/hosts; f", f.safe());
    f.assert_not_allowed(&cmd, "bash writes /etc/hosts when f runs");
}

#[test]
#[ignore = "RED 2026-11-13: function-body redirect judged with definition-time $L; bash writes /etc/hosts"]
fn r2_multiline_redirect_in_body_after_reassignment_is_not_allowed() {
    let f = Fx::new("r2");
    let cmd = format!("L={}\nf() {{\necho x > $L\n}}\nL=/etc/hosts\nf", f.safe());
    f.assert_not_allowed(&cmd, "bash writes /etc/hosts when f runs");
}

#[test]
#[ignore = "RED 2026-11-13: rm with a variable operand inside a function body is allowed"]
fn r3_rm_in_body_after_system_assignment_is_not_allowed() {
    let f = Fx::new("r3");
    f.assert_not_allowed(
        "f() { rm -f $L; }; L=/etc/hosts; f",
        "bash deletes /etc/hosts when f runs",
    );
}

#[test]
#[ignore = "RED 2026-11-13: rm with a variable operand inside a function body is allowed"]
fn r4_rm_unresolved_operand_in_called_body_is_not_allowed() {
    let f = Fx::new("r4");
    f.assert_not_allowed(
        "f() { rm -f $L; }; f",
        "the operand is as unresolvable as in a bare `rm -f $L`",
    );
}

#[test]
fn c1_control_safe_redirect_outside_function_is_allowed() {
    let f = Fx::new("c1");
    let cmd = format!("L={}; echo x > $L", f.safe());
    assert_eq!(f.bash(&cmd), "allow", "control must be Allow: {cmd}");
}

#[test]
fn c2_control_system_redirect_outside_function_stays_not_allowed() {
    let f = Fx::new("c2");
    let cmd = format!("L={}; L=/etc/hosts; echo x > $L", f.safe());
    f.assert_not_allowed(&cmd, "a write to /etc/hosts must not be Allow");
}

#[test]
fn c3_control_unresolved_rm_outside_function_stays_not_allowed() {
    let f = Fx::new("c3");
    f.assert_not_allowed("rm -f $L", "an unresolvable rm operand must not be Allow");
}
