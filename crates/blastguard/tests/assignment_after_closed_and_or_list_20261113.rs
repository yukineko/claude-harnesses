#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-13): an and-or list CLOSED by `;` must not make a later
//! assignment look conditional.
//!
//! In `true && true; L=<p>; echo hi > $L` the `;` ends the and-or list
//! `true && true`, so bash runs `L=<p>` unconditionally. blastguard's
//! `assignment_execution_is_unconditional` (`src/detect/segment.rs`) answers
//! false whenever ANY earlier segment carries a `&&`/`||` separator, so the
//! redirect target `$L` stays unresolved and is refused as a shell expansion.
//!
//! Each RED case first measures the LITERAL spelling of the same command and
//! pins it, then requires the variable spelling to answer exactly the same.
//! The command-word case (r4) shows the change resolves toward REFUSAL as well
//! (Ask today, Deny like the literal-spelled `RM=/bin/rm; $RM -rf ...`).
//!
//! The controls pin shapes that must stay refused (NOT Allow) after the change:
//! the assignment itself inside an and-or list, pipes, `&`, compound commands
//! opened earlier, `{ ...; }` scoping, expansion-valued assignments.
//!
//! DOES NOT PROVE: newline-separated variants, here-document interactions,
//! `$(...)`/backtick-nested and-or lists, or any non-Bash tool path. Controls
//! only pin "not Allow", not the exact Ask/Deny split.

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
            .join("assignment_after_closed_and_or_list_20261113")
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
            "session_id": "assignment-after-closed-and-or-20261113",
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

/// Pin the literal control to `expected`, then require `variable` to match it.
#[track_caller]
fn same_as_literal(f: &Fx, literal: &str, expected: &str, variable: &str) {
    let lit = f.bash(literal);
    assert_eq!(
        lit, expected,
        "control (literal spelling) `{literal}` must be {expected}; got {lit}"
    );
    let var = f.bash(variable);
    assert_eq!(
        var, lit,
        "`{variable}` must answer as the literal `{literal}` ({lit}); got {var}"
    );
}

#[track_caller]
fn assert_not_allow(f: &Fx, command: &str) {
    let v = f.bash(command);
    assert_ne!(v, "allow", "`{command}` must NOT be Allow");
    assert_ne!(v, "other", "`{command}`: unrecognised hook output");
}

// ---------------------------------------------------------------- RED ----

#[test]
#[ignore = "RED: and-or list closed by ; still treated as guarding later assignments (2026-11-13)"]
fn r1_redirect_after_closed_and_list() {
    let f = Fx::new("r1");
    let p = f.out_log();
    same_as_literal(
        &f,
        &format!("true && true; echo hi > {p}"),
        "allow",
        &format!("true && true; L={p}; echo hi > $L"),
    );
}

#[test]
#[ignore = "RED: and-or list closed by ; still treated as guarding later assignments (2026-11-13)"]
fn r2_redirect_after_closed_or_list_and_append() {
    let f = Fx::new("r2");
    let p = f.out_log();
    same_as_literal(
        &f,
        &format!("false || true; echo hi > {p}"),
        "allow",
        &format!("false || true; L={p}; echo hi > $L"),
    );
    same_as_literal(
        &f,
        &format!("true && true; echo hi >> {p}"),
        "allow",
        &format!("true && true; L={p}; echo hi >> $L"),
    );
}

#[test]
#[ignore = "RED: and-or list closed by ; still treated as guarding later assignments (2026-11-13)"]
fn r3_redirect_after_closed_cd_and_list() {
    let f = Fx::new("r3");
    let p = f.out_log();
    let proj = f.proj.display().to_string();
    same_as_literal(
        &f,
        &format!("cd {proj} && true; echo hi > {p}"),
        "allow",
        &format!("cd {proj} && true; L={p}; echo hi > $L"),
    );
}

#[test]
#[ignore = "RED: and-or list closed by ; still treated as guarding later assignments (2026-11-13)"]
fn r4_command_word_after_closed_list_resolves_to_deny() {
    let f = Fx::new("r4");
    // Baseline without any and-or list: Deny today.
    let base = f.bash("RM=/bin/rm; $RM -rf /some/path");
    assert_eq!(base, "deny", "baseline `RM=/bin/rm; $RM -rf /some/path`");
    same_as_literal(
        &f,
        "true && true; /bin/rm -rf /some/path",
        "deny",
        "true && true; RM=/bin/rm; $RM -rf /some/path",
    );
}

#[test]
#[ignore = "RED: and-or list closed by ; still treated as guarding later assignments (2026-11-13)"]
fn r5_protected_target_through_variable_after_closed_list() {
    let f = Fx::new("r5");
    let literal = "true && true; echo x > .githooks/pre-commit";
    let lit = f.bash(literal);
    assert_ne!(lit, "allow", "control `{literal}` must NOT be Allow");
    assert_ne!(lit, "other", "control `{literal}`: unrecognised output");
    let variable = "true && true; L=.githooks/pre-commit; echo x > $L";
    let var = f.bash(variable);
    assert_eq!(
        var, lit,
        "`{variable}` must answer as the literal ({lit}); got {var}"
    );
}

// ----------------------------------------------------------- CONTROLS ----

#[test]
fn c1_assignment_inside_and_list_stays_refused() {
    let f = Fx::new("c1");
    let p = f.out_log();
    assert_not_allow(&f, &format!("true && L={p}; echo hi > $L"));
}

#[test]
fn c2_assignment_inside_or_list_after_plain_segment_stays_refused() {
    let f = Fx::new("c2");
    let p = f.out_log();
    assert_not_allow(&f, &format!("true; true || L={p}; echo hi > $L"));
}

#[test]
fn c3_assignment_in_pipeline_stays_refused() {
    let f = Fx::new("c3");
    let p = f.out_log();
    assert_not_allow(&f, &format!("true && true | L={p}; echo hi > $L"));
}

#[test]
fn c4_backgrounded_assignment_stays_refused() {
    let f = Fx::new("c4");
    let p = f.out_log();
    assert_not_allow(&f, &format!("true && true; L={p} & echo hi > $L"));
}

#[test]
fn c5_if_construct_opened_earlier_stays_refused() {
    let f = Fx::new("c5");
    let p = f.out_log();
    assert_not_allow(
        &f,
        &format!("if true && true; then :; fi; L={p}; echo hi > $L"),
    );
}

#[test]
fn c6_assignment_inside_brace_group_stays_refused() {
    let f = Fx::new("c6");
    let p = f.out_log();
    assert_not_allow(&f, &format!("{{ true && true; L={p}; }}; echo hi > $L"));
}

#[test]
fn c7_expansion_valued_assignment_stays_refused() {
    let f = Fx::new("c7");
    assert_not_allow(&f, "true && true; L=$(mktemp); echo hi > $L");
}

#[test]
fn c8_expansion_valued_command_word_stays_refused() {
    let f = Fx::new("c8");
    assert_not_allow(&f, "true && true; RM=$(which rm); $RM -rf /some/path");
}
