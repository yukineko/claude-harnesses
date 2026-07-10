//! Post-execution diff-risk recording: wire the REAL worktree diff into
//! blastguard's public-API / sensitive-path classifier and record a High-risk
//! result to the overwatch violation registry.
//!
//! ## Why this exists (finding 4 / WorkItem-A)
//!
//! blastguard's [`blastguard::diffrisk::classify_diff`] has two signals: a
//! sensitive-path glob hit and a public-API surface change
//! ([`blastguard::diffrisk::changes_public_symbol`]). The public-API signal was
//! DEAD in production: the only in-workspace callers
//! ([`crate::gate_exec::gather_assessment`] and [`crate::schedule::schedule`])
//! run at a *pre-execution* stage where no diff exists yet, so they pass an
//! empty `diff_text` and the public-API line-scan never fires (see the extensive
//! doc comment on `classify_diff`, and `gate_exec.rs`' "cannot fire here (empty
//! diff_text)" note).
//!
//! This module supplies the missing piece: a *post-execution* call site that has
//! a REAL unified diff in hand. It runs at the moment a task transitions to
//! `done` (post-worker, pre-merge/verify), diffs the task's worktree against the
//! base branch, and feeds that real diff to `classify_diff`. When the result is
//! High-risk (public-API change AND sensitive path, per `classify_diff`'s own
//! `(true,true) => High` rule) it records ONE `ViolationSource::Blastguard`
//! event to the overwatch registry so fleet-level correlated-error detection can
//! see it.
//!
//! ## Fail-soft contract (never break a turn)
//!
//! This is OBSERVATIONAL, not a new blocking gate. Every step degrades to a
//! no-op on any error: a missing/absent worktree, a `git diff` failure, an
//! empty diff, a non-High verdict, or an overwatch write failure all simply
//! return without recording and WITHOUT changing condukt's exit code. It does
//! not touch the schedule-time gated-task separation logic in
//! [`crate::schedule`].
//!
//! ## Translated sensitive paths (WorkItem-D)
//!
//! blastguard's default sensitive globs target auth/payment/PII — a surface this
//! repo does not have. This repo's "sensitive" surface is its *gate/plugin*
//! machinery: hooks, plugin manifests, skills, agents, and the project charter.
//! [`repo_sensitive_config`] layers those globs on top of the defaults so a diff
//! that changes, say, a hook or a SKILL is treated as review-worthy here.

use crate::config::Config;
use crate::state::TaskState;
use blastguard::classify::Risk;
use blastguard::diffrisk::{classify_diff, SensitiveConfig};
use overwatch::violation::{build_event, RawViolation, ViolationSource};
use std::path::Path;

/// The rule id / discriminator recorded for a High-risk post-execution diff.
/// Stable so overwatch's `normalize_signature` folds every occurrence into the
/// same `blastguard:diffrisk-public-api` bucket for recurrence detection.
const DIFFRISK_RULE_ID: &str = "diffrisk-public-api";

/// This repo's "sensitive" surface (WorkItem-D). It has no auth/payment/PII;
/// what warrants extra review here is its gate/plugin machinery: hooks, plugin
/// manifests, skills, agents, and the project charter / prompt assets. These
/// are layered ON TOP OF blastguard's defaults (via
/// [`SensitiveConfig::with_extra_globs`]) rather than replacing them, so the
/// classifier stays a superset of the shared baseline.
const REPO_SENSITIVE_GLOBS: &[&str] = &[
    "**/hooks/**",
    "**/.claude-plugin/**",
    "**/skills/**",
    "**/agents/**",
    "**/CLAUDE.md",
];

/// The [`SensitiveConfig`] for this repo's post-execution risk classification:
/// blastguard's built-in auth/payment/PII defaults plus this repo's translated
/// gate/plugin globs (see [`REPO_SENSITIVE_GLOBS`]).
pub(crate) fn repo_sensitive_config() -> SensitiveConfig {
    let extra: Vec<String> = REPO_SENSITIVE_GLOBS.iter().map(|s| s.to_string()).collect();
    SensitiveConfig::with_extra_globs(&extra)
}

/// Compute the REAL unified diff of a task's worktree against `base` (the
/// default branch), best-effort. Returns `None` when there is no worktree on
/// disk, the git command fails, or the diff is empty — every one of which the
/// caller degrades to "record nothing".
///
/// Uses the three-dot form `git diff <base>...HEAD` so the diff reflects what
/// the branch *added* since it forked from `base` (the merge-base), not
/// unrelated commits that landed on `base` in the meantime — the same shape a
/// reviewer sees for the pending merge.
fn worktree_diff(worktree: &Path, base: &str) -> Option<String> {
    if !worktree.exists() {
        return None;
    }
    // `<base>...HEAD` diffs against the merge-base; fall back to a plain
    // working-tree diff if the symmetric form fails (e.g. base ref unknown in
    // this worktree), so an uncommitted-but-real change is still inspected.
    let spec = format!("{base}...HEAD");
    let diff = crate::worktree::git(worktree, &["diff", &spec])
        .or_else(|_| crate::worktree::git(worktree, &["diff", base]))
        .or_else(|_| crate::worktree::git(worktree, &["diff", "HEAD"]))
        .ok()?;
    if diff.trim().is_empty() {
        None
    } else {
        Some(diff)
    }
}

/// Post-execution diff-risk hook: classify a just-finished task's REAL diff and,
/// if High-risk, record ONE `blastguard` violation to overwatch. Fully
/// fail-soft — any missing input or write error is a silent no-op that never
/// changes the exit code.
///
/// Wiring point: called from the `state set --status done` handler, at the
/// moment a worker's task transitions to `done` (post-worker, pre-merge). At
/// that point the task's worktree still exists on disk and its branch tip is the
/// worker's finished commit, so a real diff is obtainable.
///
/// `run_id` is used as the overwatch `task_key`-scope; `task.id` identifies the
/// specific task. `paths` is the task's declared touched-files footprint (its
/// decomposition `touched_files`), threaded in by the caller so this module
/// stays free of decomposition-join logic and is unit-testable. Returns `true`
/// iff a violation was recorded (for tests / observability); the caller ignores
/// the value.
pub(crate) fn record_post_execution_diff_risk(
    cfg: &Config,
    cwd: &Path,
    run_id: &str,
    task: &TaskState,
    paths: &[String],
    now: i64,
    session_id: &str,
) -> bool {
    // No worktree recorded → nothing to inspect (fail-soft no-op).
    let worktree = match task.worktree.as_deref() {
        Some(w) => Path::new(w),
        None => return false,
    };
    let diff = match worktree_diff(worktree, &cfg.default_branch) {
        Some(d) => d,
        None => return false,
    };

    // The classifier's path signal wants the touched paths (matching how the
    // schedule-time force-gate feeds `classify_change`). Fail-soft: an empty
    // list simply means the sensitive-path signal can't fire.
    let sensitive = repo_sensitive_config();
    let assessment = classify_diff(paths, &diff, &sensitive);

    // OBSERVATIONAL: only High (both signals — public-API AND sensitive-path)
    // is recorded. Medium/Low are informational and not persisted, mirroring the
    // conservative "record only the clearly review-worthy corner" posture.
    if !matches!(assessment.risk, Risk::High) {
        return false;
    }

    // Build + append the violation, fail-soft. `build_event` returns None only
    // for an un-bucketable discriminator (never, since DIFFRISK_RULE_ID is a
    // fixed non-empty token), and `append_violation` degrades a write error to
    // nothing — neither can change the exit code.
    let raw = RawViolation {
        rule_id: Some(DIFFRISK_RULE_ID),
        ..Default::default()
    };
    let detail = format!(
        "post-execution diff-risk: public-API change on a sensitive path (task '{}', run '{}')",
        task.id, run_id
    );
    let repo = crate::worktree::toplevel(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    match build_event(
        ViolationSource::Blastguard,
        &raw,
        format!("{run_id}/{}", task.id),
        session_id.to_string(),
        now,
        Some(detail),
    ) {
        Some(ev) => overwatch::store::append_violation(&repo, &ev).is_ok(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_sensitive_config_matches_gate_plugin_surface() {
        let cfg = repo_sensitive_config();
        // The translated (WorkItem-D) globs fire on this repo's surface.
        assert!(cfg.any_sensitive(&["crates/condukt/hooks/stop.sh".to_string()]));
        assert!(cfg.any_sensitive(&["crates/condukt/.claude-plugin/plugin.json".to_string()]));
        assert!(cfg.any_sensitive(&["crates/foo/skills/bar/SKILL.md".to_string()]));
        assert!(cfg.any_sensitive(&["crates/foo/agents/worker.md".to_string()]));
        assert!(cfg.any_sensitive(&["CLAUDE.md".to_string()]));
        // A plain source file is NOT sensitive.
        assert!(!cfg.any_sensitive(&["crates/condukt/src/schedule.rs".to_string()]));
    }

    #[test]
    fn repo_sensitive_config_still_has_blastguard_defaults() {
        // Layered on top of, not replacing, blastguard's auth/payment/PII globs.
        let cfg = repo_sensitive_config();
        assert!(cfg.any_sensitive(&["src/auth/login.rs".to_string()]));
    }

    #[test]
    fn worktree_diff_none_when_worktree_missing() {
        let missing = Path::new("/definitely/not/a/real/worktree/xyzzy");
        assert!(worktree_diff(missing, "main").is_none());
    }
}
