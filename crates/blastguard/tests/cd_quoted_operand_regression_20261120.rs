#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression (fix f9243a7bd, blastguard 0.2.795 "judge quoted operands after
//! cd by their dequoted word"): after a `cd <literal>`, a destructive command
//! whose program name and/or operands are QUOTED must be judged by the
//! dequoted words — `'rm' '-rf' '/'` is `rm -rf /`.
//!
//! Written by an independent test author (not the fix implementer;
//! CLAUDE.md §2(a)). RED observed against a binary built at f9243a7bd^ (see
//! the commit message); GREEN at main.
//!
//! Fixture: same location rule as `s6_4_truncate_retire_20261120.rs`
//! (`support/neutral_base.rs`): `<base>/src/proj` git main tree =
//! `CLAUDE_PROJECT_DIR`, `<base>/src/.harness-worktrees/wt-own` linked
//! worktree = session cwd, `<base>/home` = hook `HOME`, `<base>/tmpdir` =
//! hook `TMPDIR`, and a scratch dir strictly inside the fixed temp root with
//! `junk.txt`. Nothing is executed; the hook only judges the text.
//!
//! DOES NOT PROVE: the deny REASON; shapes other than the ones listed; the
//! verdict without a leading `cd`.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/fixed_tmp.rs"]
mod fixed_tmp;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    tmp: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

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

fn write(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("cd-quoted-{tag}-{n}"));
    let tmp = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("bg-cd-quoted-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&tmp);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base, tmp };
    let repo = fx.repo();
    write(&repo.join("t.txt"), "a\n");
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "own"],
    );
    write(&fx.tmp.join("junk.txt"), "a\n");
    fx
}

/// Run the hook once with the session cwd in `wt-own`; `(decision, reason)`.
/// A non-zero exit panics; only then is an empty stdout read as Allow.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.wt();
    let mut p = serde_json::json!({
        "session_id": "cd-quoted-operand-regression-20261120",
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
        .env("CLAUDE_PROJECT_DIR", fx.repo())
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

/// `"main/subagent"` for the command (`{T}` = the scratch dir).
fn measure(fx: &Fx, template: &str) -> (String, String, String) {
    let c = template.replace("{T}", fx.tmp.to_str().unwrap());
    let (m, rm) = run_as(fx, &c, false);
    let (s, rs) = run_as(fx, &c, true);
    (format!("{m}/{s}"), rm, rs)
}

fn expect(tag: &str, template: &str, want: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, template);
    assert_eq!(
        got, want,
        "{template:?}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

#[test]
fn quoted_rm_rf_root_after_cd_and() {
    expect("rm-root-and", "cd /tmp && 'rm' '-rf' '/'", "deny/deny");
}

#[test]
fn quoted_rm_rf_root_after_cd_semicolon() {
    expect("rm-root-semi", "cd /tmp; 'rm' '-rf' '/'", "deny/deny");
}

#[test]
fn quoted_rm_rf_usr_after_cd() {
    expect("rm-usr", "cd /tmp && 'rm' '-rf' '/usr'", "deny/deny");
}

#[test]
fn double_quoted_operands_rm_rf_root_after_cd() {
    expect("rm-dq", "cd /tmp && rm \"-rf\" \"/\"", "deny/deny");
}

#[test]
fn quoted_unlink_etc_passwd_after_cd() {
    expect("unlink", "cd /tmp && unlink '/etc/passwd'", "deny/deny");
}

#[test]
fn quoted_rmdir_usr_after_cd() {
    expect("rmdir", "cd /tmp && rmdir '/usr'", "deny/deny");
}

#[test]
fn quoted_chmod_etc_passwd_after_cd() {
    expect(
        "chmod",
        "cd /tmp && 'chmod' '777' '/etc/passwd'",
        "deny/deny",
    );
}

/// Control: a quoted relative operand under a scratch cd target is allowed.
#[test]
fn control_quoted_rm_in_scratch_dir_is_allowed() {
    expect("ctl-scratch", "cd {T} && rm -f 'junk.txt'", "allow/allow");
}
