//! Shadow-run: opt-in, manually-triggered speculative execution of the SAME
//! task under a second model, purely to produce a clean pass/fail/cost/
//! duration comparison point for fugu-router (backlog `cb2aabff`). No
//! automatic trigger is implemented — there is no API exposing how much of
//! the account's rate-limit window remains (gauge's `window` module is the
//! closest available proxy, and even that is only an approximation the user
//! registers by hand), so the decision to fire a shadow-run is always a human
//! one, gated behind the `enabled` flag below.
//!
//! The shadow worktree is ALWAYS discarded, never merged: whatever the
//! primary worker produced is what ships. Shadow-run exists only to generate
//! a clean side-by-side data point (same task, different model, no
//! confound from differing task content) for later routing analysis.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use harness_core::verdict::Determination;
use serde::{Deserialize, Serialize};

use crate::worktree;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ShadowRunFlag {
    enabled: bool,
}

fn flag_path(dir: &Path) -> PathBuf {
    dir.join("shadow_run.json")
}

/// Whether shadow-run is currently permitted. Absent/corrupt flag file →
/// disabled (fail-closed: no flag registered means the user has never opted
/// in, so shadow-run must not fire).
pub fn is_enabled(dir: &Path) -> bool {
    std::fs::read_to_string(flag_path(dir))
        .ok()
        .and_then(|t| serde_json::from_str::<ShadowRunFlag>(&t).ok())
        .map(|f| f.enabled)
        .unwrap_or(false)
}

/// Persist the enable/disable flag under `dir` (normally `config::base_dir()`).
pub fn set_enabled(dir: &Path, enabled: bool) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_string_pretty(&ShadowRunFlag { enabled })?;
    std::fs::write(flag_path(dir), text)?;
    Ok(())
}

/// Result of a finished shadow-run attempt, ready to hand to `fugu-router
/// record` for the comparison dataset.
#[derive(Debug, Clone)]
pub struct ShadowOutcome {
    pub title: String,
    pub model: String,
    pub pass: bool,
    pub cost_usd: f64,
    pub duration_secs: f64,
}

/// Which branch a `finish` must force-delete for the shadow worktree at
/// `worktree_path`, given the caller's `--run` / `--branch`.
///
/// Namespacing is a TWO-SIDED property: `exec` cuts the worktree through
/// [`worktree::create_namespaced`], so a `finish` that force-deletes the raw
/// `--branch` aims at a ref that was never created while the real, run-scoped
/// one is stranded on disk. The branch is therefore resolved against what git
/// actually registered for that directory, never against the caller's spelling
/// alone:
///
/// * cannot ask git -> REFUSE (`Undetermined` is not "there is no namespace";
///   guessing here means `git branch -D` on an unverified ref);
/// * the worktree names no branch at all -> REFUSE (nothing to discard by name);
/// * git's branch equals the caller's `--run`/`--branch` splice -> that branch;
/// * `--run` was omitted and git's branch is the SAME logical branch under some
///   run namespace -> that branch (this is the resolution the legacy CLI could
///   not express, and it is read off git, not assumed);
/// * anything else -> REFUSE with both spellings named. A namespace mismatch is
///   an observable failure, never a quiet success that strands the real branch.
fn resolve_discard_branch(
    repo: &Path,
    worktree_path: &Path,
    run: Option<&str>,
    branch: &str,
) -> Result<String> {
    let expected = worktree::run_scoped_branch(run, branch)?;
    let registered = match worktree::registered_branch(repo, worktree_path) {
        Determination::Known(b) => b,
        Determination::Undetermined(undecided) => bail!(
            "refusing to discard the shadow worktree at {}: which branch git has \
             checked out there could not be determined ({undecided}), and discarding \
             force-deletes a branch. Nothing was removed.",
            worktree_path.display()
        ),
    };
    let Some(registered) = registered else {
        bail!(
            "refusing to discard the shadow worktree at {}: git registers no branch \
             for that path (not a worktree, or a detached HEAD), so there is no ref \
             this finish may force-delete. Nothing was removed.",
            worktree_path.display()
        )
    };
    if registered == expected {
        return Ok(registered);
    }
    if run.is_none() && worktree::is_run_scoped_form(&registered, branch) {
        // `exec` (or `worktree create`) ran under a run namespace this finish
        // was not told about. Observed from git, not assumed: the directory
        // being discarded really is on that ref, and that ref really is
        // `branch` under a namespace, so discarding it closes the gap instead
        // of stranding the branch.
        return Ok(registered);
    }
    bail!(
        "run-namespace mismatch for the shadow worktree at {}: git has branch {} \
         checked out there, but --run/--branch resolve to {}. Refusing to \
         force-delete a branch that is not the one this worktree is on; nothing \
         was removed.",
        worktree_path.display(),
        registered,
        expected
    )
}

/// Finish a shadow-run: discard the shadow worktree (force-remove + force-
/// delete its branch — the committed work is never merged) and best-effort
/// record the outcome to fugu-router by spawning exactly `fugu_bin`. Returns
/// whether the fugu-router record call actually landed (`false` when
/// `fugu_bin` is `None` — the caller could not locate fugu-router, or observed
/// it absent — or cannot be spawned or exits non-zero; the caller says which
/// on stdout instead of claiming a record). The worktree is discarded either
/// way.
///
/// The binary is a parameter, not a bare `"fugu-router"` PATH lookup, so the
/// CLI can hand in the same `harness_core::plugin_bin::resolve` result
/// `record-run` uses and a test can hand in a path that records nowhere. The bare PATH lookup this
/// replaced let every `cargo test -p condukt` append fake "shadow attempt"
/// episodes to the user's real store (1335 observed 2026-10-01).
///
/// `run` is the run namespace the shadow worktree was cut under (`None` for the
/// legacy, un-namespaced caller). The ref that gets deleted is resolved by
/// [`resolve_discard_branch`] BEFORE anything is removed, so a mismatch leaves
/// the worktree and every branch untouched.
pub fn finish(
    repo: &Path,
    worktree_path: &Path,
    branch: &str,
    run: Option<&str>,
    outcome: &ShadowOutcome,
    fugu_bin: Option<&Path>,
) -> Result<bool> {
    let target = resolve_discard_branch(repo, worktree_path, run, branch)?;
    worktree::discard(repo, worktree_path, Some(&target)).with_context(|| {
        format!(
            "failed to discard shadow worktree at {}",
            worktree_path.display()
        )
    })?;
    Ok(match fugu_bin {
        Some(bin) => record_to_fugu_router(outcome, bin),
        // No program to spawn ⇒ nothing was recorded.
        None => false,
    })
}

fn record_to_fugu_router(outcome: &ShadowOutcome, fugu_bin: &Path) -> bool {
    let mut cmd = std::process::Command::new(fugu_bin);
    cmd.arg("record")
        .args(["--title", &outcome.title])
        .args(["--files", ""])
        .args(["--class", "shadow-run"])
        .args(["--model", &outcome.model])
        .args(["--status", if outcome.pass { "verified" } else { "failed" }])
        .args(["--cost", &outcome.cost_usd.to_string()])
        .args(["--duration", &outcome.duration_secs.to_string()]);
    match cmd.status() {
        Ok(status) => status.success(),
        Err(_) => false, // fugu_bin not spawnable → nothing recorded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn flag_dir() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path().to_path_buf();
        (tmp, dir)
    }

    #[test]
    fn disabled_by_default_when_no_flag_file() {
        let (_tmp, dir) = flag_dir();
        assert!(!is_enabled(&dir));
    }

    #[test]
    fn enable_then_disable_round_trips() {
        let (_tmp, dir) = flag_dir();
        set_enabled(&dir, true).unwrap();
        assert!(is_enabled(&dir));
        set_enabled(&dir, false).unwrap();
        assert!(!is_enabled(&dir));
    }

    #[test]
    fn corrupt_flag_file_is_treated_as_disabled() {
        let (_tmp, dir) = flag_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(flag_path(&dir), "not json").unwrap();
        assert!(!is_enabled(&dir));
    }

    fn git(dir: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git should run")
    }

    /// Initialise a bare-minimum git repo under a process-private `TempDir`.
    /// The repo lives in a `repo` subdirectory (not at `tmp`'s root) so that
    /// callers can carve out a sibling directory under the same `TempDir` for
    /// a worktree base — keeping it both outside the repo (required by
    /// `worktree::create`'s anti-nesting guard) and private to this process
    /// (unlike `tmp.path().parent()`, which is the shared system temp dir and
    /// collides across concurrently-running test processes).
    fn init_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        fs::write(repo.join("base.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);
        (tmp, repo)
    }

    /// End-to-end plumbing test: a shadow worktree with a committed "shadow
    /// implementation" is discarded (never merged into main) and the outcome
    /// is handed to `finish`. Since fugu-router is not guaranteed to be on
    /// PATH in the test environment, we only assert on the worktree/branch
    /// disposal side — the fugu-router record call is exercised as a
    /// best-effort side effect (`finish`'s `Ok` return already covers both
    /// outcomes of that soft dependency).
    #[test]
    fn finish_discards_shadow_worktree_and_never_merges_it() {
        let (tmp, repo) = init_repo();
        // A sibling of `repo` under the same process-private `TempDir`, not
        // `repo.parent()` (the shared system temp dir): that would collide
        // with every other concurrently-running instance of this test on the
        // same fixed worktree_base/branch/topic combination.
        let worktree_base = tmp.path().join("shadow-worktrees");
        let branch = "shadow/t1-opus";
        let path = worktree::create(&repo, &worktree_base, "t1-shadow", branch)
            .expect("worktree create should succeed");

        // Simulate the shadow worker's committed (but never-to-be-merged) change.
        fs::write(path.join("shadow.txt"), "shadow model output\n").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-m", "shadow attempt"]);

        let outcome = ShadowOutcome {
            title: "t1 shadow attempt".to_string(),
            model: "opus".to_string(),
            pass: true,
            cost_usd: 0.42,
            duration_secs: 12.5,
        };
        // Explicit nonexistent binary: this test must never reach a real
        // fugu-router (or the user's real ~/.fugu-router store).
        let recorded = finish(
            &repo,
            &path,
            branch,
            None,
            &outcome,
            Some(Path::new("/nonexistent/fugu-router")),
        )
        .expect("finish should succeed");
        assert!(
            !recorded,
            "a nonexistent fugu-router binary must report not-recorded"
        );

        assert!(!path.exists(), "shadow worktree dir should be removed");
        assert!(
            !repo.join("shadow.txt").exists(),
            "shadow content must never land on main"
        );
        let branches = String::from_utf8_lossy(&git(&repo, &["branch", "--list", branch]).stdout)
            .trim()
            .to_string();
        assert!(
            branches.is_empty(),
            "shadow branch should be force-deleted, found: {branches:?}"
        );
    }

    #[test]
    fn shadow_run_does_not_fire_when_disabled() {
        // Plumbing contract test for the CLI gate: with the flag left at its
        // default (disabled), `is_enabled` must be false so `main.rs`'s
        // `ShadowRunAction::Exec` handler refuses before ever calling
        // `worktree::create`. Exercised at the CLI level in
        // `tests/shadow_run.rs`; this asserts the underlying primitive the
        // gate depends on.
        let (_tmp, dir) = flag_dir();
        assert!(!is_enabled(&dir));
    }
}

#[cfg(test)]
mod backlog_f31dd957 {
    //! backlog f31dd957: the `Undetermined` and `Known(None)` refusal arms of
    //! `resolve_discard_branch` had kill rate 0. Each arm gets its own
    //! behavioural test; a fall-through to `Ok(expected)` in either arm turns
    //! the matching test RED.
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn undetermined_registered_branch_refuses() {
        // `repo` is not a git repository, so `git worktree list` fails and
        // `registered_branch` is Undetermined.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&repo).unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        assert!(
            matches!(
                worktree::registered_branch(&repo, &wt),
                Determination::Undetermined(_)
            ),
            "fixture precondition: registered_branch must be Undetermined for a non-repo"
        );
        let r = resolve_discard_branch(&repo, &wt, None, "shadow-b");
        let err = r.expect_err("Undetermined registered branch must REFUSE, not resolve");
        assert!(
            err.to_string().contains("could not be determined"),
            "refusal must come from the Undetermined arm: {err}"
        );
    }

    #[test]
    fn detached_worktree_known_none_refuses() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        let wt = tmp.path().join("wt-detached");
        git(
            &repo,
            &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
        );
        assert_eq!(
            worktree::registered_branch(&repo, &wt),
            Determination::Known(None),
            "fixture precondition: a detached worktree registers no branch"
        );
        let r = resolve_discard_branch(&repo, &wt, None, "shadow-b");
        let err = r.expect_err("a worktree naming no branch must REFUSE, not resolve");
        assert!(
            err.to_string().contains("registers no branch"),
            "refusal must come from the Known(None) arm: {err}"
        );
    }
}
