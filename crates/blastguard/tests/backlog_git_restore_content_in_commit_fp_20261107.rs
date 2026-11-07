// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-07): `git restore --staged --worktree -- <file>` is denied
//! with "git restore discards working-tree changes" (detect/git.rs `"restore"`
//! arm) even when the file's worktree AND index content is byte-identical to a
//! blob already reachable from a commit (here: another branch), i.e. nothing
//! is lost. Observed in a subagent: the user approved the restore and
//! blastguard still blocked it twice.
//!
//! Two readings, undetermined (decide before fixing):
//! (a) false positive: the rule should look at blob reachability (content in
//!     any commit => restore is reversible => Allow), like the other
//!     reversibility probes (reversible.rs);
//! (b) intended by design: the rule classifies the command shape only, never
//!     the blast radius, and the user must run it themselves.
//! This test expresses (a). The control pins that a restore over content that
//! exists in NO commit stays blocked, so a blanket Allow cannot pass.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::{Path, PathBuf};
use std::process::Stdio;

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

fn fresh_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-restore-fp-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join("a.sh"), "base\n").unwrap();
    git(&dir, &["add", "a.sh"]);
    git(&dir, &["commit", "-qm", "base"]);
    dir
}

fn verdict(cwd: &Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = std::process::Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?})"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let d = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let r = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (d, r)
}

#[test]
#[ignore = "RED: git restore of content already in a commit is denied 2026-11-07"]
fn restore_of_content_already_in_a_commit_is_allowed() {
    let dir = fresh_repo("incommit");
    // Commit the "work" content on another branch, then return to main and make
    // worktree + index hold exactly that blob.
    git(&dir, &["checkout", "-q", "-b", "other"]);
    std::fs::write(dir.join("a.sh"), "work v2\n").unwrap();
    git(&dir, &["commit", "-qam", "work"]);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("a.sh"), "work v2\n").unwrap();
    git(&dir, &["add", "a.sh"]);
    let cmd = format!(
        "cd {} && git restore --staged --worktree -- a.sh",
        dir.display()
    );
    let (d, r) = verdict(&dir, &cmd);
    assert_eq!(
        d, "allow",
        "{cmd:?} -> {d} ({r}); content exists in commit on `other`"
    );
}

#[test]
fn control_restore_of_content_in_no_commit_stays_blocked() {
    let dir = fresh_repo("nocommit");
    std::fs::write(dir.join("a.sh"), "never committed anywhere\n").unwrap();
    git(&dir, &["add", "a.sh"]);
    let cmd = format!(
        "cd {} && git restore --staged --worktree -- a.sh",
        dir.display()
    );
    let (d, r) = verdict(&dir, &cmd);
    assert!(
        d == "deny" || d == "ask",
        "{cmd:?} must stay blocked (content in no commit), got {d} ({r})"
    );
}
