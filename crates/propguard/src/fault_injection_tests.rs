//! Fault-injection fail-closed check for propguard's Stop verdict (backlog
//! 8696dd7e, slice 2 adopter #2).
//!
//! Path under test: `gate::evaluate` step 3 -> `git::changed_files` ->
//! `scan_changed` -> `run_git_bin` -> `boundary::run_with_timeout`. A scan the
//! boundary could not complete is `ChangeScan::Failed`, which must resolve to
//! `decide_scan_failed`'s `git-scan-failed` BLOCK, never an allow.
//!
//! done_criteria are supplied INLINE through `Config` so that no boundary file
//! read has to succeed before the git scan is reached.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::config::Config;
use crate::gate::{evaluate, Decision};
use crate::git::{changed_files, ChangeScan};
use crate::state::SessionState;
use harness_core::boundary::fault::{with_fault_plan, Entry, FaultPlan};
use harness_core::boundary::{run_with_timeout, CommandOutput};
use harness_core::degrade::{assert_fails_closed, Permissiveness};
use harness_core::verdict::Determination;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// `Decision` scored for `degrade`: `Allow` is permissive, `Block` is not.
struct Verdict(Decision);

impl std::fmt::Debug for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", show(&self.0))
    }
}

impl Permissiveness for Verdict {
    fn permissiveness(&self) -> u8 {
        match self.0 {
            Decision::Allow { .. } => 1,
            Decision::Block { .. } => 0,
        }
    }
}

/// Subprocesses go through the typed boundary (raw-io ratchet counts `.output()`).
fn run(cmd: &mut Command) -> CommandOutput {
    match run_with_timeout(cmd, Duration::from_secs(120)) {
        Determination::Known(o) => o,
        Determination::Undetermined(_) => panic!("fixture subprocess undetermined"),
    }
}

fn git(dir: &Path, args: &[&str]) {
    let o = run(Command::new("git").args(args).current_dir(dir));
    assert_eq!(o.code(), 0, "git {args:?}: {}", o.stderr());
}

const FIXTURE_FILE: &str = "src/app.rs";

/// A real repo with one commit and an uncommitted, checkable source file.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "-q", "-b", "main"]);
    git(p, &["config", "user.email", "t@t.com"]);
    git(p, &["config", "user.name", "t"]);
    std::fs::write(p.join("README.md"), "seed\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "seed"]);
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join(FIXTURE_FILE), "pub fn f() -> u32 { 1 }\n").unwrap();
    dir
}

fn config(state_dir: &Path) -> Config {
    Config {
        done_criteria: "Parsing must reject malformed input. Never panic on empty input. \
                        The result must be deterministic."
            .to_string(),
        state_dir: state_dir.to_path_buf(),
        ..Config::default()
    }
}

/// `Decision` has no `Debug` (gate.rs is out of this task's scope).
fn show(d: &Decision) -> String {
    match d {
        Decision::Allow { tag, attempts, .. } => format!("Allow{{tag:{tag}, attempts:{attempts}}}"),
        Decision::Block {
            tag,
            reason,
            attempts,
            ..
        } => {
            format!("Block{{tag:{tag}, attempts:{attempts}, reason:{reason}}}")
        }
    }
}

fn tag(d: &Decision) -> &'static str {
    match d {
        Decision::Allow { tag, .. } | Decision::Block { tag, .. } => tag,
    }
}

#[test]
fn control_unfaulted_scan_reaches_the_files_and_not_the_scan_failed_block() {
    let repo = fixture();
    let state = tempfile::tempdir().unwrap();
    let cfg = config(state.path());

    let run = with_fault_plan(FaultPlan::none(), || {
        let scan = changed_files(repo.path());
        let decision = evaluate(&cfg, repo.path(), &SessionState::default());
        (scan, decision)
    });
    assert_eq!(run.injected, 0, "FaultPlan::none() must inject nothing");
    let (scan, decision) = run.value;
    match scan {
        ChangeScan::Files(v) => assert!(
            v.iter().any(|f| f == FIXTURE_FILE),
            "control: the fixture path must be in the changed set, got {v:?}"
        ),
        other => panic!("control: expected Files, got {other:?}"),
    }
    assert!(
        !tag(&decision).starts_with("git-scan-failed"),
        "control: an intact scan must not take the scan-failed arm: {}",
        show(&decision)
    );
}

#[test]
fn blind_boundary_never_leaves_the_stop_decision_permissive() {
    let repo = fixture();
    let state = tempfile::tempdir().unwrap();
    let cfg = config(state.path());
    assert_fails_closed(|| Verdict(evaluate(&cfg, repo.path(), &SessionState::default())));
}

#[test]
fn faulted_run_with_timeout_resolves_to_the_scan_failed_block() {
    let repo = fixture();
    let state = tempfile::tempdir().unwrap();
    let cfg = config(state.path());

    let run = with_fault_plan(FaultPlan::only(Entry::RunWithTimeout), || {
        evaluate(&cfg, repo.path(), &SessionState::default())
    });
    assert!(
        run.injected >= 1,
        "the git scan must have hit the faulted seam (injected = {})",
        run.injected
    );
    assert!(
        matches!(
            run.value,
            Decision::Block {
                tag: "git-scan-failed",
                ..
            }
        ),
        "a faulted scan must be the decide_scan_failed block, got {}",
        show(&run.value)
    );
}
