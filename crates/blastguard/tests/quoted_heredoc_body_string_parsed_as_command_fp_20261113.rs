#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-13 false positive: a line of a QUOTED here-doc body is parsed as a
//! command line when the here-doc command is NOT the first command of the
//! line (`a && cat >> f <<'X'`, `a; cat >> f <<'X'`).
//!
//! Observed (a subagent's probe-script append was denied, deployed 0.2.496):
//! `cd <dir> && sed -e '...' probe_b2.py > probe_c.py && cat >> probe_c.py
//! <<'X'` / a Python body containing the line
//! `            "L=/etc; cp README.md $L/hosts", "L=/etc/hosts; mv README.md $L", "mv README.md /etc/hosts",`
//! / `X` / `python3 -I probe_c.py` -> deny, reason "`\"L=/etc;` is not a
//! command blastguard recognises, and what follows it parses as a destructive
//! command line — blastguard cannot tell whether `\"L=/etc;` runs it".
//!
//! Minimised on the same binary:
//! - deny: `true && cat >> f.py <<'X'` / `"cp README.md /etc/hosts",` / `X`
//!   (nothing after `X`), reason "'cp destination /etc/hosts\",' writes into a
//!   system directory ...". Same with `true; true; cat >> f.py`, with `cat`
//!   and no redirect, and with `git commit -q -F -` after `true &&`.
//! - allow: the same here-doc with no command before it; a body line that
//!   does not START with the destructive words (`x = ["cp ... /etc/hosts"]`);
//!   a benign line (`"cp README.md notes.txt",`).
//! - The `;` inside the string is NOT needed; a destructive literal is (the
//!   observed `"L=/etc; cp ... $L/hosts"` alone was allowed — it only tripped
//!   because the same line also held `"mv README.md /etc/hosts"`).
//!
//! Relation to backlog 53a97aa3 (RED febfee9c, backtick at line start): same
//! family — a quoted here-doc body line read as a command line — but that
//! entry's trigger is a command AFTER the terminator. Here the trigger is a
//! command BEFORE the here-doc command, with nothing after the terminator;
//! the backtick line of 53a97aa3 is also denied in this position. `r3` pins
//! the trailing-command variant with a string (not backtick) line.
//!
//! RED: `r1`..`r3` expect Allow and fail at 8002d4c5 (`r2` is Ask in this
//! fixture, Deny in the real checkout). Controls: `c1` (same shape,
//! benign line) is Allow; `c2` (the same cp as a real command after `true &&`)
//! and `c3` (the line unquoted at top level) stay NOT Allow, so a fix that
//! stops parsing command lines after `&&` is caught.
//!
//! DOES NOT PROVE: unquoted here-docs (where the body is still data for
//! `cat`, but expansions run), `<<-`, or here-strings.

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
            .join("quoted_heredoc_body_string_parsed_as_command_fp_20261113")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        std::fs::write(proj.join("notes.txt"), "n\n").unwrap();
        Fx { base, home, proj }
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "quoted-heredoc-body-string-parsed-as-command-fp-20261113",
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
#[ignore = "RED 2026-11-13: quoted here-doc body line parsed as a command when a command precedes the here-doc"]
fn r1_minimal_and_list_then_quoted_heredoc_append_is_allowed() {
    let f = Fx::new("r1");
    let cmd = "true && cat >> scratch/x.py <<'X'\n\"cp notes.txt /etc/hosts\",\nX";
    assert_eq!(f.bash(cmd), "allow", "quoted here-doc body is data: {cmd}");
}

#[test]
#[ignore = "RED 2026-11-13: quoted here-doc body line parsed as a command when a command precedes the here-doc"]
fn r2_observed_probe_append_shape_is_allowed() {
    let f = Fx::new("r2");
    let cmd = format!(
        "cd {} && sed -e 's/a/b/' notes.txt > scratch/x.py && cat >> scratch/x.py <<'X'\n\
for cmd in [\"a\",\n            \"L=/etc; cp README.md $L/hosts\", \"L=/etc/hosts; mv README.md $L\", \"mv README.md /etc/hosts\",\n            \"b\"]:\n    print(cmd)\nX",
        f.proj.display()
    );
    assert_eq!(f.bash(&cmd), "allow", "quoted here-doc body is data: {cmd}");
}

#[test]
#[ignore = "RED 2026-11-13: quoted here-doc body line parsed as a command when a command follows the terminator"]
fn r3_git_commit_body_string_line_with_trailing_command_is_allowed() {
    let f = Fx::new("r3");
    let cmd = "git commit -q -F - <<'X'\n\"cp notes.txt /etc/hosts\",\nX\necho rc=$?";
    assert_eq!(f.bash(cmd), "allow", "quoted here-doc body is data: {cmd}");
}

#[test]
fn c1_control_same_shape_benign_line_is_allowed() {
    let f = Fx::new("c1");
    let cmd = "true && cat >> scratch/x.py <<'X'\n\"cp notes.txt scratch/y.txt\",\nX";
    assert_eq!(f.bash(cmd), "allow", "control must be Allow: {cmd}");
}

#[test]
fn c2_control_real_cp_after_and_list_stays_not_allowed() {
    let f = Fx::new("c2");
    let cmd = "true && cp notes.txt /etc/hosts";
    let v = f.bash(cmd);
    assert_ne!(
        v, "allow",
        "a real cp onto /etc/hosts must not be Allow: {cmd}"
    );
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}

#[test]
fn c3_control_unquoted_top_level_line_stays_not_allowed() {
    let f = Fx::new("c3");
    let cmd = "echo a\ncp notes.txt /etc/hosts\necho rc=$?";
    let v = f.bash(cmd);
    assert_ne!(
        v, "allow",
        "a real cp onto /etc/hosts must not be Allow: {cmd}"
    );
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}
