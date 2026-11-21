#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro of a blastguard false positive observed 2026-11-21 in the MAIN
//! Claude session (session project dir = the primary checkout). The session
//! had created a linked worktree of that same repo under the sibling dir
//! `~/src/.harness-worktrees/flow-watch-20261120` (branch
//! `flow/watch-20261120`) and ran
//! `cd <that worktree> && mkdir -p crates/flow/bin && cat > crates/flow/bin/flow-watch <<'EOF'`
//! ... `EOF`. Verbatim refusal:
//! "'> /Users/yuki/src/.harness-worktrees/flow-watch-20261120/crates/flow/bin/flow-watch'
//! destroys the file's current contents and it is inside a git work tree, but
//! not one of this session's trees (neither inside its safe roots nor a
//! recognised worktree checkout), so git being able to restore it does not
//! make the overwrite this session's to make".
//!
//! Distinct from backlog 94b93e95 (python3 heredoc open() in
//! `~/.condukt/worktrees`): this is a plain shell `>` redirect, the worktree
//! lives under the repo's `.harness-worktrees` sibling dir, and the session
//! project dir is the PRIMARY checkout (not a worktree), which is also why the
//! other `.harness-worktrees` tests (session cwd = own worktree) do not cover
//! it.
//!
//! The RED asserts only that the whose-tree refusal is not the verdict. The
//! controls: the same redirect into a git repo that is NOT a worktree of the
//! session's repo stays Deny, and the fixture recognises the worktree at all.
//!
//! Fixture: `support/neutral_base.rs` location rule; `<base>/src/proj` primary
//! checkout (session project dir), `<base>/src/.harness-worktrees/flow-watch`
//! its linked worktree, `<base>/src/other` an unrelated repo. Nothing is
//! executed by the hook.
//!
//! DOES NOT PROVE: the exact heredoc body of the observation (a short stand-in
//! is used); verdicts with a session id other than the fixed one.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::Command;

const WHOSE_TREE: &str = "not one of this session's trees";
const REL: &str = "crates/flow/bin/flow-watch";

struct Fx {
    base: PathBuf,
    home: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
    other: PathBuf,
}

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", home);
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo(home: &Path, repo: &Path) {
    std::fs::create_dir_all(repo.join("crates/flow/bin")).unwrap();
    git(home, repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(REL), "#!/bin/sh\n").unwrap();
    git(home, repo, &["add", REL]);
    git(home, repo, &["commit", "-q", "-m", "init"]);
}

impl Fx {
    fn new(name: &str) -> Fx {
        let root = neutral_base::neutral_base()
            .join(format!("bg-redir-wt-fp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let base = std::fs::canonicalize(&root).unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let src = base.join("src");
        std::fs::create_dir_all(src.join(".harness-worktrees")).unwrap();
        let primary = src.join("proj");
        init_repo(&home, &primary);
        let wt = src.join(".harness-worktrees/flow-watch-20261120");
        git(
            &home,
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "flow/watch-20261120",
                wt.to_str().unwrap(),
            ],
        );
        assert!(wt.join(".git").is_file(), "fixture: linked worktree");
        let other = src.join("other");
        init_repo(&home, &other);
        Fx {
            base,
            home,
            primary,
            wt,
            other,
        }
    }

    /// Decision plus raw stdout, judged as a Bash call from the session's
    /// project dir (the primary checkout).
    fn bash(&self, command: &str) -> (String, String) {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "redir-wt-fp-session",
            "cwd": self.primary.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.env("CLAUDE_PROJECT_DIR", &self.primary)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("TMPDIR")
            .current_dir(&self.primary);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0 ({command:?}); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if stdout.trim().is_empty() {
            return ("allow".into(), stdout);
        }
        let doc: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
        let d = doc["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or_else(|| panic!("no permissionDecision: {stdout}"))
            .to_string();
        (d, stdout)
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn cat_redirect(dir: &Path) -> String {
    format!(
        "cd {} && mkdir -p crates/flow/bin && cat > {REL} <<'EOF'\n#!/usr/bin/env python3\nprint('hi')\nEOF\n",
        dir.display()
    )
}

/// RED: observed shape, relative `>` target after `cd` into a registered
/// linked worktree of the session's repo.
#[test]
fn cd_then_cat_redirect_into_linked_worktree_is_not_whose_tree_deny() {
    let fx = Fx::new("cd");
    let cmd = cat_redirect(&fx.wt);
    let (d, out) = fx.bash(&cmd);
    assert!(
        d != "deny" && !out.contains(WHOSE_TREE),
        "a registered linked worktree of the session's repo is the session's tree, but \
{cmd:?} got decision {d}: {out}"
    );
}

/// Guard (green, measured 2026-11-21): same overwrite with an absolute target
/// and no `cd` is already not Deny, so the FP is specific to the `cd`-resolved
/// relative target.
#[test]
fn absolute_redirect_into_linked_worktree_is_not_whose_tree_deny() {
    let fx = Fx::new("abs");
    let cmd = format!("echo a > {}/{REL}", fx.wt.display());
    let (d, out) = fx.bash(&cmd);
    assert!(
        d != "deny" && !out.contains(WHOSE_TREE),
        "{cmd:?} got decision {d}: {out}"
    );
}

/// Control (stays green): the same redirect into a git repo that is NOT a
/// worktree of the session's repo stays Deny.
#[test]
fn control_redirect_into_unrelated_git_repo_stays_deny() {
    let fx = Fx::new("other");
    let cmd = cat_redirect(&fx.other);
    let (d, out) = fx.bash(&cmd);
    assert_eq!(d, "deny", "{cmd:?} must stay Deny: {out}");
}
