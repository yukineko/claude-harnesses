// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction, written by an agent that did not write (and will not
//! write) the fix (CLAUDE.md §2(a)).
//!
//! A NEW file in a system directory is refused when written directly
//! (`cp src /etc/x`, `echo x > /etc/x`, `echo x | tee /etc/x` all answer
//! "writes into a system directory - a NEW file there is ..."), but the same
//! write carried by an interpreter is Allow:
//!
//!   * `cd <repo> && python3 - <<'EOF'` + `shutil.copy2("<src>", "/etc/x")`
//!   * `cd <repo> && python3 - <<'EOF'` + `open("/etc/x", "w").write("x")`
//!   * `python3 -c 'import shutil; shutil.copy2("<src>", "/etc/x")'`
//!
//! The interpreter path only asks "does this destroy existing bytes?"
//! (`reversible::decide_recovery` -> `NothingToDestroy` for a missing target);
//! it has no system-directory check. User ruling 2026-11-07: system-harming
//! targets are Deny through any wrapper. A wrapper answering LESS
//! restrictively than the direct form is a fail-open.
//!
//! Property asserted: for each system target, every interpreter form is at
//! least as restrictive as the most restrictive direct form, as the main agent
//! and as a subagent. Controls pin the current Allow for a new file in the
//! repo and in /private/tmp through the same interpreter shapes.
//!
//! `~/Library/LaunchAgents` is deliberately NOT tested here: every form,
//! direct ones included, is Allow there, which is a separate question for the
//! user.

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
            .join("interpreter_system_dir_new_file_failopen_20261107")
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

    fn src(&self) -> String {
        self.repo.join("f.txt").display().to_string()
    }

    /// One hook call with a fresh session, HOME and approvals store.
    fn decision(&self, command: &str, role: Role) -> String {
        let case = format!("c{}", SEQ.fetch_add(1, Ordering::Relaxed));
        let session = format!("bg-sysnew-{}-{case}-{}", self.tag, std::process::id());
        let home = self.base.join("homes").join(&case);
        std::fs::create_dir_all(&home).unwrap();
        let mut v = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if role == Role::Sub {
            v["agent_id"] = serde_json::json!("agent-sysnew");
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
                self.base.join("store").join(&case),
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
        let out = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out}"))
            .pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("no permissionDecision in {out}"))
    }

    fn direct_forms(&self, target: &str) -> Vec<(&'static str, String)> {
        vec![
            ("cp", format!("cp {} {target}", self.src())),
            ("echo >", format!("echo x > {target}")),
            ("tee", format!("echo x | tee {target}")),
        ]
    }

    fn interpreter_forms(&self, target: &str) -> Vec<(&'static str, String)> {
        let repo = self.repo.display();
        let src = self.src();
        vec![
            (
                "heredoc shutil.copy2",
                format!(
                    "cd {repo} && python3 - <<'EOF'\nimport shutil\nshutil.copy2(\"{src}\", \"{target}\")\nEOF"
                ),
            ),
            (
                "heredoc open-w",
                format!(
                    "cd {repo} && python3 - <<'EOF'\nopen(\"{target}\", \"w\").write(\"x\")\nEOF"
                ),
            ),
            (
                "python3 -c shutil.copy2",
                format!("python3 -c 'import shutil; shutil.copy2(\"{src}\", \"{target}\")'"),
            ),
        ]
    }
}

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("unknown permissionDecision {other:?}"),
    }
}

/// Unique per test process, so "a NEW file" holds: asserted absent below.
fn leaf(stem: &str) -> String {
    format!("bg_sysnew_{stem}_{}", std::process::id())
}

fn system_targets() -> Vec<String> {
    vec![
        format!("/etc/{}", leaf("etc")),
        format!("/usr/local/bin/{}", leaf("ulb")),
        format!("/System/{}", leaf("sys")),
        format!("/Library/LaunchDaemons/{}.plist", leaf("ld")),
    ]
}

fn assert_absent(target: &str) {
    assert!(
        !Path::new(target).exists(),
        "fixture precondition: {target} must not exist (the case is a NEW file)"
    );
}

/// CONTROL: the direct forms already refuse a new file in each system
/// directory. If this fails the RED below proves nothing.
#[test]
fn control_direct_forms_refuse_new_system_file() {
    let f = Fixture::new("control_direct");
    let mut failing = Vec::new();
    for t in system_targets() {
        assert_absent(&t);
        for (name, c) in f.direct_forms(&t) {
            for role in [Role::Main, Role::Sub] {
                let d = f.decision(&c, role);
                if rank(&d) == 0 {
                    failing.push(format!("[{}] {name} -> {t}: {d}", role.name()));
                }
            }
        }
    }
    assert!(
        failing.is_empty(),
        "control: direct system-dir writes must be refused:\n{}",
        failing.join("\n")
    );
}

/// RED: an interpreter carrying the same new-file write into a system
/// directory must be at least as restrictive as the direct form.
#[test]
#[ignore = "backlog 0fbfe00d: interpreter new-file write into a system dir is Allow while the direct form is Deny (fail-open)"]
fn interpreter_new_system_file_at_least_as_restrictive_as_direct() {
    let f = Fixture::new("red_interp");
    let mut failing = Vec::new();
    let mut total = 0;
    for t in system_targets() {
        assert_absent(&t);
        for role in [Role::Main, Role::Sub] {
            let floor = f
                .direct_forms(&t)
                .iter()
                .map(|(_, c)| f.decision(c, role))
                .max_by_key(|d| rank(d))
                .unwrap();
            for (name, c) in f.interpreter_forms(&t) {
                total += 1;
                let d = f.decision(&c, role);
                if rank(&d) < rank(&floor) {
                    failing.push(format!(
                        "[{}] {name} -> {t}: {d} (direct form: {floor})",
                        role.name()
                    ));
                }
            }
        }
    }
    assert!(
        failing.is_empty(),
        "interpreter new-file write into a system dir is LESS restrictive than the direct form \
         ({} of {total} (form x target x role)) — fail-open (user ruling 2026-11-07: system \
         harm is Deny through any wrapper):\n{}",
        failing.len(),
        failing.join("\n")
    );
}

/// CONTROL (pinned): a new file in the repo or in /private/tmp through the
/// same interpreter shapes stays Allow. A fix must not refuse these.
#[test]
fn control_interpreter_new_file_in_repo_and_private_tmp_stays_allow() {
    let f = Fixture::new("control_allow");
    let targets = [
        f.repo.join(leaf("repo")).display().to_string(),
        format!("/private/tmp/{}", leaf("tmp")),
    ];
    let mut failing = Vec::new();
    for t in &targets {
        assert_absent(t);
        for (name, c) in f.interpreter_forms(t) {
            for role in [Role::Main, Role::Sub] {
                let d = f.decision(&c, role);
                if d != "allow" {
                    failing.push(format!("[{}] {name} -> {t}: {d}", role.name()));
                }
            }
        }
    }
    assert!(
        failing.is_empty(),
        "control: new file in repo / /private/tmp via interpreter must stay allow:\n{}",
        failing.join("\n")
    );
}
