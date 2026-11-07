// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction: (1) a `for` loop variable used as an unquoted `rm` operand
//! must not be Deny for the main agent (user ruling 2026-11-07: unresolved rm
//! operands are Ask; the glob belongs to `for`, not to `rm`). (2) Resolvable
//! system paths reaching `unlink`/`mv`/`chmod` through a variable must be Deny
//! for both roles, including on repeats within one session.
//!
//! Real binary, fresh session per case, main agent AND subagent. Allow is
//! exactly empty stdout.

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
            .join("loopvar_and_mutator_var_20261107")
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

    fn bash(&self, command: &str, role: Role, case: &str) -> String {
        let session = format!("bg-loopvar-{}-{case}-{}", self.tag, std::process::id());
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
            v["agent_id"] = serde_json::json!("agent-loopvar");
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

    /// Main agent only: none of the variants may be Deny.
    fn assert_main_not_deny(&self, group: &str, variants: &[&str]) {
        let mut failing = Vec::new();
        for c in variants {
            let d = self.decision(c, Role::Main, &self.fresh_case());
            if d == "deny" {
                failing.push(format!("[main] {c:?} -> {d} (want not deny)"));
            }
        }
        assert!(
            failing.is_empty(),
            "{group}: {} variant(s) Deny:\n{}",
            failing.len(),
            failing.join("\n")
        );
    }

    /// Each variant, both roles, 3 calls in one shared session per role: all Deny.
    fn assert_deny_repeat(&self, group: &str, variants: &[&str]) {
        let mut failing = Vec::new();
        for c in variants {
            for role in [Role::Main, Role::Sub] {
                let case = self.fresh_case();
                for n in 1..=3 {
                    let d = self.decision(c, role, &case);
                    if d != "deny" {
                        failing.push(format!(
                            "[{}] {c:?} call #{n} -> {d} (want deny)",
                            role.name()
                        ));
                    }
                }
            }
        }
        assert!(
            failing.is_empty(),
            "{group}: {} call(s) not Deny:\n{}",
            failing.len(),
            failing.join("\n")
        );
    }

    fn assert_all(&self, group: &str, want: &str, variants: &[&str]) {
        let mut failing = Vec::new();
        for c in variants {
            for role in [Role::Main, Role::Sub] {
                let d = self.decision(c, role, &self.fresh_case());
                if d != want {
                    failing.push(format!("[{}] {c:?} -> {d} (want {want})", role.name()));
                }
            }
        }
        assert!(
            failing.is_empty(),
            "{group}: not as expected:\n{}",
            failing.join("\n")
        );
    }
}

// ---- Targets ----

#[test]
#[ignore = "RED: loop var rm / mutator var resolution"]
fn t1_for_loop_unquoted_var_rm_is_not_deny_for_main() {
    Fixture::new("t1").assert_main_not_deny(
        "t1 for-loop var rm",
        &[
            "for f in *.tmp; do rm $f; done",
            "for f in build/*.o; do rm $f; done",
        ],
    );
}

#[test]
#[ignore = "RED: loop var rm / mutator var resolution"]
fn t2_for_loop_unquoted_var_rm_tmp_glob_is_ask_for_main() {
    Fixture::new("t2").assert_split_main_ask();
}

impl Fixture {
    fn assert_split_main_ask(&self) {
        let c = "for f in *.tmp; do rm $f; done";
        let d = self.decision(c, Role::Main, &self.fresh_case());
        assert_eq!(
            d, "ask",
            "[main] {c:?} -> {d} (want ask, ruling 2026-11-07)"
        );
    }
}

#[test]
#[ignore = "RED: loop var rm / mutator var resolution"]
fn t3_mutator_var_resolving_to_system_path_is_deny_on_repeat() {
    Fixture::new("t3").assert_deny_repeat(
        "t3 mutator var system path",
        &[
            "X=/etc/passwd; unlink $X",
            "X=/etc/hosts; mv $X /tmp/h",
            "X=/etc/passwd; chmod 000 $X",
        ],
    );
}

// ---- Controls (pin current verdict) ----

#[test]
fn control_for_loop_system_glob_is_deny() {
    Fixture::new("ctl1").assert_all("ctl1", "deny", &["for f in /etc/*; do rm $f; done"]);
}

#[test]
fn control_for_loop_quoted_var_not_deny_for_main() {
    Fixture::new("ctl2").assert_main_not_deny("ctl2", &["for f in *.tmp; do rm \"$f\"; done"]);
}

#[test]
fn control_unlink_relative_var_not_deny() {
    // Main only: a subagent gets Deny for any unresolved/ask-class call
    // (observed: subagent -> deny for this command).
    Fixture::new("ctl3").assert_main_not_deny("ctl3", &["X=build.log; unlink $X"]);
}

#[test]
fn control_plain_relative_rm_is_allow() {
    Fixture::new("ctl4").assert_all("ctl4", "allow", &["rm -f build.log"]);
}
