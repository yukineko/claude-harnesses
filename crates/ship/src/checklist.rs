//! Checklist rendering + the ONE auto-runnable "safe" step.
//!
//! Hard invariant: nothing in this module (or anywhere else in this crate)
//! ever invokes a state-mutating git command (commit/merge/push/add/checkout/
//! reset). `run_safe` is allowed to execute `scripts/rebuild-plugins.sh`;
//! `rollout` (a separate, explicit operation — not folded into `run_safe`)
//! is allowed to execute `scripts/rollout-plugins.sh`. Neither script touches
//! git; both only write into `~/.claude/plugins/`. See
//! `tests/integration.rs::binary_never_mutates_git`.

use std::path::Path;
use std::process::Command;

use harness_core::verdict::Determination;

/// Aggregated shipping-state detection results for a repo.
///
/// Every field is a [`Determination`]: `Undetermined` means the detection
/// itself could not run (git failed, no upstream, unreadable `crates/`). It
/// must never render the same as a clean `Known` value — it renders `[?]`,
/// never `[x]`, and counts as unshipped work.
#[derive(Debug, Clone)]
pub struct ShipStatus {
    pub uncommitted: Determination<bool>,
    pub unmerged_branches: Determination<Vec<String>>,
    pub leftover_worktrees: Determination<Vec<String>>,
    pub unpushed_count: Determination<usize>,
    pub stale_crates: Determination<Vec<String>>,
}

impl Default for ShipStatus {
    /// The all-clean, fully-determined status (test fixture convenience).
    fn default() -> Self {
        ShipStatus {
            uncommitted: Determination::Known(false),
            unmerged_branches: Determination::Known(Vec::new()),
            leftover_worktrees: Determination::Known(Vec::new()),
            unpushed_count: Determination::Known(0),
            stale_crates: Determination::Known(Vec::new()),
        }
    }
}

/// True iff `d` is known-outstanding per `pending`, OR undetermined: a check
/// that could not run resolves to the restrictive side (CLAUDE.md §3).
fn outstanding_or_unknown<T>(d: &Determination<T>, pending: impl Fn(&T) -> bool) -> bool {
    match d {
        Determination::Known(v) => pending(v),
        Determination::Undetermined(_) => true,
    }
}

impl ShipStatus {
    /// True iff there is any unshipped work worth reminding the user about.
    ///
    /// Leftover worktrees are informational-only (not "unshipped work" per se)
    /// so a KNOWN list doesn't affect this; they're still shown in the
    /// checklist. Any `Undetermined` detection (including leftover worktrees)
    /// counts as unshipped work — a check that couldn't run is not evidence
    /// that everything is clean.
    pub fn has_unshipped_work(&self) -> bool {
        outstanding_or_unknown(&self.uncommitted, |b| *b)
            || outstanding_or_unknown(&self.unmerged_branches, |v| !v.is_empty())
            || outstanding_or_unknown(&self.unpushed_count, |n| *n > 0)
            || outstanding_or_unknown(&self.stale_crates, |v| !v.is_empty())
            || outstanding_or_unknown(&self.leftover_worktrees, |_| false)
    }
}

const GATED: &str = "GATED — 要人間承認";

/// Render the full human/agent-readable checklist for a `ShipStatus`.
///
/// Commit・merge・push items are explicitly marked `GATED — 要人間承認` — they
/// are informational only and are NEVER auto-run by this binary. The
/// plugin-rebuild (stale crates) item is the one safe/auto-runnable step.
pub fn render(status: &ShipStatus) -> String {
    let mut out = String::new();
    out.push_str("⚓ ship checklist\n");

    // uncommitted changes -> commit is gated
    match &status.uncommitted {
        Determination::Known(true) => out.push_str(&format!(
            "  [ ] uncommitted changes present — commit ({GATED})\n"
        )),
        Determination::Known(false) => out.push_str("  [x] no uncommitted changes\n"),
        Determination::Undetermined(why) => out.push_str(&format!(
            "  [?] uncommitted-changes check could not run: {why}\n"
        )),
    }

    // unmerged condukt branches -> merge is gated
    match &status.unmerged_branches {
        Determination::Known(v) if v.is_empty() => {
            out.push_str("  [x] no unmerged condukt/* branches\n");
        }
        Determination::Known(v) => out.push_str(&format!(
            "  [ ] unmerged condukt/* branches: {} — merge ({GATED})\n",
            v.join(", ")
        )),
        Determination::Undetermined(why) => out.push_str(&format!(
            "  [?] unmerged-condukt/*-branches check could not run: {why}\n"
        )),
    }

    // leftover worktrees -> informational
    match &status.leftover_worktrees {
        Determination::Known(v) if v.is_empty() => out.push_str("  [x] no leftover worktrees\n"),
        Determination::Known(v) => {
            out.push_str(&format!("  [ ] leftover worktrees: {}\n", v.join(", ")));
        }
        Determination::Undetermined(why) => out.push_str(&format!(
            "  [?] leftover-worktrees check could not run: {why}\n"
        )),
    }

    // unpushed commits -> push is gated
    match &status.unpushed_count {
        Determination::Known(0) => out.push_str("  [x] no unpushed commits\n"),
        Determination::Known(n) => {
            out.push_str(&format!("  [ ] {n} unpushed commit(s) — push ({GATED})\n"))
        }
        Determination::Undetermined(why) => out.push_str(&format!(
            "  [?] unpushed-commits check could not run: {why}\n"
        )),
    }

    // stale crates -> the committed bin is older than its src.
    match &status.stale_crates {
        Determination::Known(v) if v.is_empty() => {
            out.push_str("  [x] no stale plugin binaries\n");
        }
        Determination::Known(v) => {
            out.push_str(&format!(
                "  [ ] stale plugin binaries: {} — the committed crates/<name>/bin/ binary is older than \
its src. Run `scripts/rebuild-plugins.sh --stage-repo` (safe, auto-runnable — writes files, never git) \
to refresh the committed binary, then commit it ({GATED}). NOTE: plain `scripts/rebuild-plugins.sh` and \
`scripts/rollout-plugins.sh` / `ship rollout` only swap the plugin *cache* and do NOT clear this \
committed-binary staleness\n",
                v.join(", ")
            ));
        }
        Determination::Undetermined(why) => {
            out.push_str(&format!("  [?] stale-crate check could not run: {why}\n"));
        }
    }

    out
}

/// Render only the GATED (commit/merge/push) lines that remain outstanding.
/// Used after `--run-safe` to reprint what still needs human approval.
pub fn render_gated_remaining(status: &ShipStatus) -> String {
    let mut out = String::new();
    let mut any = false;

    match &status.uncommitted {
        Determination::Known(false) => {}
        Determination::Known(true) => {
            out.push_str(&format!(
                "  [ ] uncommitted changes present — commit ({GATED})\n"
            ));
            any = true;
        }
        Determination::Undetermined(why) => {
            out.push_str(&format!(
                "  [?] uncommitted-changes check could not run: {why}\n"
            ));
            any = true;
        }
    }
    match &status.unmerged_branches {
        Determination::Known(v) if v.is_empty() => {}
        Determination::Known(v) => {
            out.push_str(&format!(
                "  [ ] unmerged condukt/* branches: {} — merge ({GATED})\n",
                v.join(", ")
            ));
            any = true;
        }
        Determination::Undetermined(why) => {
            out.push_str(&format!(
                "  [?] unmerged-condukt/*-branches check could not run: {why}\n"
            ));
            any = true;
        }
    }
    match &status.unpushed_count {
        Determination::Known(0) => {}
        Determination::Known(n) => {
            out.push_str(&format!("  [ ] {n} unpushed commit(s) — push ({GATED})\n"));
            any = true;
        }
        Determination::Undetermined(why) => {
            out.push_str(&format!(
                "  [?] unpushed-commits check could not run: {why}\n"
            ));
            any = true;
        }
    }

    if !any {
        out.push_str("  [x] nothing gated remains — all clear\n");
    }

    out
}

/// The outcome of a script that ran to completion. `succeeded` is the
/// script's exit status, so a caller can turn a FAILED run into a non-zero
/// exit instead of only printing the word FAILED.
#[derive(Debug)]
pub struct ScriptRun {
    pub summary: String,
    pub succeeded: bool,
}

/// Run the ONE auto-runnable safe step: `scripts/rebuild-plugins.sh --stage-repo`,
/// resolved relative to `repo`. MUST NOT run any git command.
///
/// `--stage-repo` refreshes BOTH the live plugin cache AND the committed
/// `crates/<name>/bin/` binary (a plain `cp`, never a git command — see
/// `tests/integration.rs::binary_never_mutates_git`). The committed binary is
/// what `stale_crates` measures and what the directory marketplace ships to
/// `/plugin install`, so staging it is what actually clears a "stale plugin
/// binaries" item. Side effect: this dirties the working tree with the
/// refreshed binary, which then surfaces as an `uncommitted changes — commit
/// (GATED)` item on re-detect (the commit itself stays human-gated).
pub fn run_safe(repo: &Path) -> Result<ScriptRun, String> {
    let script = repo.join("scripts").join("rebuild-plugins.sh");
    if !script.exists() {
        return Err(format!("script not found: {}", script.display()));
    }

    let output = Command::new(&script)
        .arg("--stage-repo")
        .current_dir(repo)
        .output()
        .map_err(|e| format!("failed to run {}: {e}", script.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut summary = String::new();
    if output.status.success() {
        summary.push_str("[run-safe] scripts/rebuild-plugins.sh --stage-repo: OK\n");
    } else {
        summary.push_str(&format!(
            "[run-safe] scripts/rebuild-plugins.sh --stage-repo: FAILED (status: {})\n",
            output.status
        ));
    }
    if !stdout.trim().is_empty() {
        summary.push_str(&format!("--- stdout ---\n{stdout}\n"));
    }
    if !stderr.trim().is_empty() {
        summary.push_str(&format!("--- stderr ---\n{stderr}\n"));
    }

    Ok(ScriptRun {
        summary,
        succeeded: output.status.success(),
    })
}

/// Run the heavier, explicit rollout step: `scripts/rollout-plugins.sh`,
/// resolved relative to `repo`. It advances the installed version pointer
/// (the `/plugin update` step) by creating the cache `<name>/<version>/` dir
/// and repointing `installed_plugins.json`, then runs rebuild-plugins.sh and
/// each plugin's sync-plugin-assets.sh. It mutates only `~/.claude/plugins/`
/// state, NEVER git — kept as a separate operation from `run_safe` (not
/// folded into `--run-safe`) because it is a heavier, more consequential
/// step than a binary swap into an existing cache dir.
///
/// NOTE: rollout does NOT stage the committed `crates/<name>/bin/` binary
/// (it runs rebuild-plugins.sh without `--stage-repo`), so it does NOT clear
/// a "stale plugin binaries" item — that is committed-binary staleness, which
/// `check --run-safe` refreshes (`--stage-repo`) and a GATED commit finalizes.
pub fn rollout(repo: &Path) -> Result<ScriptRun, String> {
    let script = repo.join("scripts").join("rollout-plugins.sh");
    if !script.exists() {
        return Err(format!("script not found: {}", script.display()));
    }

    let output = Command::new(&script)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("failed to run {}: {e}", script.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut summary = String::new();
    if output.status.success() {
        summary.push_str("[rollout] scripts/rollout-plugins.sh: OK\n");
    } else {
        summary.push_str(&format!(
            "[rollout] scripts/rollout-plugins.sh: FAILED (status: {})\n",
            output.status
        ));
    }
    if !stdout.trim().is_empty() {
        summary.push_str(&format!("--- stdout ---\n{stdout}\n"));
    }
    if !stderr.trim().is_empty() {
        summary.push_str(&format!("--- stderr ---\n{stderr}\n"));
    }

    Ok(ScriptRun {
        summary,
        succeeded: output.status.success(),
    })
}

/// Build a concise SessionEnd reminder line for unshipped work. Returns None
/// when nothing is unshipped (caller should print nothing in that case).
pub fn session_end_reminder(status: &ShipStatus) -> Option<String> {
    if !status.has_unshipped_work() {
        return None;
    }

    let mut parts = vec![];
    match &status.uncommitted {
        Determination::Known(true) => parts.push("未コミットの変更".to_string()),
        Determination::Known(false) => {}
        Determination::Undetermined(why) => {
            parts.push(format!("未コミット変更の検査が未完了: {why}"));
        }
    }
    match &status.unmerged_branches {
        Determination::Known(v) if !v.is_empty() => {
            parts.push(format!("未マージブランチ {}件", v.len()));
        }
        Determination::Known(_) => {}
        Determination::Undetermined(why) => {
            parts.push(format!("未マージブランチの検査が未完了: {why}"));
        }
    }
    if let Determination::Undetermined(why) = &status.leftover_worktrees {
        parts.push(format!("残存 worktree の検査が未完了: {why}"));
    }
    match &status.unpushed_count {
        Determination::Known(0) => {}
        Determination::Known(n) => parts.push(format!("未pushコミット {n}件")),
        Determination::Undetermined(why) => {
            parts.push(format!("未pushコミットの検査が未完了: {why}"));
        }
    }
    match &status.stale_crates {
        Determination::Known(v) if !v.is_empty() => {
            parts.push(format!("古いプラグインバイナリ {}件", v.len()));
        }
        Determination::Known(_) => {}
        Determination::Undetermined(why) => {
            parts.push(format!("古いプラグインバイナリの検査が未完了: {why}"));
        }
    }

    Some(format!(
        "⚓ 未出荷の作業があります: {} — `/ship` で確認してください。",
        parts.join("・")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_marks_commit_merge_push_as_gated() {
        let status = ShipStatus {
            uncommitted: Determination::Known(true),
            unmerged_branches: Determination::Known(vec!["condukt/foo".to_string()]),
            leftover_worktrees: Determination::Known(vec![]),
            unpushed_count: Determination::Known(2),
            stale_crates: Determination::Known(vec![]),
        };
        let out = render(&status);
        // commit
        assert!(out.contains("commit") && out.contains(GATED));
        // merge
        assert!(out.contains("merge") && out.contains(GATED));
        // push
        assert!(out.contains("push") && out.contains(GATED));
    }

    #[test]
    fn render_clean_status_has_no_gated_markers() {
        let status = ShipStatus::default();
        let out = render(&status);
        assert!(!out.contains(GATED));
        assert!(out.contains("[x] no uncommitted changes"));
    }

    #[test]
    fn render_marks_stale_crates_as_safe_auto_runnable() {
        let status = ShipStatus {
            stale_crates: Determination::Known(vec!["ship".to_string()]),
            ..Default::default()
        };
        let out = render(&status);
        assert!(out.contains("rebuild-plugins.sh"));
        assert!(out.contains("safe, auto-runnable"));
    }

    #[test]
    fn render_stale_remedy_stages_repo_and_gates_commit() {
        // The stale item is committed-binary staleness: the honest remedy is
        // `--stage-repo` (refresh the committed bin) + a GATED commit, NOT a
        // cache-only rebuild/rollout.
        let status = ShipStatus {
            stale_crates: Determination::Known(vec!["ship".to_string()]),
            ..Default::default()
        };
        let out = render(&status);
        assert!(out.contains("--stage-repo"));
        // the follow-up commit is gated
        assert!(out.contains(GATED));
        // and it must warn that cache-only paths do NOT clear it
        assert!(out.contains("do NOT clear"));
    }

    #[test]
    fn session_end_reminder_none_for_all_clean() {
        let status = ShipStatus::default();
        assert!(session_end_reminder(&status).is_none());
    }

    #[test]
    fn session_end_reminder_some_for_uncommitted() {
        let status = ShipStatus {
            uncommitted: Determination::Known(true),
            ..Default::default()
        };
        let reminder = session_end_reminder(&status);
        assert!(reminder.is_some());
        assert!(reminder.unwrap().contains("/ship"));
    }

    #[test]
    fn session_end_reminder_ignores_leftover_worktrees_only() {
        // leftover worktrees alone are informational, not "unshipped work"
        let status = ShipStatus {
            leftover_worktrees: Determination::Known(vec!["/tmp/some-worktree".to_string()]),
            ..Default::default()
        };
        assert!(session_end_reminder(&status).is_none());
    }

    #[test]
    fn render_gated_remaining_all_clear_when_nothing_gated() {
        let status = ShipStatus {
            leftover_worktrees: Determination::Known(vec!["/tmp/x".to_string()]),
            stale_crates: Determination::Known(vec!["ship".to_string()]),
            ..Default::default()
        };
        let out = render_gated_remaining(&status);
        assert!(out.contains("all clear"));
        assert!(!out.contains(GATED));
    }

    #[test]
    fn run_safe_errors_when_script_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = run_safe(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn rollout_errors_when_script_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = rollout(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn render_names_cache_only_paths_as_not_clearing_stale() {
        // rollout / plain rebuild are named for stale crates, but explicitly
        // as the cache-only paths that do NOT clear committed-binary staleness
        // (so the user isn't misdirected to a remedy that can't work).
        let status = ShipStatus {
            stale_crates: Determination::Known(vec!["ship".to_string()]),
            ..Default::default()
        };
        let out = render(&status);
        assert!(out.contains("rollout-plugins.sh"));
        assert!(out.contains("ship rollout"));
        assert!(out.contains("do NOT clear"));
    }

    /// CA-ship-checklist-01: an `Undetermined` stale-crate scan must render
    /// as an explicit "could not run" line and count as unshipped work — it
    /// must not be indistinguishable from a clean `Known(vec![])`.
    #[test]
    fn undetermined_stale_scan_renders_unknown_and_counts_as_unshipped() {
        let status = ShipStatus {
            stale_crates: Determination::undetermined("permission denied"),
            ..Default::default()
        };
        assert!(status.has_unshipped_work());
        let out = render(&status);
        assert!(out.contains("could not run"));
        assert!(!out.contains("[x] no stale plugin binaries"));
        assert!(session_end_reminder(&status).is_some());
    }
}
