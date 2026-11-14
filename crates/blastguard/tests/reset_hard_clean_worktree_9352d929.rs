#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 9352d929 (GH #1660): `git reset --hard HEAD` relief in a CLEAN linked
//! worktree. Allow only for the literal rev HEAD, run in a directory that has
//! a `.git` file and whose `git status` probe positively reports clean.
//! Everything else keeps Deny.
//!
//! DOES NOT PROVE: `--merge` / `--keep`, subagent payloads. Untracked files
//! are deliberately kept Deny (not shown to be harmless), pinned below.
//! assume-unchanged / skip-worktree entries and dirty submodule content
//! hidden by `submodule.<name>.ignore=all` are also kept Deny (fail closed;
//! whether the reset would actually discard them is NOT observed here).

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
    /// `managed`: put the linked worktree under `$HOME/.condukt/worktrees`.
    fn new(name: &str, managed: bool) -> Fx {
        let base = neutral_base::neutral_base()
            .join("reset_hard_9352d929")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        let wt = if managed {
            home.join(".condukt/worktrees/RUN-t1")
        } else {
            base.join("src/plain-linked-wt")
        };
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
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

    fn bash_in(&self, cwd: &Path, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "r9352d929",
            "hook_event_name": "PreToolUse",
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", cwd)
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
        assert_eq!(out.status.code(), Some(0), "hook must exit 0");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn bash(&self, command: &str) -> String {
        self.bash_in(&self.wt.clone(), command)
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
fn clean_head_is_allow() {
    let f = Fx::new("clean_allow", false);
    let out = f.bash("git reset --hard HEAD");
    assert_eq!(verdict(&out), "allow", "got {out}");
}

#[test]
fn clean_head_via_dash_c_is_allow() {
    let f = Fx::new("clean_dash_c", false);
    let out = f.bash_in(
        &f.base,
        &format!("git -C {} reset --hard HEAD", f.wt.display()),
    );
    assert_eq!(verdict(&out), "allow", "got {out}");
}

#[test]
fn dash_c_to_dirty_tree_with_clean_cwd_is_not_allow() {
    let f = Fx::new("dash_c_dirty", false);
    std::fs::write(f.wt.join("tracked.txt"), "dirty\n").unwrap();
    let out = f.bash_in(
        &f.base,
        &format!("git -C {} reset --hard HEAD", f.wt.display()),
    );
    assert_eq!(verdict(&out), "deny", "got {out}");
}

/// Undetermined cleanliness: the `.git` file points at a gitdir that does not
/// exist, so the status probe exits non-zero. Must not be Allow.
#[test]
fn undeterminable_status_probe_is_not_allow() {
    let f = Fx::new("probe_fails", false);
    std::fs::write(f.wt.join(".git"), "gitdir: /nonexistent/9352d929\n").unwrap();
    let out = f.bash("git reset --hard HEAD");
    assert_eq!(verdict(&out), "deny", "got {out}");
}

#[test]
fn non_head_rev_keeps_deny() {
    let f = Fx::new("rev", false);
    for rev in ["main", "HEAD~0", "HEAD^", "wt", "origin/main"] {
        let out = f.bash(&format!("git reset --hard {rev}"));
        assert_eq!(verdict(&out), "deny", "rev {rev}: got {out}");
    }
}

#[test]
fn omitted_rev_keeps_deny() {
    let f = Fx::new("norev", false);
    let out = f.bash("git reset --hard");
    assert_eq!(verdict(&out), "deny", "got {out}");
}

#[test]
fn staged_only_is_not_allow() {
    let f = Fx::new("staged", false);
    std::fs::write(f.wt.join("tracked.txt"), "staged\n").unwrap();
    git(&f.wt, &["add", "tracked.txt"]);
    let out = f.bash("git reset --hard HEAD");
    assert_eq!(verdict(&out), "deny", "got {out}");
}

#[test]
fn untracked_file_is_not_allow() {
    let f = Fx::new("untracked", false);
    std::fs::write(f.wt.join("new.txt"), "x\n").unwrap();
    let out = f.bash("git reset --hard HEAD");
    assert_eq!(verdict(&out), "deny", "got {out}");
}

#[test]
fn repointing_globals_keep_deny() {
    let f = Fx::new("globals", false);
    for cmd in [
        "git -c core.worktree=/tmp reset --hard HEAD",
        "git --namespace x reset --hard HEAD",
        "git --git-dir=.git reset --hard HEAD",
    ] {
        let out = f.bash(cmd);
        assert_eq!(verdict(&out), "deny", "{cmd}: got {out}");
    }
}

#[test]
fn primary_checkout_clean_keeps_deny() {
    let f = Fx::new("primary", false);
    let repo = f.base.join("src/repo");
    let out = f.bash_in(&repo, "git reset --hard HEAD");
    assert_eq!(verdict(&out), "deny", "got {out}");
}

#[test]
fn managed_root_allow_unchanged_even_when_dirty() {
    let f = Fx::new("managed", true);
    std::fs::write(f.wt.join("tracked.txt"), "dirty\n").unwrap();
    for cmd in ["git reset --hard HEAD", "git reset --hard main"] {
        let out = f.bash(cmd);
        assert_eq!(verdict(&out), "allow", "{cmd}: got {out}");
    }
}

/// An assume-unchanged entry with a local edit is invisible to `git status`;
/// the probe must not read that as clean.
#[test]
fn assume_unchanged_local_edit_is_not_allow() {
    let f = Fx::new("assume_unchanged", false);
    git(
        &f.wt,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    );
    std::fs::write(f.wt.join("tracked.txt"), "hidden edit\n").unwrap();
    let out = f.bash("git reset --hard HEAD");
    assert_eq!(verdict(&out), "deny", "got {out}");
}

/// A skip-worktree entry with a local edit is invisible to `git status`;
/// the probe must not read that as clean.
#[test]
fn skip_worktree_local_edit_is_not_allow() {
    let f = Fx::new("skip_worktree", false);
    git(&f.wt, &["update-index", "--skip-worktree", "tracked.txt"]);
    std::fs::write(f.wt.join("tracked.txt"), "hidden edit\n").unwrap();
    let out = f.bash("git reset --hard HEAD");
    assert_eq!(verdict(&out), "deny", "got {out}");
}

/// `submodule.<name>.ignore=all` hides dirty submodule content from a plain
/// `git status`; the probe must still see it.
#[test]
fn dirty_submodule_hidden_by_ignore_all_is_not_allow() {
    let f = Fx::new("submodule_ignore_all", false);
    let sub = f.base.join("src/subsrc");
    std::fs::create_dir_all(&sub).unwrap();
    git(&sub, &["init", "-q"]);
    std::fs::write(sub.join("s.txt"), "sub\n").unwrap();
    git(&sub, &["add", "s.txt"]);
    git(&sub, &["commit", "-q", "-m", "sub"]);
    git(
        &f.wt,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "sub",
        ],
    );
    git(
        &f.wt,
        &["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"],
    );
    git(&f.wt, &["add", ".gitmodules"]);
    git(&f.wt, &["commit", "-q", "-m", "add sub"]);
    std::fs::write(f.wt.join("sub/s.txt"), "dirty sub\n").unwrap();
    let out = f.bash("git reset --hard HEAD");
    assert_eq!(verdict(&out), "deny", "got {out}");
}
