#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Possible FP observed 2026-11-20: an append of Rust source to a tracked
//! file of the session's OWN linked worktree via a QUOTED heredoc,
//! `cd <own worktree> && cat >> crates/blastguard/src/detect/rm.rs <<'EOF'`
//! ... `EOF`, was denied with a reason that quoted `'> '` as a truncating
//! redirect whose target was an empty relative path. `>>` appends; the quoted
//! heredoc body is data.
//!
//! Written by an independent test author (CLAUDE.md §2(a)).
//!
//! Asserted: the shape (cwd = `CLAUDE_PROJECT_DIR` = own linked worktree) is
//! Allow in both modes, with and without the leading `cd`. Control: the same
//! append into `/etc/hosts` is Deny in both modes.
//!
//! Fixture: `support/neutral_base.rs` location rule; `<base>/src/proj` git
//! main tree with `crates/blastguard/src/detect/rm.rs` committed,
//! `<base>/src/.harness-worktrees/wt-own` its linked worktree (session cwd and
//! `CLAUDE_PROJECT_DIR`), `<base>/home` = hook `HOME`. Nothing is executed.
//!
//! Measured 2026-11-20 at main ec71b087e: all three pass (the FP did not
//! reproduce with this body), so they are kept as passing guards.
//!
//! DOES NOT PROVE: the exact heredoc body of the original observation (it was
//! not recorded here); verdicts with a session id; the reason text.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

const TARGET: &str = "crates/blastguard/src/detect/rm.rs";

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn repo(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn wt(&self) -> PathBuf {
        self.base
            .join("src")
            .join(".harness-worktrees")
            .join("wt-own")
    }
    fn git(&self, dir: &Path, args: &[&str]) {
        let mut cmd = git_env::pinned(dir);
        cmd.args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home());
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        assert!(
            out.status.success(),
            "fixture git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("cat-append-{tag}-{n}"));
    let _ = std::fs::remove_dir_all(&base);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base };
    let repo = fx.repo();
    let f = repo.join(TARGET);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "pub fn existing() {}\n").unwrap();
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "own"],
    );
    fx
}

/// Run the hook once (cwd = `CLAUDE_PROJECT_DIR` = own worktree).
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.wt();
    let mut p = serde_json::json!({
        "session_id": "cat-append-heredoc-rel-fp-20261120",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", fx.base.join("tmpdir"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git_env::scrub(&mut cmd);
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// A Rust test-module body with `>` in the places Rust source puts it:
/// a return arrow, a comparison, a generic close, a doc-comment blockquote
/// and a line ending in `> `.
const BODY: &str = "\
#[cfg(test)]\n\
mod appended_tests {\n\
    /// > quoted doc line\n\
    fn gt(a: u32, b: u32) -> bool {\n\
        a > b\n\
    }\n\
    fn v() -> Vec<Option<u32>> {\n\
        vec![]\n\
    }\n\
    // trailing gt > \n\
    #[test]\n\
    fn works() {\n\
        let _x: Option<u32>;\n\
        let _y: Vec<u8> = vec![]; let _z = 1;\n\
        let _f = |x: u32| x > 0 || x > 1;\n\
        assert!(gt(2, 1) && 2 > 1);\n\
        assert!(v().is_empty());\n\
    }\n\
}\n";

fn append_cmd(prefix: &str, target: &str) -> String {
    format!("{prefix}cat >> {target} <<'EOF'\n{BODY}EOF")
}

fn both(fx: &Fx, command: &str) -> (String, String, String) {
    let (m, rm) = run_as(fx, command, false);
    let (s, rs) = run_as(fx, command, true);
    (format!("{m}/{s}"), rm, rs)
}

#[test]
fn cd_own_worktree_then_cat_append_quoted_heredoc_is_allowed() {
    let fx = fixture("cd");
    let c = append_cmd(&format!("cd {} && ", fx.wt().to_str().unwrap()), TARGET);
    let (got, rm, rs) = both(&fx, &c);
    assert_eq!(
        got, "allow/allow",
        "{c:?}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

#[test]
fn cat_append_quoted_heredoc_without_cd_is_allowed() {
    let fx = fixture("nocd");
    let c = append_cmd("", TARGET);
    let (got, rm, rs) = both(&fx, &c);
    assert_eq!(
        got, "allow/allow",
        "{c:?}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

#[test]
fn control_cat_append_quoted_heredoc_to_etc_hosts_is_denied() {
    let fx = fixture("etc");
    let c = append_cmd(
        &format!("cd {} && ", fx.wt().to_str().unwrap()),
        "/etc/hosts",
    );
    let (got, rm, rs) = both(&fx, &c);
    assert_eq!(
        got, "deny/deny",
        "{c:?}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}
