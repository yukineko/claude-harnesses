//! SessionStart hook: if this project has an in-progress condukt run or orphan
//! worktrees, remind the agent at the top of the session (stdout is injected as
//! additional context). Silent when there is nothing to resume.

use crate::config::Config;
use crate::state;
use crate::store::repo_root;
use std::path::PathBuf;

pub fn run(cwd: &str) {
    let cfg = Config::load();
    let cwd_path = if cwd.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(cwd)
    };

    reconcile_findings(&cfg, &cwd_path);

    let runs = state::open_runs(&cfg, &cwd_path);
    let repo = repo_root(&cwd_path);
    let orphans = crate::wt_reconcile::reportable_unregistered_dirs(&repo, &cfg.worktree_base)
        .unwrap_or_default();

    let active_runs: Vec<_> = runs.iter().filter(|r| !r.paused).collect();
    let paused_runs: Vec<_> = runs.iter().filter(|r| r.paused).collect();

    if active_runs.is_empty() && paused_runs.is_empty() && orphans.is_empty() {
        return;
    }

    let mut lines = vec![String::from(
        "[condukt] Unfinished orchestration state for this project:",
    )];
    for r in &active_runs {
        let (done, total) = r.counts();
        lines.push(format!(
            "  - run '{}' ({}): {done}/{total} tasks verified",
            r.run_id, r.goal
        ));
    }
    for r in &paused_runs {
        lines.push(format!(
            "  - run '{}' ({}): PAUSED — resume with `condukt state resume --run {}`",
            r.run_id, r.goal, r.run_id
        ));
    }
    for o in &orphans {
        lines.push(format!("  - orphan worktree on disk: {}", o.display()));
    }
    lines.push(String::from(
        "Resume with `/condukt` (it reads the open run) or clean up via \
         `condukt worktree cleanup` and `condukt state show --run <id>`.",
    ));
    println!("{}", lines.join("\n"));
}

/// R5 (backlog 89544915): SessionStart also closes condukt-gate findings whose
/// run state shows the task terminal. This hook is an observability entry and
/// always exits 0, so a reconcile that could not decide (or that panicked) is
/// made VISIBLE instead: every `NOT closed` line goes to stderr AND into the
/// injected context on stdout, so it is never mistaken for "nothing to close".
fn reconcile_findings(cfg: &Config, cwd: &std::path::Path) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::finding_reconcile::reconcile(cfg, cwd)
    }));
    let report = match outcome {
        Ok(r) => r,
        Err(_) => {
            let line =
                "[condukt] gate reconcile-findings PANICKED during SessionStart: NOT closed \
                        — no gate-exec finding was judged; run `condukt gate reconcile-findings`";
            eprintln!("{line}");
            println!("{line}");
            return;
        }
    };
    let not_closed = report.not_closed_lines("[condukt] gate reconcile-findings");
    for line in &not_closed {
        eprintln!("{line}");
    }
    if report.resolved.is_empty() && not_closed.is_empty() {
        return;
    }
    let mut lines = Vec::new();
    if !report.resolved.is_empty() {
        lines.push(format!(
            "[condukt] auto-resolved {} gate-exec finding(s) by observing run state: {}",
            report.resolved.len(),
            report.resolved.join(", ")
        ));
    }
    lines.extend(not_closed);
    println!("{}", lines.join("\n"));
}
