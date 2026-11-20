// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]
//! RED repro of a blastguard false positive observed 2026-11-20: a
//! single-quoted heredoc commit message (`git commit -F - <<'EOF'`) whose
//! prose quotes shell text in backtick spans (`cd <wt> && echo a > F`) was
//! refused with "'> <wt>/F' destroys the file's current contents and
//! `<wt>/F` is a relative path and blastguard does not know which directory
//! it is relative to, so it cannot tell whether these bytes are recoverable".
//! Fixture: the linked-worktree layout of
//! backlog_worktree_redirect_and_sq_fp_20261107.rs.

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

/// The commit message body observed 2026-11-20 (verbatim apart from the
/// leading/trailing lines): its backtick spans quote shell text, including a
/// `<<'EOF'` opener and a `> F` redirect, as prose.
const BODY: &str = "blastguard tests 0.2.774: RED — python heredoc open(rel,'w') after cd into a condukt worktree gets the whose-tree refusal

Observed 2026-11-20 in a condukt worker subagent editing its own task
worktree under ~/.condukt/worktrees: `cd <wt>/crates/blastguard && python3 -
<<'EOF' … open(p,'w') … EOF` was denied as \"not one of this session's
trees\", while the shell spelling `cd <wt> && echo a > F` is Allow. Two
controls pin the fixture (shell twin not refused) and the anti-vacuity
side (another project's tracked file stays not Allow).
";

fn commit(body: &str) -> String {
    format!("git commit -q -F - <<'EOF'\n{body}\nEOF")
}

/// RED: a single-quoted heredoc commit message is data; prose inside it must
/// not be judged as a redirect.
#[test]
fn quoted_heredoc_commit_message_quoting_shell_prose_is_allow() {
    let fx = Fx::new("cmsg");
    let cmd = commit(BODY);
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_eq!(d, "allow", "{cmd:?}: {out}");
}

/// Control: the same message without the backtick span holding `> F` is
/// Allow (pins that the commit itself is allowed in this fixture).
#[test]
fn control_same_message_without_redirect_prose_is_allow() {
    let fx = Fx::new("cmsgctl");
    let body = BODY.replace("`cd <wt> && echo a > F`", "the shell spelling");
    let cmd = commit(&body);
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_eq!(d, "allow", "{cmd:?}: {out}");
}

/// Anti-vacuity: a real redirect after the heredoc still is not Allow when
/// its target is relative to an unknown directory.
#[test]
fn control_real_relative_redirect_after_commit_heredoc_stays_not_allow() {
    let fx = Fx::new("cmsgreal");
    let cmd = format!("{}\ncd \"$X\" && echo a > F", commit("msg"));
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_ne!(d, "allow", "{cmd:?}: {out}");
}

/// RED: the same message followed by a trailing command line (the observed
/// shape ended with `git log --oneline -1`).
#[test]
fn commit_message_quoting_shell_prose_then_trailing_command_is_allow() {
    let fx = Fx::new("cmsgtrail");
    let cmd = format!("{}\ngit log --oneline -1", commit(BODY));
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_eq!(d, "allow", "{cmd:?}: {out}");
}

/// The exact observed chain (`cd <dir> && sed -i … && git add … && git commit
/// -q -F - <<'EOF' 2>&1 | grep …`, body, `EOF`, `git log --oneline -1`).
fn observed_chain(dir: &str, body: &str) -> String {
    format!(
        "cd {dir} && sed -i '' '6s/version = \"0.2.773\"/version = \"0.2.774\"/' \
crates/blastguard/Cargo.toml && git add crates/blastguard/Cargo.toml && git commit -q -F - \
<<'EOF' 2>&1 | grep -E 'BLOCK|\\(exit|weakening'\n{body}\nEOF\ngit log --oneline -1"
    )
}

/// RED candidate: the exact observed chain.
#[test]
#[ignore = "backlog 454f80ef: quoted git commit -F - heredoc piped to grep has its message prose judged as code (FP)"]
fn observed_chain_commit_message_quoting_shell_prose_is_allow() {
    let fx = Fx::new("cmsgchain");
    let cmd = observed_chain(&fx.repo.display().to_string(), BODY);
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_eq!(d, "allow", "{cmd:?}: {out}");
}

/// Control: the observed chain with a plain message is Allow (pins that the
/// chain itself is allowed, so the RED isolates the message body). Measured
/// 2026-11-20: with only the `> F` span removed the body still gets Ask
/// ("the program calls `open` on a path blastguard cannot resolve
/// statically") from its `open(p,'w')` prose — the same body-judged-as-code
/// defect, so that variant is not a control.
#[test]
fn control_observed_chain_with_plain_message_is_allow() {
    let fx = Fx::new("cmsgchainctl");
    let cmd = observed_chain(&fx.repo.display().to_string(), "plain message\n\nbody line");
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_eq!(d, "allow", "{cmd:?}: {out}");
}

/// Control: the trailing-command shape without the `> F` prose is Allow.
#[test]
fn control_trailing_command_without_redirect_prose_is_allow() {
    let fx = Fx::new("cmsgtrailctl");
    let body = BODY.replace("`cd <wt> && echo a > F`", "the shell spelling");
    let cmd = format!("{}\ngit log --oneline -1", commit(&body));
    let (d, out) = fx.bash(&fx.repo, &cmd);
    assert_eq!(d, "allow", "{cmd:?}: {out}");
}
