// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros of two blastguard false positives observed 2026-11-07 in a
//! subagent working in `~/.condukt/worktrees/mab3-3da84f08` (a linked worktree
//! its parent session created with `git worktree add`).
//!
//! CASE 1: a `cd <wt> && ... && git show HEAD:F > F && ... ; cp ...` chain was
//! refused with "'> <wt>/F' destroys the file's current contents and it is
//! inside a git work tree, but not one of this session's trees (neither inside
//! its safe roots nor a recognised worktree checkout) ...".
//! CASE 3: `python3 -c '... s="> <wt>/F" ...'` (the `>` is text inside a
//! single-quoted shell word) got the same block.
//!
//! Each case has its own test so it can be filed separately. Fixture: a fake
//! HOME, a repo, and a linked worktree (real `git worktree add`) under
//! `$HOME/.condukt/worktrees`. Assertions are verdicts only, never message text
//! (except that CASE 1 also pins the refusal is not the whose-tree one).
//! Written by a disinterested author (CLAUDE.md section 2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::Command;

struct Fx {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    wt: PathBuf,
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

impl Fx {
    fn new(name: &str) -> Fx {
        // NOT under /tmp or /var/tmp: those are blastguard TEMP safe roots, which
        // would make every path in the fixture "inside the session's safe roots"
        // (measured: the fixture then allows, the real layout denies). The
        // calibrated fixture base (support/neutral_base.rs) is neither, and is
        // not under the workspace `target/` (backlog 37955e32).
        let root =
            neutral_base::neutral_base().join(format!("bg-wt-fp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let base = std::fs::canonicalize(&root).unwrap();
        let home = base.join("home");
        let repo = base.join("proj");
        std::fs::create_dir_all(home.join(".condukt/worktrees")).unwrap();
        std::fs::create_dir_all(repo.join("crates/condukt/src")).unwrap();
        git(&home, &repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("crates/condukt/src/maintree.rs"), "fn a() {}\n").unwrap();
        git(&home, &repo, &["add", "crates/condukt/src/maintree.rs"]);
        git(&home, &repo, &["commit", "-q", "-m", "init"]);
        let wt = home.join(".condukt/worktrees/mab3-3da84f08");
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "mab3/3da84f08",
                wt.to_str().unwrap(),
            ],
        );
        assert!(wt.join(".git").is_file(), "fixture: linked worktree");
        // The parent session's own worktree (the session runs there, as in
        // the observed incident), a sibling of the main repo.
        let sess = base.join(".harness-worktrees/session-bf349dd5");
        std::fs::create_dir_all(base.join(".harness-worktrees")).unwrap();
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "session-bf349dd5",
                sess.to_str().unwrap(),
            ],
        );
        Fx {
            base,
            home,
            repo: sess,
            wt,
        }
    }

    /// Decision (`allow`/`ask`/`deny`) plus the raw stdout, judged as a Bash
    /// call from `cwd` (the session's project dir is the main repo).
    fn bash(&self, cwd: &Path, command: &str) -> (String, String) {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "wt-fp-session",
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("TMPDIR")
            .current_dir(cwd);
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

const WHOSE_TREE: &str = "not one of this session's trees";

/// CASE 1 (verbatim shape of the observed command, paths rebuilt in the
/// fixture). The target file is tracked in a linked worktree under
/// `$HOME/.condukt/worktrees`; git can restore it.
#[test]
fn case1_overwrite_of_tracked_file_in_session_created_worktree_is_not_whose_tree_refusal() {
    let fx = Fx::new("case1");
    let wt = fx.wt.display();
    let s = fx.base.join("scratch");
    std::fs::create_dir_all(&s).unwrap();
    let s = s.display();
    let cmd = format!(
        "cd {wt} && S={s} && cp crates/condukt/src/maintree.rs $S/maintree.new && \
git show HEAD:crates/condukt/src/maintree.rs > crates/condukt/src/maintree.rs && \
. \"$HOME/.cargo/env\" && cargo test -p condukt --test audit_b15_maintree_transcript_liveness \
2>&1 | grep -E \"^test |left|right\" ; cp $S/maintree.new crates/condukt/src/maintree.rs && \
git diff --stat crates/condukt/src/maintree.rs"
    );
    // Judged from the main repo (session project dir) and from the worktree.
    for cwd in [fx.repo.clone(), fx.wt.clone()] {
        let (d, out) = fx.bash(&cwd, &cmd);
        assert!(
            !out.contains(WHOSE_TREE),
            "cwd {}: a worktree under $HOME/.condukt/worktrees must be recognised as the \
session's tree, but got decision {d}: {out}",
            cwd.display()
        );
    }
}

/// CASE 3: `>` inside a single-quoted shell word is not a redirect. The path
/// is in a git worktree that is NOT one of the session's trees (a second repo
/// outside every safe root), so attribution is irrelevant: the text is data.
#[test]
fn case3_gt_path_inside_single_quoted_python_program_is_not_a_redirect() {
    let fx = Fx::new("case3");
    // Another project entirely, tracked file present.
    let other = fx.base.join("other");
    std::fs::create_dir_all(other.join("crates/condukt/src")).unwrap();
    git(&fx.home, &other, &["init", "-q", "-b", "main"]);
    std::fs::write(other.join("crates/condukt/src/maintree.rs"), "x\n").unwrap();
    git(&fx.home, &other, &["add", "crates/condukt/src/maintree.rs"]);
    git(&fx.home, &other, &["commit", "-q", "-m", "init"]);
    let target = other.join("crates/condukt/src/maintree.rs");
    let cmd = format!(
        "python3 -c 'import sys; s=\"> {}\"; print(s)'",
        target.display()
    );
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_eq!(d, "allow", "`>` inside a single-quoted word is data: {out}");
}
