// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Three memory rules turned into blastguard behaviour. Black-box: each case
//! drives the real `blastguard` binary with a PreToolUse Bash payload and reads
//! the hook stdout (empty = Allow).
//!
//! 1. A backtick inside a DOUBLE-quoted argument to `backlog add|edit` or
//!    `git commit` is command-substituted by the shell and that span silently
//!    disappears from the stored text -> Ask. Single-quoted stays Allow.
//! 2. `git checkout -- <file>` on a file with uncommitted changes destroys
//!    them -> blocked; a clean file stays Allow; undeterminable -> blocked.
//! 3. GitHub Actions observation (`gh run list|view|watch`) is Allow; executing
//!    or configuring it (`gh workflow run`, `gh run rerun`) is Ask, never Deny
//!    (user ruling 2026-10-29). `gh pr ...` / `gh issue ...` reads stay Allow.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/git_env.rs"]
mod git_env;

struct Fx {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let root =
            Path::new("/var/tmp").join(format!("bg-memory-rules-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let base = std::fs::canonicalize(&root).unwrap();
        let f = Fx {
            home: base.join("home"),
            repo: base.join("proj"),
            base,
        };
        std::fs::create_dir_all(&f.home).unwrap();
        std::fs::create_dir_all(&f.repo).unwrap();
        f
    }

    fn git(&self, args: &[&str]) {
        let out = git_env::scrub(&mut Command::new("git"))
            .arg("-C")
            .arg(&self.repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.home)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn run(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "memory-rules",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut child = git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_blastguard")))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("TMPDIR")
            .current_dir(&self.base)
            .spawn()
            .expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn decision(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "unparsed"
    }
}

#[test]
fn rule1_backtick_in_double_quoted_note_or_commit_message_asks() {
    let f = Fx::new("r1");
    for cmd in [
        "backlog add --notes \"fix `foo` bug\" \"title\"",
        "backlog edit abc123 --notes \"see `bar`\"",
        "git commit -m \"use `bar`\"",
    ] {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "ask", "`{cmd}` must Ask; got {out:?}");
    }
    // Control: single-quoted backticks are literal text, nothing is substituted.
    for cmd in [
        "backlog add --notes 'fix `foo` bug' 'title'",
        "git commit -m 'use `bar`'",
    ] {
        let out = f.run(cmd);
        assert_eq!(
            decision(&out),
            "allow",
            "`{cmd}` must stay Allow; got {out:?}"
        );
    }
}

#[test]
fn rule2_git_checkout_of_a_dirty_file_is_blocked_clean_file_allowed() {
    let f = Fx::new("r2");
    f.git(&["init", "-q", "-b", "main"]);
    std::fs::write(f.repo.join("dirty.txt"), "committed\n").unwrap();
    std::fs::write(f.repo.join("clean.txt"), "committed\n").unwrap();
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "init"]);
    std::fs::write(f.repo.join("dirty.txt"), "uncommitted fix\n").unwrap();

    for cmd in [
        "git checkout -- dirty.txt",
        "git checkout HEAD -- dirty.txt",
        "git restore dirty.txt",
        // Cannot determine dirtiness (no such path) -> restrictive side.
        "git checkout -- no-such-file.txt",
    ] {
        let out = f.run(cmd);
        let d = decision(&out);
        assert!(
            d == "deny" || d == "ask",
            "`{cmd}` must be blocked (deny/ask); got {d}: {out:?}"
        );
    }
    // Control: a clean tracked file loses nothing.
    let out = f.run("git checkout -- clean.txt");
    assert_eq!(
        decision(&out),
        "allow",
        "clean checkout must stay Allow; got {out:?}"
    );
}

#[test]
fn rule3_gha_observation_allowed_execution_asks_never_denies() {
    let f = Fx::new("r3");
    for cmd in [
        "gh run list",
        "gh run watch 123",
        "gh run view 123 --log",
        "gh workflow list",
        "gh pr list",
        "gh issue list",
    ] {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "allow", "`{cmd}` must Allow; got {out:?}");
    }
    for cmd in ["gh workflow run ci.yml", "gh run rerun 123"] {
        let out = f.run(cmd);
        assert_eq!(
            decision(&out),
            "ask",
            "`{cmd}` must Ask (not deny); got {out:?}"
        );
    }
}
