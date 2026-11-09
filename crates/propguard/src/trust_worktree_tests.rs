//! backlog 4cacdfae, INVERTED by the user ruling 2026-11-08: a LINKED git
//! worktree whose main working tree is in the trust store does NOT inherit that
//! trust: it must NOT load its own project config, and the file is recorded in
//! `untrusted_project` (the gate then blocks `config-untrusted`). The two
//! controls must keep holding.
//!
//! The existing config tests serialize HOME mutation through a lock private to
//! `config::tests`, which this module cannot reach. Rather than race them, each
//! scenario re-executes this test binary as a child with its own `HOME`, so the
//! parent process never mutates global env.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    let o = run(harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir))
    .expect("git runs");
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
    let cfg = Config::load(Path::new(&root));
    println!("TRUSTWT_OBSERVED={}", cfg.checker_cmd);
    println!(
        "TRUSTWT_UNTRUSTED={}",
        cfg.untrusted_project
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
}

/// Parent half: returns the `checker_cmd` the child observed.
fn observe(root: &Path, trust: Option<&Path>, home: &Path) -> String {
    observe_both(root, trust, home).0
}

/// (`checker_cmd`, `untrusted_project` as a string, empty when `None`).
fn observe_both(root: &Path, trust: Option<&Path>, home: &Path) -> (String, String) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    harness_core::git_env::scrub(&mut cmd);
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
        return ("<undetermined>".to_string(), "<undetermined>".to_string());
    };
    let find = |key: &str| {
        stdout
            .lines()
            .find_map(|l| l.split_once(key).map(|(_, v)| v.to_string()))
            .unwrap_or_else(|| panic!("child printed no {key} line"))
    };
    (find("TRUSTWT_OBSERVED="), find("TRUSTWT_UNTRUSTED="))
}

#[test]
fn linked_worktree_of_trusted_main_does_not_inherit_and_flags_untrusted_project() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (main, wt) = fixture(base.path());
    assert_eq!(
        observe(&main, Some(&main), home.path()),
        "wt-checker",
        "apparatus: main loads"
    );
    let (cmd, untrusted) = observe_both(&wt, Some(&main), home.path());
    assert_eq!(
        cmd, DEFAULT_CMD,
        "a linked worktree of a TRUSTED main must NOT load its project config (no inheritance)"
    );
    assert!(
        untrusted.ends_with("propguard.toml") && untrusted.contains("wt"),
        "the existing-but-untrusted project file must be recorded so the gate blocks: {untrusted:?}"
    );
    assert_eq!(
        observe_both(&main, Some(&main), home.path()).1,
        "",
        "main is trusted: nothing flagged"
    );
}

#[test]
fn linked_worktree_of_untrusted_main_still_ignores_project_config() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (_main, wt) = fixture(base.path());
    let (cmd, untrusted) = observe_both(&wt, None, home.path());
    assert_eq!(cmd, DEFAULT_CMD);
    assert!(untrusted.ends_with("propguard.toml"), "{untrusted:?}");
}

#[test]
fn trusted_plain_checkout_still_loads_project_config() {
    let (home, base) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (main, _wt) = fixture(base.path());
    assert_eq!(observe(&main, Some(&main), home.path()), "wt-checker");
}
