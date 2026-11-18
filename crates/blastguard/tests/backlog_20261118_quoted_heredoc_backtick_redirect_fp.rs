#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog (2026-11-18): blastguard false positive observed live twice (a
//! subagent session, then main relaying the exact command). The command
//! below commits with `git commit -F -` and a QUOTED `<<'EOF'` here-document
//! as the message; the body is data, never executed. It was refused with:
//!
//!   '> /Users/yuki/src/.harness-worktrees/bg-s4-20261118/`' destroys the
//!   file's current contents and `/Users/yuki/src/.harness-worktrees/
//!   bg-s4-20261118/`` contains a shell expansion, so blastguard cannot tell
//!   which file this names, let alone whether its contents are recoverable
//!
//! A backtick-and-`>` sequence inside the quoted body is read as a redirect
//! whose target holds a shell expansion. The RED pins that the exact command
//! is not Deny in main mode, with cwd a temp git repo.
//!
//! Control: a REAL `> "$x"` redirect (a target with an expansion) is still
//! not Allow.
//!
//! DOES NOT PROVE: the subagent-mode verdict; which reading produced the
//! false target; unquoted here-documents (whose bodies DO expand) or other
//! quoting spellings of the delimiter.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The exact command that was blocked (verbatim).
const BLOCKED: &str = r##"cd /Users/yuki/src/.harness-worktrees/bg-s4-20261118 && p=crates/blastguard/tests/backlog_20261118_perl_regex_arrow_read_as_redirect_fp.rs && git add -- $p && git commit -q -F - -- $p <<'EOF' 2>&1 | tail -30
test(blastguard): RED for `-> Decision` in a quoted perl -e regex read as a redirect onto `\`

Live false positive in the S4 subagent session (2026-11-18): the exact
perl -0pi rewrite command is refused with "'> \' destroys the file's
current contents and it exists, and it is not inside a git work tree".
RED observed in main mode (Deny); control: a real `> \` onto an existing
file is still refused.

test-weakening-justified: ignore-added on blocked_perl_regex_rewrite_is_not_deny_in_main_mode — new RED for an unfixed FP, observed failing (Deny) before ignore

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
git log --oneline -1; git status --short | head -30"##;

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "heredoc-backtick-fp-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base }
}

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
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn decision(fx: &Fx, cwd: &Path, command: &str) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "heredoc-backtick-fp-20261118",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.base.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn repo(tag: &str) -> (Fx, PathBuf) {
    let fx = fixture(tag);
    let proj = fx.base.join("src").join("proj");
    std::fs::write(proj.join("README"), "x\n").unwrap();
    git(&proj, &["init", "-q"]);
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-q", "-m", "init"]);
    (fx, proj)
}

// test-weakening-justified: ignore-added on a new RED for an unfixed FP (backlog 2026-11-18)
#[test]
#[ignore = "RED: 2026-11-18 blastguard FP — backtick-and-> inside a quoted <<'EOF' body read as a redirect with an expansion; observed Deny"]
fn quoted_heredoc_commit_message_with_backticks_is_not_deny_in_main_mode() {
    let (fx, proj) = repo("red");
    let (d, r) = decision(&fx, &proj, BLOCKED);
    assert_ne!(d, "deny", "the quoted-heredoc commit was refused: {r}");
}

/// Control: a real redirect onto a target with an expansion is not Allow.
#[test]
fn real_redirect_with_expansion_target_not_allowed() {
    let (fx, proj) = repo("ctl");
    let (d, r) = decision(&fx, &proj, "echo x > \"$x\"");
    assert_ne!(d, "allow", "a real `> \"$x\"` redirect was allowed ({r})");
}
