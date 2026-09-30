//! backlog 4cacdfae: a LINKED git worktree whose main working tree is in the
//! trust store must honor its own `.precommit-audit.toml`. Exact-match
//! `is_trusted` does not. End-to-end against the real binary with a private
//! `HOME` (trust store) so no process-global env is mutated here.
//!
//! Observable: a custom `[[rule]]` blocks the commit (exit 1) only when the
//! project config is honored; when it is ignored the audit is clean (exit 0)
//! and stderr says the config "is not trusted".
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

const CFG: &str = r#"
[checks]
linters = false
missing_test = false

[[rule]]
id = "no-todo-fixme"
pattern = 'TODO|FIXME'
include_globs = ["src/**"]
message = "Resolve TODO/FIXME before committing."
"#;

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// (main, linked worktree). Config is committed so it exists in both; each
/// checkout then gets an untracked `src/a.conf` with a TODO for the rule to hit.
fn fixture(base: &Path) -> (PathBuf, PathBuf) {
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["config", "user.email", "t@t.com"]);
    git(&main, &["config", "user.name", "t"]);
    std::fs::write(main.join(".precommit-audit.toml"), CFG).unwrap();
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "seed"]);
    let wt = base.join("wt");
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"],
    );
    assert!(wt.join(".git").is_file(), "apparatus: linked worktree");
    for d in [&main, &wt] {
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.conf"), "key = value  # TODO later\n").unwrap();
    }
    (main, wt)
}

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_precommit-audit"));
    c.env_remove("HARNESS_TRUST_ALL");
    c
}

fn trust(root: &Path, home: &Path) {
    let o = bin()
        .arg("trust")
        .arg("--root")
        .arg(root)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "trust: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// (exit code, stderr) of a precommit-mode audit of `root`.
fn audit(root: &Path, home: &Path) -> (i32, String) {
    let o = bin()
        .args(["--mode", "precommit", "--root"])
        .arg(root)
        .current_dir(root)
        .env("HOME", home)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn linked_worktree_of_trusted_main_honors_project_config() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (main, wt) = fixture(base.path());
    trust(&main, home.path());
    let (code, err) = audit(&main, home.path());
    assert_eq!(
        code, 1,
        "apparatus: trusted main honors the rule; stderr: {err}"
    );
    let (code, err) = audit(&wt, home.path());
    assert_eq!(
        code, 1,
        "a linked worktree of a TRUSTED main must honor its project config; stderr: {err}"
    );
    assert!(
        !err.contains("not trusted"),
        "must not warn untrusted; stderr: {err}"
    );
}

#[test]
fn linked_worktree_of_untrusted_main_still_ignores_project_config() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_main, wt) = fixture(base.path());
    let (code, err) = audit(&wt, home.path());
    assert_eq!(
        code, 0,
        "untrusted worktree config must be ignored; stderr: {err}"
    );
    assert!(err.contains("not trusted"), "should warn; stderr: {err}");
}

#[test]
fn trusted_plain_checkout_still_honors_project_config() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (main, _wt) = fixture(base.path());
    trust(&main, home.path());
    let (code, err) = audit(&main, home.path());
    assert_eq!(
        code, 1,
        "trusted plain checkout honors the rule; stderr: {err}"
    );
}
