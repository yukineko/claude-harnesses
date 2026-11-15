//! User ruling 2026-11-08 (worktree trust inheritance removed, CLAUDE.md
//! section 3): an UNTRUSTED root that carries a `.precommit-audit.toml` makes
//! the audit REFUSE (the mode's fail exit: 2 in stop mode, 1 in precommit mode)
//! naming the file; it used to run "built-in checks only" and could pass.
//!
//! Written by an independent test author (CLAUDE.md section 2(a)). Drives the
//! real binary with a sandbox HOME; the real trust list is never touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("pca-untrusted-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// (repo, sandbox home). The repo has one commit and, when `with_config`, a
/// project `.precommit-audit.toml` (checks off => a clean tree audits clean).
fn fixture(tag: &str, with_config: bool) -> (PathBuf, PathBuf) {
    let base = unique_dir(tag);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("README.md"), "# repo\n").unwrap();
    if with_config {
        std::fs::write(
            repo.join(".precommit-audit.toml"),
            "[checks]\nlinters = false\nmissing_test = false\n",
        )
        .unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    (repo, home)
}

fn trust(home: &Path, root: &Path) {
    let d = home.join(".harness");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("trust.toml"),
        format!("trusted = [{:?}]\n", root.to_string_lossy()),
    )
    .unwrap();
}

/// Run the binary; stop mode feeds a Stop hook payload, precommit mode none.
fn run(repo: &Path, home: &Path, mode: &str, event: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_precommit-audit");
    let mut child = harness_core::git_env::scrub(&mut Command::new(bin))
        .args(["--mode", mode, "--root"])
        .arg(repo)
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("HARNESS_SANDBOX_HOME")
        .env_remove("HARNESS_TRUST_ALL")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(format!("{{\"hook_event_name\":\"{event}\"}}").as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn untrusted_with_config_refuses_in_stop_mode_with_exit_2() {
    let (repo, home) = fixture("stop", true);
    let (code, err) = run(&repo, &home, "stop", "Stop");
    assert_eq!(
        code, 2,
        "stop-mode refusal is the fail exit 2; stderr: {err}"
    );
    assert!(
        err.contains(".precommit-audit.toml"),
        "must name file: {err}"
    );
    assert!(
        err.contains("precommit-audit trust"),
        "must name remedy: {err}"
    );
}

#[test]
fn untrusted_with_config_refuses_in_precommit_mode_with_exit_1() {
    let (repo, home) = fixture("pre", true);
    let (code, err) = run(&repo, &home, "precommit", "Stop");
    assert_eq!(code, 1, "precommit-mode refusal is exit 1; stderr: {err}");
    assert!(
        err.contains(".precommit-audit.toml"),
        "must name file: {err}"
    );
}

/// Control: no project config => today's behaviour (clean tree audits clean).
#[test]
fn untrusted_without_config_is_unchanged() {
    let (repo, home) = fixture("noconf", false);
    assert!(!repo.join(".precommit-audit.toml").exists());
    let (code, err) = run(&repo, &home, "precommit", "Stop");
    assert_eq!(code, 0, "no config => no refusal; stderr: {err}");
    assert!(!err.contains("REFUSING"), "{err}");
    let (code, err) = run(&repo, &home, "stop", "Stop");
    assert_eq!(code, 0, "no config => no refusal (stop); stderr: {err}");
}

/// Anti-vacuity control: the SAME fixture with the root LISTED in the trust
/// file audits clean, so the refusals above are caused by missing trust alone.
#[test]
fn trusted_with_same_config_audits_clean() {
    let (repo, home) = fixture("trusted", true);
    trust(&home, &repo);
    let (code, err) = run(&repo, &home, "precommit", "Stop");
    assert_eq!(code, 0, "trusted root must audit; stderr: {err}");
    assert!(!err.contains("REFUSING"), "{err}");
}

/// A linked worktree of a LISTED main tree is itself untrusted (no
/// inheritance) and refuses.
#[test]
fn linked_worktree_of_trusted_main_with_config_refuses() {
    let (main, home) = fixture("wt", true);
    trust(&home, &main);
    let wt = main.parent().unwrap().join("linked");
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"],
    );
    assert!(wt.join(".git").is_file(), "apparatus: linked worktree");
    assert!(wt.join(".precommit-audit.toml").exists(), "apparatus: cfg");
    let (code, err) = run(&wt, &home, "precommit", "Stop");
    assert_eq!(code, 1, "worktree must not inherit trust; stderr: {err}");
    assert!(err.contains(".precommit-audit.toml"), "{err}");
}
