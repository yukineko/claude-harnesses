#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog (2026-11-18): blastguard false positive observed live in a
//! subagent session in the S4 worktree. The exact command below (a `perl
//! -0pi -e` in-place rewrite whose quoted regex contains `-> Decision`) was
//! refused with:
//!
//!   PreToolUse:Bash hook error: '> \' destroys the file's current contents
//!   and it exists, and it is not inside a git work tree — nothing holds a
//!   second copy of these bytes
//!
//! The `->` sits inside a single-quoted `perl -e` program; no shell redirect
//! onto a file named `\` exists in the command. The RED pins that the command
//! is not Deny in main mode, run in a git repository that holds the edited
//! file (committed, clean).
//!
//! Control: a REAL `> \` redirect onto an existing file named `\` outside any
//! git work tree is still refused (not Allow).
//!
//! DOES NOT PROVE: the subagent-mode verdict (a legitimate Ask hardens to
//! Deny there); which reading produced the false `> \` target; other perl /
//! sed regex spellings with `->` or `>`.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The exact command that was blocked (verbatim).
const BLOCKED: &str = r##"cd /Users/yuki/src/.harness-worktrees/bg-s4-20261118 && f=crates/blastguard/src/detect/ast/b5.rs && perl -0pi -e 's/judge: &dyn Fn\(&str\) -> Decision\)/judge: &Probes<'"'"'_>)/g; s/judge: &dyn Fn\(&str\) -> Decision,/judge: &Probes<'"'"'_>,/g; s/let verdict = judge\(&format!/let verdict = (judge.judge)(&format!/' $f && perl -0pi -e 's/pub\(crate\) fn raise\(legacy: Decision, command: &str, judge: &Probes<.\_>\) -> Decision \{/pub(crate) fn raise(legacy: Decision, command: &str, judge: &Probes<'"'"'_>) -> Decision {/' $f && grep -n "Probes\|judge.judge" $f"##;

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
        "perl-arrow-fp-{tag}-{}",
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
        "session_id": "perl-arrow-fp-20261118",
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

// test-weakening-justified: ignore-added on a new RED for an unfixed FP (backlog 2026-11-18)
#[test]
#[ignore = "RED: 2026-11-18 blastguard FP — `-> Decision` inside a quoted perl -e regex read as a redirect onto `\\`; observed Deny"]
fn blocked_perl_regex_rewrite_is_not_deny_in_main_mode() {
    let fx = fixture("red");
    let proj = fx.base.join("src").join("proj");
    let f = proj.join("crates/blastguard/src/detect/ast/b5.rs");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(
        &f,
        "pub(crate) fn raise(legacy: Decision, command: &str, judge: &dyn Fn(&str) -> Decision) -> Decision {}\n",
    )
    .unwrap();
    git(&proj, &["init", "-q"]);
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-q", "-m", "init"]);
    let (d, r) = decision(&fx, &proj, BLOCKED);
    assert_ne!(d, "deny", "the perl regex rewrite was refused: {r}");
}

/// Control: a real `> \` onto an existing file outside any git work tree is
/// still refused.
#[test]
fn real_redirect_onto_backslash_file_still_refused() {
    let fx = fixture("ctl");
    let proj = fx.base.join("src").join("proj");
    // A file NAMED `\` (on unix `\` is not a path separator; clippy's
    // join_absolute_paths misreads it).
    #[allow(clippy::join_absolute_paths)]
    let victim = proj.join("\\");
    std::fs::write(victim, "precious\n").unwrap();
    let (d, r) = decision(&fx, &proj, "echo x > \\\\");
    assert_ne!(d, "allow", "a real `> \\` redirect was allowed ({r})");
}
