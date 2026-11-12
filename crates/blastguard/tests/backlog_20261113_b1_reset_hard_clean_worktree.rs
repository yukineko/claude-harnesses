#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED B1 (audit 2026-11-13, measured at 09dd0c8c on 2026-11-13).
//!
//! DEFECT: `git reset --hard <rev>` run by the MAIN session (no `agent_id`) in
//! a CLEAN linked git worktree (`git status --short` empty) is Denied with
//! "git reset --hard discards working-tree changes", although nothing
//! uncommitted exists to discard. The worktree here is a linked checkout that
//! is NOT under a managed storage root (`$HOME/.condukt/worktrees`,
//! `.harness-worktrees`), so the managed-place Allow does not apply.
//!
//! The RED asserts only that the verdict is not Deny (Allow or Ask would both
//! be acceptable for the filed defect; the audit wants it not blocked).
//!
//! DOES NOT PROVE: that a DIRTY tree is correctly handled (control below only
//! pins "not Allow"), nor other `reset` forms (`--merge`, `--keep`), nor
//! worktrees under managed roots (already Allow, see
//! managed_place_destructive_allow_20261107.rs), nor subagent payloads.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

struct Fx {
    base: PathBuf,
    home: PathBuf,
    wt: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("backlog_20261113_b1")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        let wt = base.join("src/plain-linked-wt");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("tracked.txt"), "clean\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        assert!(wt.join(".git").is_file(), "linked worktree has .git file");
        Fx { base, home, wt }
    }

    fn status(&self) -> String {
        let mut cmd = git_env::pinned(&self.wt);
        cmd.args(["status", "--short"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Main-session payload (no agent_id), cwd = the linked worktree.
    fn bash(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "b1-20261113",
            "hook_event_name": "PreToolUse",
            "cwd": self.wt.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.wt)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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
        String::from_utf8_lossy(&out.stdout).into_owned()
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
#[ignore = "RED: B1 — remove the ignore when fixed"]
fn b1_reset_hard_in_clean_linked_worktree_is_not_denied() {
    let f = Fx::new("clean");
    assert_eq!(f.status().trim(), "", "fixture: worktree must be clean");
    let out = f.bash("git reset --hard HEAD");
    assert_ne!(
        verdict(&out),
        "deny",
        "git reset --hard in a CLEAN linked worktree discards nothing and must not be Deny; got {out}"
    );
}

/// Control (not ignored): in a DIRTY linked worktree the same command is not
/// Allow. Pinned at HEAD to Deny.
#[test]
fn b1_control_reset_hard_in_dirty_linked_worktree_not_allow() {
    let f = Fx::new("dirty");
    std::fs::write(f.wt.join("tracked.txt"), "dirty\n").unwrap();
    assert!(!f.status().trim().is_empty(), "fixture: worktree dirty");
    let out = f.bash("git reset --hard HEAD");
    assert_ne!(
        verdict(&out),
        "allow",
        "reset --hard in a DIRTY tree must not be Allow; got {out}"
    );
}
