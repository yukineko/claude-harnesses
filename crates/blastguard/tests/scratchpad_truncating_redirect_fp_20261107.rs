#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (blastguard false positive observed 2026-11-07). A session running
//! in a linked git worktree (`<parent>/.harness-worktrees/session-X`) ran
//!
//!   cd <wt> && git commit -q -m ... -m ... > <scratchpad>/mergemain.log 2>&1; echo rc=$?; grep -E "blocked" <scratchpad>/mergemain.log; git log --oneline -1; git merge-base --is-ancestor main session-X; echo is-ancestor=$?
//!
//! where `<scratchpad>` is the session's own
//! `/private/tmp/claude-<uid>/<project-slug>/<session-id>/scratchpad`, and got an
//! Ask: "truncating redirect is confined to /private/tmp ...". The same shape was
//! Allow earlier in the session (before blastguard 0.2.377). User ruling
//! 2026-10-28 (tests/tmp_truncation_recoverable.rs): a truncating redirect inside
//! /private/tmp (incl. the Claude scratchpad) is recoverable work and must Allow.
//!
//! Fixture: fake HOME, a repo and a real linked worktree under
//! `<base>/src/.harness-worktrees`, and a fake scratchpad of the same shape under
//! /private/tmp. The verdict is asserted, never message text.

#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    wt: PathBuf,
    scratch: PathBuf,
    tmp_root: PathBuf,
    session: String,
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
        let uniq = format!("{}-{name}", std::process::id());
        let base = neutral_base::neutral_base()
            .join("scratchpad_trunc_fp")
            .join(&uniq);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.join("home");
        let repo = base.join("src/harness");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&home, &repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&home, &repo, &["add", "a.txt"]);
        git(&home, &repo, &["commit", "-q", "-m", "init"]);
        let wts = base.join("src/.harness-worktrees");
        std::fs::create_dir_all(&wts).unwrap();
        let wt = wts.join("session-bf349dd5");
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "session-bf349dd5",
                wt.to_str().unwrap(),
            ],
        );
        let tmp_root = PathBuf::from(format!("/private/tmp/bg-scratch-fp-{uniq}"));
        let _ = std::fs::remove_dir_all(&tmp_root);
        let session = "bf349dd5-8e0c-4418-af43-0de710b93593".to_string();
        let scratch = tmp_root
            .join("claude-502/-Users-yuki-src-harness")
            .join(&session)
            .join("scratchpad");
        std::fs::create_dir_all(&scratch).unwrap();
        Fx {
            base: std::fs::canonicalize(&base).unwrap(),
            home,
            wt,
            scratch,
            tmp_root,
            session,
        }
    }

    fn run(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": self.session,
            "cwd": self.wt.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.wt)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        println!("[{command}] => {:?}", s.trim());
        s
    }

    fn command(&self) -> String {
        let log = self.scratch.join("mergemain.log");
        let log = log.display();
        format!(
            "cd {wt} && git commit -q -m \"Merge branch 'main' into session-bf349dd5\" -m \"Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\" > {log} 2>&1; echo rc=$?; grep -E \"blocked\" {log}; git log --oneline -1; git merge-base --is-ancestor main session-bf349dd5; echo is-ancestor=$?",
            wt = self.wt.display()
        )
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp_root);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[track_caller]
fn assert_allow(case: &str, out: &str) {
    assert!(
        out.trim().is_empty(),
        "{case}: must be Allow (no output); got {out}"
    );
}

/// Control (passes today): the exact command, log file not yet present.
#[test]
fn control_scratchpad_log_redirect_new_file_allows() {
    let f = Fx::new("new");
    let out = f.run(&f.command());
    assert_allow("new log", &out);
}

/// Control (passes today): the exact command, log file left over from an
/// earlier run and spelled with the SAME case.
#[test]
fn control_scratchpad_log_redirect_existing_same_case_allows() {
    let f = Fx::new("existing");
    std::fs::write(f.scratch.join("mergemain.log"), "old\n").unwrap();
    let out = f.run(&f.command());
    assert_allow("existing log, same case", &out);
}

/// ROOT CAUSE (measured 2026-11-07 with the real session scratchpad): the log
/// already on disk was named `mergeMain.log` (capital M) and the command
/// spelled `mergemain.log`. On a case-insensitive filesystem (default APFS)
/// those are the same file; blastguard's `SafeRoots::no_symlink_below` compares
/// the realpath of each spelled prefix with `<parent real>/<typed name>`,
/// reads the case difference as "a symlink below the root", and
/// `classify_temp` answers Undetermined, so `temp_disposable_redirect_target`
/// is false and the confined Ask stands. A debug build printed
/// `cls=Undetermined(... reaches /private/tmp through a symlink below the root
/// ...)` for `mergemain.log` and Allow for every other name, incl. a fresh
/// copy with the same bytes. A case-only spelling difference is not a symlink.
///
/// On a case-sensitive filesystem `mergemain.log` is a different, absent file,
/// which is Allow, so this test passes there; it bites on the dev machines.
#[test]
#[ignore = "blastguard FP: RED, remove ignore when fixed"]
fn scratchpad_log_redirect_existing_file_with_case_only_difference_allows() {
    let f = Fx::new("case");
    std::fs::write(f.scratch.join("mergeMain.log"), "old\n").unwrap();
    let out = f.run(&f.command());
    assert_allow("existing log, on-disk case differs from typed case", &out);
}
