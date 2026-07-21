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

use anyhow::{Context, Result};
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

/// Finish a shadow-run: discard the shadow worktree (force-remove + force-
/// delete its branch — the committed work is never merged) and best-effort
/// record the outcome to fugu-router. Returns whether the fugu-router record
/// call actually landed (`false` when fugu-router is absent from PATH — a
/// soft no-op, matching `record_runs`'s fail-soft posture elsewhere in this
/// binary).
pub fn finish(
    repo: &Path,
    worktree_path: &Path,
    branch: &str,
    outcome: &ShadowOutcome,
) -> Result<bool> {
    worktree::discard(repo, worktree_path, Some(branch)).with_context(|| {
        format!(
            "failed to discard shadow worktree at {}",
            worktree_path.display()
        )
    })?;
    Ok(record_to_fugu_router(outcome))
}

fn record_to_fugu_router(outcome: &ShadowOutcome) -> bool {
    let mut cmd = std::process::Command::new("fugu-router");
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
        Err(_) => false, // fugu-router not on PATH → soft no-op
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
        finish(&repo, &path, branch, &outcome).expect("finish should succeed");

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
