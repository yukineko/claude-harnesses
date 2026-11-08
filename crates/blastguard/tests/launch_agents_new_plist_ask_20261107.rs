// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction, written by an agent that did not write (and will not
//! write) the fix (CLAUDE.md §2(a)).
//!
//! A NEW `.plist` in `$HOME/Library/LaunchAgents` registers a program that
//! launchd runs at every login (persistence). Today every write shape is
//! Allow there: `cp`, `echo >`, `tee`, and the python3 interpreter forms
//! (stdin heredoc with `shutil.copy2` / `open(...,"w")`, `python3 -c`).
//!
//! User ruling 2026-11-07 (relayed by the coordinator): a new file in
//! `~/Library/LaunchAgents` is **Ask** for both the direct and the interpreter
//! forms. As everywhere in blastguard, a subagent cannot answer an Ask, so the
//! subagent verdict for an Ask is Deny (`tests/subagent_no_ask.rs`); this test
//! asserts main = ask, subagent = deny.
//!
//! HOME is sandboxed: each hook call gets its own fixture HOME (with an empty
//! `Library/LaunchAgents` directory), and the target is an absolute path under
//! that HOME, so the test never touches the real `~/Library/LaunchAgents`
//! and never depends on what is in it.
//!
//! Control (pinned): the same forms writing a new file in the fixture repo
//! stay Allow under the same sandboxed HOME, so a RED here cannot be
//! "the fixture refuses everything".

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
            .join("launch_agents_new_plist_ask_20261107")
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

    /// A fresh sandboxed HOME with an empty `Library/LaunchAgents`.
    fn fresh_home(&self) -> PathBuf {
        let case = format!("c{}", SEQ.fetch_add(1, Ordering::Relaxed));
        let home = self.base.join("homes").join(case);
        std::fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
        home
    }

    /// One hook call with `home` as HOME, a fresh session and approvals store.
    fn decision(&self, home: &Path, command: &str, role: Role) -> String {
        let case = home.file_name().unwrap().to_string_lossy().into_owned();
        let session = format!("bg-lagent-{}-{case}-{}", self.tag, std::process::id());
        let mut v = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if role == Role::Sub {
            v["agent_id"] = serde_json::json!("agent-lagent");
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
            .env("HOME", home)
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

    /// Direct and interpreter forms of "create a new file at `target`".
    fn forms(&self, target: &str) -> Vec<(&'static str, String)> {
        let repo = self.repo.display();
        let src = self.src();
        vec![
            ("cp", format!("cp {src} {target}")),
            ("echo >", format!("echo x > {target}")),
            ("tee", format!("echo x | tee {target}")),
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

/// RED: a new plist in the sandboxed `$HOME/Library/LaunchAgents` is Ask
/// (main) / Deny (subagent) in every direct and interpreter form.
#[test]
fn new_launch_agent_plist_is_ask_in_every_form() {
    let f = Fixture::new("red_lagent");
    let mut failing = Vec::new();
    let mut total = 0;
    for (role, want) in [(Role::Main, "ask"), (Role::Sub, "deny")] {
        for i in 0..f.forms("x").len() {
            let home = f.fresh_home();
            let target = home
                .join("Library/LaunchAgents/new.plist")
                .display()
                .to_string();
            assert!(
                !Path::new(&target).exists(),
                "fixture precondition: {target} must not exist"
            );
            let (name, c) = f.forms(&target).swap_remove(i);
            total += 1;
            let d = f.decision(&home, &c, role);
            if d != want {
                failing.push(format!(
                    "[{}] {name} -> {target}: {d} (want {want})",
                    role.name()
                ));
            }
        }
    }
    assert!(
        failing.is_empty(),
        "new ~/Library/LaunchAgents plist not refused as ruled (user ruling 2026-11-07: Ask \
         for direct and interpreter forms; subagent Ask = Deny) — {} of {total} (form x role):\n{}",
        failing.len(),
        failing.join("\n")
    );
}

/// CONTROL (pinned): under the same sandboxed HOME, the same forms writing a
/// new file in the repo stay Allow.
#[test]
fn control_new_file_in_repo_under_sandboxed_home_stays_allow() {
    let f = Fixture::new("control_repo");
    let mut failing = Vec::new();
    for role in [Role::Main, Role::Sub] {
        for i in 0..f.forms("x").len() {
            let home = f.fresh_home();
            let target = f
                .repo
                .join(format!("new_{i}_{}.txt", role.name()))
                .display()
                .to_string();
            assert!(!Path::new(&target).exists());
            let (name, c) = f.forms(&target).swap_remove(i);
            let d = f.decision(&home, &c, role);
            if d != "allow" {
                failing.push(format!("[{}] {name} -> {target}: {d}", role.name()));
            }
        }
    }
    assert!(
        failing.is_empty(),
        "control: new file in the repo under a sandboxed HOME must stay allow:\n{}",
        failing.join("\n")
    );
}
