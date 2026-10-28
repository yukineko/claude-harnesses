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
//! 3. GitHub Actions is banned IN claude-harnesses (its CLAUDE.md §7), not in
//!    every repository: `gh run ...` / `gh workflow ...` aimed at that
//!    repository (a remote of the cwd's repo, `-R`/`--repo`, or `GH_REPO`) ->
//!    Deny; aimed at any other repository -> Allow; target undeterminable ->
//!    Deny. `gh pr ...` / `gh issue ...` stay Allow.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        let out = Command::new("git")
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_blastguard"))
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
fn rule3_github_actions_commands_are_denied_pr_and_issue_allowed() {
    let f = Fx::new("r3");
    f.git(&["init", "-q", "-b", "main"]);
    f.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/yukineko/claude-harnesses.git",
    ]);
    for cmd in GH_ACTIONS_CMDS {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "deny", "`{cmd}` must Deny; got {out:?}");
    }
    for cmd in ["gh pr list", "gh pr create --fill", "gh issue list"] {
        let out = f.run(cmd);
        assert_eq!(
            decision(&out),
            "allow",
            "`{cmd}` must stay Allow; got {out:?}"
        );
    }
}

const GH_ACTIONS_CMDS: [&str; 5] = [
    "gh run list",
    "gh run watch 123",
    "gh run view 123 --log",
    "gh workflow run ci.yml",
    "gh workflow list",
];

#[test]
fn rule3_github_actions_commands_are_allowed_in_other_repositories() {
    let f = Fx::new("r3-other");
    f.git(&["init", "-q", "-b", "main"]);
    f.git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:Org/medipartner_transaction.git",
    ]);
    for cmd in GH_ACTIONS_CMDS {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "allow", "`{cmd}` must Allow; got {out:?}");
    }
    // Another repository named explicitly is not claude-harnesses either.
    let out = f.run("gh run list -R someone/other-repo");
    assert_eq!(decision(&out), "allow", "got {out:?}");
}

#[test]
fn rule3_not_a_git_repository_is_not_claude_harnesses() {
    let f = Fx::new("r3-nogit");
    let out = f.run("gh run list");
    assert_eq!(decision(&out), "allow", "got {out:?}");
}

#[test]
fn rule3_claude_harnesses_reached_any_way_is_denied() {
    // gh may resolve to any remote, not just origin.
    let g = Fx::new("r3-upstream");
    g.git(&["init", "-q", "-b", "main"]);
    g.git(&["remote", "add", "origin", "https://github.com/me/fork.git"]);
    g.git(&[
        "remote",
        "add",
        "upstream",
        "git@github.com:yukineko/Claude-Harnesses",
    ]);
    let out = g.run("gh run list");
    assert_eq!(decision(&out), "deny", "upstream remote; got {out:?}");

    let f = Fx::new("r3-named");
    f.git(&["init", "-q", "-b", "main"]);
    f.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/Org/medipartner_transaction.git",
    ]);
    for cmd in [
        "gh run list -R yukineko/claude-harnesses",
        "gh run list --repo yukineko/claude-harnesses",
        "gh run list --repo=github.com/yukineko/claude-harnesses",
        "gh -R yukineko/claude-harnesses workflow list",
        "GH_REPO=yukineko/claude-harnesses gh run list",
        "export GH_REPO=yukineko/claude-harnesses; gh run list",
    ] {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "deny", "`{cmd}` must Deny; got {out:?}");
    }
    // A `cd` into claude-harnesses earlier on the line moves where gh runs.
    let cmd = format!("cd {} && gh run list", g.repo.display());
    let out = f.run(&cmd);
    assert_eq!(decision(&out), "deny", "`{cmd}` must Deny; got {out:?}");
}

#[test]
fn rule3_undeterminable_repository_is_denied() {
    let f = Fx::new("r3-undet");
    f.git(&["init", "-q", "-b", "main"]);
    f.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/Org/medipartner_transaction.git",
    ]);
    for cmd in [
        "gh run list -R \"$R\"",
        "gh run list --repo \"$(cat r)\"",
        "gh run list -R",
    ] {
        let out = f.run(cmd);
        assert_eq!(decision(&out), "deny", "`{cmd}` must Deny; got {out:?}");
    }
}
