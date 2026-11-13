#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-14 suspected false positive (observed in a subagent): a command
//! `/bin/sh -c '... ( ... ) & wait $!; ...'` was blocked because the command
//! word `$!` was judged an unresolvable expansion. The real command word is
//! `wait`; `$!` (PID of the last background job) is only its ARGUMENT.
//!
//! Observed 2026-11-14 at c4336b61: plain `sleep 1 & wait $!` is Allow (r1,
//! passes, kept as control); the `/bin/sh -c '...'` wrapped form is Ask with
//! reason "the command word `$!` is an expansion ..." (r2, RED).
//! `r2` expects Allow. Controls `c1`/`c2` must stay non-Allow:
//! `$!` as a genuine command word, and a destructive command after `wait $!`.
//! Decisions only are asserted, never message prose.
//!
//! RED cases are `#[ignore]` per repo convention; run with `-- --ignored`.
//!
//! DOES NOT PROVE: other special parameters (`$?`, `$$`, `$@`) as arguments
//! of `wait`, or `wait` with job specs (`%1`).

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
            .join("wait_bang_argument_fp_20261114")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        Fx { base, home, proj }
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "wait-bang-argument-fp-20261114",
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
fn r1_control_wait_bang_plain_is_allowed() {
    let f = Fx::new("r1");
    let cmd = "sleep 1 & wait $!";
    assert_eq!(f.bash(cmd), "allow", "`$!` is an argument of wait: {cmd}");
}

#[test]
#[ignore = "RED 2026-11-14: `wait $!` argument judged as a command word"]
fn r2_wait_bang_inside_sh_c_is_allowed() {
    let f = Fx::new("r2");
    let cmd = "/bin/sh -c 'sleep 1 & wait $!'";
    assert_eq!(f.bash(cmd), "allow", "`$!` is an argument of wait: {cmd}");
}

#[test]
fn c1_control_bang_as_real_command_word_stays_not_allowed() {
    let f = Fx::new("c1");
    let cmd = "sleep 1 & $! foo";
    let v = f.bash(cmd);
    assert_ne!(v, "allow", "`$!` as command word must not be Allow: {cmd}");
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}

#[test]
fn c2_control_destructive_command_after_wait_bang_stays_not_allowed() {
    let f = Fx::new("c2");
    let cmd = "sleep 1 & wait $!; rm -rf /etc";
    let v = f.bash(cmd);
    assert_ne!(v, "allow", "rm -rf /etc must not be Allow: {cmd}");
    assert_ne!(v, "other", "unrecognised hook output: {cmd}");
}
