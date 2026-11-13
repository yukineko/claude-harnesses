#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-13 false positive: the body of an arithmetic expansion `$(( … ))`
//! is analysed as a COMMAND LINE, so when its first operand is an expansion
//! (`$x`, `$(date +%s)`, a backtick substitution) that operand is reported as
//! an unresolvable command word.
//!
//! Measurement point: deployed blastguard 0.2.499 binary, main at 8dedad41,
//! 2026-11-13. Observed verdicts (identical in main and subagent mode):
//! - deny: `echo $(( $(date +%s) - 5 ))`, reason "the command word `$(date`
//!   is an expansion whose value only exists at run time — blastguard cannot
//!   tell what program this runs". Same reason for the originally observed
//!   `$(( $(date +%s) - $(cat /some/file) ))`, for `x=$(( $(date +%s) - 5 ))`,
//!   and with no spaces `$(($(date +%s)-5))`.
//! - deny: `echo $(( $x + 1 ))`, reason "the command word `$x` is an
//!   expansion ..."; same for `[ $(( $n + 1 )) -gt 3 ]`.
//! - deny: backtick operand `echo $(( `date +%s` - 5 ))`, reason "the command
//!   word ``date` is an expansion ...".
//! - allow: `echo $(( 1 + 2 ))`, `x=$(( 3 - 1 )); echo $x`,
//!   `echo $(( x + 1 ))` (first operand is not an expansion),
//!   `echo $(date +%s)`, `echo $(date +%s) - 5` (same substitution outside
//!   arithmetic).
//! - bash 5 evaluates `x=4; echo $(( $x + 1 ))` to `5` and
//!   `echo $(( $(echo 7) - 5 ))` to `2`: both are arithmetic, not a subshell.
//!
//! So `date` itself is NOT what is rejected (`echo $(date +%s)` is Allow): the
//! arithmetic body `$(date +%s) - 5` is split into words as if it were a
//! command, and its first word `$(date` is then judged as a command word.
//! The doc comment of `executed_substitution_payloads`
//! (src/detect/substitution.rs) states the intended behaviour: "`$((...))`
//! arithmetic is a `$(` too and is returned like the rest; its body is an
//! expression, which the analyser finds nothing destructive in" — the
//! observed deny contradicts that sentence.
//!
//! Two readings: (a) defect — an arithmetic body is an expression, its
//! operands are never executed as a command name; (b) intended — `$((` is
//! deliberately treated as possibly `$( (`, a command substitution holding a
//! subshell. bash only reads `$((` as `$( (` when the body does not parse as
//! arithmetic; the cases here all parse as arithmetic (bash prints numbers),
//! and the docstring above says (a). If (b) is the intent, the docstring is
//! the thing to fix and these RED cases should be deleted, not made green.
//!
//! RED: `r1`..`r3` expect Allow and fail at 8dedad41 (`cargo test -p
//! blastguard --test <this file> -- --ignored`, exit 101; each is Ask in this
//! fixture with the same "command word ... is an expansion" reason, Deny on
//! the deployed binary in the real checkout). Controls (pass now):
//! `c1` plain arithmetic is Allow; `c2` a destructive command substitution
//! INSIDE arithmetic (`$(( $(rm -rf /etc) + 1 ))`) stays NOT Allow, so a fix
//! that stops analysing substitutions nested in `$((…))` is caught; `c3` a real
//! subshell inside a command substitution whose command word is an expansion
//! (`$( ($x) )`) stays NOT Allow, so a fix that stops judging command words
//! after `$(` + `(` is caught.
//!
//! DOES NOT PROVE: `$[ … ]` legacy arithmetic, `(( … ))` arithmetic commands,
//! `let`, arithmetic inside `${var:offset}`, or that bash's `$((` vs `$( (`
//! disambiguation is modelled anywhere else in blastguard.

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
            .join("arithmetic_expansion_operand_as_command_word_fp_20261113")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        std::fs::write(proj.join("scratch/stamp"), "1\n").unwrap();
        Fx { base, home, proj }
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "arithmetic-expansion-operand-as-command-word-fp-20261113",
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
#[ignore = "RED 2026-11-13: arithmetic expansion body is parsed as a command line; its first operand is judged as a command word"]
fn r1_command_substitution_operand_in_arithmetic_is_allowed() {
    let f = Fx::new("r1");
    let cmd = "echo $(( $(date +%s) - $(cat scratch/stamp) ))";
    assert_eq!(
        f.bash(cmd),
        "allow",
        "arithmetic body is an expression: {cmd}"
    );
}

#[test]
#[ignore = "RED 2026-11-13: arithmetic expansion body is parsed as a command line; its first operand is judged as a command word"]
fn r2_variable_operand_in_arithmetic_is_allowed() {
    let f = Fx::new("r2");
    let cmd = "x=4; echo $(( $x + 1 ))";
    assert_eq!(
        f.bash(cmd),
        "allow",
        "arithmetic body is an expression: {cmd}"
    );
}

#[test]
#[ignore = "RED 2026-11-13: arithmetic expansion body is parsed as a command line; its first operand is judged as a command word"]
fn r3_backtick_operand_in_arithmetic_is_allowed() {
    let f = Fx::new("r3");
    let cmd = format!("echo $(( {BT}date +%s{BT} - 5 ))");
    assert_eq!(
        f.bash(&cmd),
        "allow",
        "arithmetic body is an expression: {cmd}"
    );
}

#[test]
fn c1_control_plain_arithmetic_is_allowed() {
    let f = Fx::new("c1");
    let cmd = "x=$(( 3 - 1 )); echo $(( x + 1 ))";
    assert_eq!(f.bash(cmd), "allow", "control must be Allow: {cmd}");
}

#[test]
fn c2_control_destructive_substitution_inside_arithmetic_stays_not_allowed() {
    let f = Fx::new("c2");
    let cmd = "echo $(( $(rm -rf /etc) + 1 ))";
    let v = f.bash(cmd);
    assert_ne!(
        v, "allow",
        "a real rm -rf /etc nested in arithmetic must not be Allow: {cmd}"
    );
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}

#[test]
fn c3_control_real_subshell_with_expansion_command_word_stays_not_allowed() {
    let f = Fx::new("c3");
    let cmd = "echo $( ($x) )";
    let v = f.bash(cmd);
    assert_ne!(
        v, "allow",
        "an expansion as a real command word must not be Allow: {cmd}"
    );
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}
