//! backlog 4cacdfae: a LINKED git worktree whose main working tree is in the
//! trust store must load its own project `propguard.toml`. Exact-match
//! `is_trusted` does not, so these tests pin worktree inheritance plus the two
//! controls.
//!
//! The existing config tests serialize HOME mutation through a lock private to
//! `config::tests`, which this module cannot reach. Rather than race them, each
//! scenario re-executes this test binary as a child with its own `HOME`, so the
//! parent process never mutates global env.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::config::Config;
use harness_core::boundary::{run_with_timeout, CommandOutput};
use harness_core::verdict::Determination;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Every subprocess goes through the typed boundary (the raw-io ratchet counts
/// a bare `.output()` in a gate crate's `src/`, test module or not).
fn run(cmd: &mut Command) -> Result<CommandOutput, String> {
    match run_with_timeout(cmd, Duration::from_secs(120)) {
        Determination::Known(o) => Ok(o),
        Determination::Undetermined(_) => Err("subprocess outcome undetermined".to_string()),
    }
}

const PROJECT_TOML: &str = "checker_cmd = \"wt-checker\"\n";
const DEFAULT_CMD: &str = "claude -p";

fn git(dir: &Path, args: &[&str]) {
    let o = run(Command::new("git").args(args).current_dir(dir)).expect("git runs");
    assert_eq!(o.code(), 0, "git {args:?}: {}", o.stderr());
}

fn fixture(base: &Path) -> (PathBuf, PathBuf) {
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["config", "user.email", "t@t.com"]);
    git(&main, &["config", "user.name", "t"]);
    std::fs::write(Config::project_path(&main), PROJECT_TOML).unwrap();
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-qm", "seed"]);
    let wt = base.join("wt");
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"],
    );
    assert!(wt.join(".git").is_file(), "apparatus: linked worktree");
    assert!(
        Config::project_path(&wt).exists(),
        "apparatus: config in worktree"
    );
    (main, wt)
}

/// Child half: trust `TRUSTWT_TRUST` (if set) in the child's own `HOME`, load
/// the config for `TRUSTWT_ROOT`, print the observed value on a marker line.
#[test]
fn child_probe() {
    let Ok(root) = std::env::var("TRUSTWT_ROOT") else {
        return; // not a child invocation
    };
    if let Ok(t) = std::env::var("TRUSTWT_TRUST") {
        harness_core::trust::add(Path::new(&t)).unwrap();
    }
    println!(
        "TRUSTWT_OBSERVED={}",
        Config::load(Path::new(&root)).checker_cmd
    );
}

/// Parent half: returns the `checker_cmd` the child observed.
fn observe(root: &Path, trust: Option<&Path>, home: &Path) -> String {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "trust_worktree_tests::child_probe",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ])
    .env("HOME", home)
    .env("TRUSTWT_ROOT", root)
    .env_remove("HARNESS_TRUST_ALL");
    if let Some(t) = trust {
        cmd.env("TRUSTWT_TRUST", t);
    }
    let out = run(&mut cmd).expect("child runs");
    let (code, stderr) = (out.code(), out.stderr().to_string());
    assert_eq!(code, 0, "child failed: {stderr}");
    let Determination::Known(stdout) = out.stdout_on_success() else {
        return "<undetermined>".to_string();
    };
    stdout
        .lines()
        .find_map(|l| {
            l.split_once("TRUSTWT_OBSERVED=")
                .map(|(_, v)| v.to_string())
        })
        .expect("child printed no TRUSTWT_OBSERVED line")
}

#[test]
fn linked_worktree_of_trusted_main_loads_project_config() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (main, wt) = fixture(base.path());
    assert_eq!(
        observe(&main, Some(&main), home.path()),
        "wt-checker",
        "apparatus: main loads"
    );
    assert_eq!(
        observe(&wt, Some(&main), home.path()),
        "wt-checker",
        "a linked worktree of a TRUSTED main must load its project propguard.toml"
    );
}

#[test]
fn linked_worktree_of_untrusted_main_still_ignores_project_config() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_main, wt) = fixture(base.path());
    assert_eq!(observe(&wt, None, home.path()), DEFAULT_CMD);
}

#[test]
fn trusted_plain_checkout_still_loads_project_config() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (main, _wt) = fixture(base.path());
    assert_eq!(observe(&main, Some(&main), home.path()), "wt-checker");
}
