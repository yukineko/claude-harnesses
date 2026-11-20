// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro of a blastguard false positive observed 2026-11-20 in a condukt
//! worker subagent (run run-20261120-071122-49691, task A) working in its own
//! task worktree `~/.condukt/worktrees/<run>-<task>` (a linked worktree its
//! parent session created with `condukt worktree create`).
//!
//! Observed command shape (paths rebuilt in the fixture):
//! `cd <wt>/crates/blastguard && python3 - <<'EOF'` … `open(p,'w').write(s)`
//! … `EOF` + a trailing `grep -rn … <wt>/crates/blastguard` line, where `p`
//! is a relative path to a tracked file. Verbatim refusal:
//! "the Python program calls `open` on `src/detect/ast/b5.rs` (judged as
//! `: > 'src/detect/ast/b5.rs'`): '> <wt>/crates/blastguard/'src/detect/ast/b5.rs''
//! destroys the file's current contents and it is inside a git work tree, but
//! not one of this session's trees (neither inside its safe roots nor a
//! recognised worktree checkout), so git being able to restore it does not
//! make the overwrite this session's to make".
//!
//! The same overwrite spelled as a shell redirect (`cd <wt> && echo a >
//! README.md`) is Allow (cd_and_chain_equivalence.rs), and a linked worktree
//! under `$HOME/.condukt/worktrees` is the session's tree (user ruling
//! 2026-10-03). The RED asserts only that the whose-tree refusal is not the
//! verdict; the control pins that the same program aimed at a different
//! project's tracked file still is not Allow.

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

fn py_heredoc_edit(dir: &str, rel: &str, trailer: &str) -> String {
    format!(
        "cd {dir} && python3 - <<'EOF'\np='{rel}'\ns=open(p).read()\n\
open(p,'w').write(s.replace('a','b'))\nEOF\n{trailer}"
    )
}

/// RED: the observed shape (trailing `grep` line included) inside a linked
/// worktree under `$HOME/.condukt/worktrees`, judged from the session's tree.
#[test]
#[ignore = "backlog 94b93e95: python3 - heredoc open(rel,'w') after cd into a condukt worktree gets the whose-tree refusal (FP)"]
fn python_heredoc_relative_open_write_in_condukt_worktree_is_not_whose_tree_refusal() {
    let fx = Fx::new("pyheredoc");
    let dir = fx.wt.join("crates/condukt");
    let dir = dir.display().to_string();
    for trailer in [format!("grep -rn b {dir}"), String::new()] {
        let cmd = py_heredoc_edit(&dir, "src/maintree.rs", &trailer);
        let (d, out) = fx.bash(&fx.repo, &cmd);
        assert!(
            !out.contains(WHOSE_TREE),
            "a tracked file in a linked worktree under $HOME/.condukt/worktrees is the \
session's tree, but {cmd:?} got decision {d}: {out}"
        );
    }
}

/// Twin of the RED with the shell spelling of the same overwrite: must not
/// get the whose-tree refusal either (pins that the fixture recognises the
/// worktree at all, so the RED is not vacuous).
#[test]
fn control_shell_redirect_in_condukt_worktree_is_not_whose_tree_refusal() {
    let fx = Fx::new("shtwin");
    let dir = fx.wt.join("crates/condukt");
    let cmd = format!("cd {} && echo a > src/maintree.rs", dir.display());
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert!(!out.contains(WHOSE_TREE), "{cmd:?} got decision {d}: {out}");
}

/// Anti-vacuity: the same program aimed at another project's tracked file
/// (outside every safe root and every worktree storage root) stays not Allow.
#[test]
fn control_python_heredoc_open_write_in_other_project_stays_not_allow() {
    let fx = Fx::new("pyother");
    let other = fx.base.join("other");
    std::fs::create_dir_all(other.join("crates/condukt/src")).unwrap();
    git(&fx.home, &other, &["init", "-q", "-b", "main"]);
    std::fs::write(other.join("crates/condukt/src/maintree.rs"), "a\n").unwrap();
    git(&fx.home, &other, &["add", "crates/condukt/src/maintree.rs"]);
    git(&fx.home, &other, &["commit", "-q", "-m", "init"]);
    let dir = other.join("crates/condukt");
    let cmd = py_heredoc_edit(&dir.display().to_string(), "src/maintree.rs", "");
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_ne!(d, "allow", "{cmd:?} must not be Allow: {out}");
}
