// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction (backlog 3c750df2): an `rm` operand that is a variable or
//! command substitution blastguard cannot resolve must be **Ask** for the main
//! agent (a subagent is auto-Deny), NOT a Deny. User ruling 2026-11-07
//! ("Ask, deny known paths"). Resolvable shapes naming a system path stay Deny
//! (controls; see also `system_harm_bypass_round2_20261107.rs` g3).
//!
//! Every assertion is made as the main agent AND as a subagent (payload carries
//! `agent_id`/`agent_type`). Each case gets a fresh session id and a fresh
//! HOME/approvals store unless the test is about repeats. Allow is exactly
//! empty stdout.

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Main,
    Sub,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Main => "main",
            Role::Sub => "subagent",
        }
    }
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    tag: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("rm_unresolved_operand_ask_20261107")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("src/repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Fixture {
            base,
            repo,
            tag: name.to_string(),
        }
    }

    /// One hook call. `case` selects an isolated HOME/store/session (own repeat
    /// ledger); pass the same `case` to share a session across calls.
    fn bash(&self, command: &str, role: Role, case: &str) -> String {
        let session = format!("bg-rmask-{}-{case}-{}", self.tag, std::process::id());
        let home = self.base.join("homes").join(case);
        std::fs::create_dir_all(&home).unwrap();
        let mut v = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if role == Role::Sub {
            v["agent_id"] = serde_json::json!("agent-rmask");
            v["agent_type"] = serde_json::json!("general-purpose");
        }
        let payload = v.to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env(
                "BLASTGUARD_APPROVALS_DIR",
                self.base.join("store").join(case),
            )
            .env("HOME", &home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_CODE_SESSION_ID", &session)
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `"allow"` for empty stdout, else the emitted `permissionDecision`.
    fn decision(&self, command: &str, role: Role, case: &str) -> String {
        let out = self.bash(command, role, case);
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out}"))
            .pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("no permissionDecision in {out}"))
    }

    fn fresh_case(&self) -> String {
        format!("c{}", SEQ.fetch_add(1, Ordering::Relaxed))
    }

    /// Each variant: main must be `want_main`, subagent must be `want_sub`.
    fn assert_split(&self, group: &str, want_main: &str, want_sub: &str, variants: &[&str]) {
        let mut failing = Vec::new();
        for c in variants {
            for (role, want) in [(Role::Main, want_main), (Role::Sub, want_sub)] {
                let d = self.decision(c, role, &self.fresh_case());
                if d != want {
                    failing.push(format!("[{}] {c:?} -> {d} (want {want})", role.name()));
                }
            }
        }
        assert!(
            failing.is_empty(),
            "{group}: {} of {} (variant x role) not as ruled (user ruling 2026-11-07):\n{}",
            failing.len(),
            variants.len() * 2,
            failing.join("\n")
        );
    }

    fn assert_all(&self, group: &str, want: &str, variants: &[&str]) {
        self.assert_split(group, want, want, variants);
    }
}

// ---- Targets: unresolved rm operand => Ask (main), Deny (subagent) ----

#[test]
fn t1_unresolved_rm_operand_is_ask_for_main_deny_for_sub() {
    Fixture::new("t1").assert_split(
        "t1 unresolved rm operand",
        "ask",
        "deny",
        &[
            r#"rm "$f""#,
            "rm $f",
            r#"rm -f "$TMPDIR/x""#,
            r#"rm "$(mktemp)""#,
            "for f in *.tmp; do rm \"$f\"; done",
        ],
    );
}

#[test]
fn t2_unresolved_rm_operand_repeat_stays_ask_for_main() {
    let f = Fixture::new("t2");
    let mut failing = Vec::new();
    for n in 1..=3 {
        let d = f.decision(r#"rm "$f""#, Role::Main, "rep-main");
        if d != "ask" {
            failing.push(format!("[main] rm \"$f\" call #{n} -> {d} (want ask)"));
        }
    }
    assert!(
        failing.is_empty(),
        "t2: {} call(s) not Ask (user ruling 2026-11-07):\n{}",
        failing.len(),
        failing.join("\n")
    );
}

// ---- Controls (must pass now and after the fix) ----

#[test]
fn control_resolvable_system_path_var_stays_deny_on_repeat() {
    let f = Fixture::new("ctl1");
    let mut failing = Vec::new();
    for role in [Role::Main, Role::Sub] {
        let case = format!("rep-{}", role.name());
        for n in 1..=3 {
            let d = f.decision("X=/etc/passwd; rm $X", role, &case);
            if d != "deny" {
                failing.push(format!("[{}] call #{n} -> {d}", role.name()));
            }
        }
    }
    assert!(
        failing.is_empty(),
        "X=/etc/passwd; rm $X must stay Deny:\n{}",
        failing.join("\n")
    );
}

#[test]
fn control_resolvable_command_substitution_system_path_is_deny() {
    Fixture::new("ctl2").assert_all("control deny", "deny", &["rm $(echo /etc/passwd)"]);
}

#[test]
fn control_plain_relative_rm_is_allow() {
    Fixture::new("ctl3").assert_all("control allow", "allow", &["rm -f build.log"]);
}
