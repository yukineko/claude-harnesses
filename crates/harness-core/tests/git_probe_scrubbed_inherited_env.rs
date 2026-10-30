#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `probe_repo_scrubbed` must answer about the directory it is given, not about
//! the repository an inherited `GIT_DIR` names.
//!
//! A plain directory outside every repo is `NotRepo`. With `GIT_DIR=<B>/.git`
//! in the environment, an unscrubbed `git rev-parse --is-inside-work-tree`
//! says `true` for it. The test re-execs itself as a child that carries that
//! variable, so no `set_var` is needed in the parent process.

use harness_core::git_probe::{probe_repo_scrubbed, RepoProbe};
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_FLAG: &str = "HC_PROBE_SCRUBBED_CHILD_PLAIN";
const INNER: &str = "inner_probe_with_inherited_git_dir";

fn git(dir: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    harness_core::git_env::scrub(&mut c);
    let out = c
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Runs only inside the child (flag set); a bare no-op in a normal run.
#[test]
fn inner_probe_with_inherited_git_dir() {
    let Ok(plain) = std::env::var(CHILD_FLAG) else {
        return;
    };
    assert!(
        std::env::var_os("GIT_DIR").is_some(),
        "child must carry GIT_DIR"
    );
    let got = probe_repo_scrubbed(Path::new(&plain));
    assert_eq!(
        got,
        RepoProbe::NotRepo,
        "probe_repo_scrubbed followed the inherited GIT_DIR for a plain dir"
    );
}

#[test]
fn probe_repo_scrubbed_ignores_inherited_git_dir() {
    if std::env::var_os(CHILD_FLAG).is_some() {
        return; // the child runs the inner test only
    }
    let base: PathBuf = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join("hc-probe-scrubbed-env")
        .join(format!("{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let plain = base.join("plain");
    let repo_b = base.join("repo-b");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::create_dir_all(&repo_b).unwrap();
    git(&repo_b, &["init", "-q"]);

    let out = harness_core::git_env::scrub(&mut Command::new(std::env::current_exe().unwrap()))
        .args(["--exact", INNER, "--test-threads=1", "--nocapture"])
        .env(CHILD_FLAG, &plain)
        .env("GIT_DIR", repo_b.join(".git"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&base);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "child failed:\n{text}");
    assert!(
        text.contains("1 passed"),
        "child did not actually run the inner test:\n{text}"
    );
}
