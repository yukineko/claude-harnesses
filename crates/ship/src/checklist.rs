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

/// Aggregated shipping-state detection results for a repo.
#[derive(Debug, Default, Clone)]
pub struct ShipStatus {
    pub uncommitted: bool,
    pub unmerged_branches: Vec<String>,
    pub leftover_worktrees: Vec<String>,
    pub unpushed_count: usize,
    pub stale_crates: Vec<String>,
}

impl ShipStatus {
    /// True iff there is any unshipped work worth reminding the user about.
    ///
    /// Leftover worktrees are informational-only (not "unshipped work" per se)
    /// so they don't affect this; they're still shown in the checklist.
    pub fn has_unshipped_work(&self) -> bool {
        self.uncommitted
            || !self.unmerged_branches.is_empty()
            || self.unpushed_count > 0
            || !self.stale_crates.is_empty()
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
    if status.uncommitted {
        out.push_str(&format!(
            "  [ ] uncommitted changes present — commit ({GATED})\n"
        ));
    } else {
        out.push_str("  [x] no uncommitted changes\n");
    }

    // unmerged condukt branches -> merge is gated
    if status.unmerged_branches.is_empty() {
        out.push_str("  [x] no unmerged condukt/* branches\n");
    } else {
        out.push_str(&format!(
            "  [ ] unmerged condukt/* branches: {} — merge ({GATED})\n",
            status.unmerged_branches.join(", ")
        ));
    }

    // leftover worktrees -> informational
    if status.leftover_worktrees.is_empty() {
        out.push_str("  [x] no leftover worktrees\n");
    } else {
        out.push_str(&format!(
            "  [ ] leftover worktrees: {}\n",
            status.leftover_worktrees.join(", ")
        ));
    }

    // unpushed commits -> push is gated
    if status.unpushed_count == 0 {
        out.push_str("  [x] no unpushed commits\n");
    } else {
        out.push_str(&format!(
            "  [ ] {} unpushed commit(s) — push ({GATED})\n",
            status.unpushed_count
        ));
    }

    // stale crates -> the committed bin is older than its src.
    if status.stale_crates.is_empty() {
        out.push_str("  [x] no stale plugin binaries\n");
    } else {
        out.push_str(&format!(
            "  [ ] stale plugin binaries: {} — the committed crates/<name>/bin/ binary is older than \
its src. Run `scripts/rebuild-plugins.sh --stage-repo` (safe, auto-runnable — writes files, never git) \
to refresh the committed binary, then commit it ({GATED}). NOTE: plain `scripts/rebuild-plugins.sh` and \
`scripts/rollout-plugins.sh` / `ship rollout` only swap the plugin *cache* and do NOT clear this \
committed-binary staleness\n",
            status.stale_crates.join(", ")
        ));
    }

    out
}

/// Render only the GATED (commit/merge/push) lines that remain outstanding.
/// Used after `--run-safe` to reprint what still needs human approval.
pub fn render_gated_remaining(status: &ShipStatus) -> String {
    let mut out = String::new();
    let mut any = false;

    if status.uncommitted {
        out.push_str(&format!(
            "  [ ] uncommitted changes present — commit ({GATED})\n"
        ));
        any = true;
    }
    if !status.unmerged_branches.is_empty() {
        out.push_str(&format!(
            "  [ ] unmerged condukt/* branches: {} — merge ({GATED})\n",
            status.unmerged_branches.join(", ")
        ));
        any = true;
    }
    if status.unpushed_count > 0 {
        out.push_str(&format!(
            "  [ ] {} unpushed commit(s) — push ({GATED})\n",
            status.unpushed_count
        ));
        any = true;
    }

    if !any {
        out.push_str("  [x] nothing gated remains — all clear\n");
    }

    out
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
pub fn run_safe(repo: &Path) -> Result<String, String> {
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

    Ok(summary)
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
pub fn rollout(repo: &Path) -> Result<String, String> {
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

    Ok(summary)
}

/// Build a concise SessionEnd reminder line for unshipped work. Returns None
/// when nothing is unshipped (caller should print nothing in that case).
pub fn session_end_reminder(status: &ShipStatus) -> Option<String> {
    if !status.has_unshipped_work() {
        return None;
    }

    let mut parts = vec![];
    if status.uncommitted {
        parts.push("未コミットの変更".to_string());
    }
    if !status.unmerged_branches.is_empty() {
        parts.push(format!(
            "未マージブランチ {}件",
            status.unmerged_branches.len()
        ));
    }
    if status.unpushed_count > 0 {
        parts.push(format!("未pushコミット {}件", status.unpushed_count));
    }
    if !status.stale_crates.is_empty() {
        parts.push(format!(
            "古いプラグインバイナリ {}件",
            status.stale_crates.len()
        ));
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
            uncommitted: true,
            unmerged_branches: vec!["condukt/foo".to_string()],
            leftover_worktrees: vec![],
            unpushed_count: 2,
            stale_crates: vec![],
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
            stale_crates: vec!["ship".to_string()],
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
            stale_crates: vec!["ship".to_string()],
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
            uncommitted: true,
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
            leftover_worktrees: vec!["/tmp/some-worktree".to_string()],
            ..Default::default()
        };
        assert!(session_end_reminder(&status).is_none());
    }

    #[test]
    fn render_gated_remaining_all_clear_when_nothing_gated() {
        let status = ShipStatus {
            leftover_worktrees: vec!["/tmp/x".to_string()],
            stale_crates: vec!["ship".to_string()],
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
            stale_crates: vec!["ship".to_string()],
            ..Default::default()
        };
        let out = render(&status);
        assert!(out.contains("rollout-plugins.sh"));
        assert!(out.contains("ship rollout"));
        assert!(out.contains("do NOT clear"));
    }
}
