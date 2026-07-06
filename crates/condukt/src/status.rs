use crate::config::Config;
use crate::state::{self, Status};
use std::collections::HashMap;
use std::path::Path;

fn status_icon(s: Status) -> &'static str {
    match s {
        Status::Verified => "✓",
        Status::Failed => "✗",
        Status::Running => "↻",
        Status::Done => "•",
        Status::Pending => "○",
        Status::Cancelled => "⊘",
        Status::Discarded => "⌫",
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Verified => "verified",
        Status::Failed => "failed",
        Status::Running => "running",
        Status::Done => "done",
        Status::Pending => "pending",
        Status::Cancelled => "cancelled",
        Status::Discarded => "discarded",
    }
}

fn load_titles(cfg: &Config, cwd: &Path, run_id: &str) -> HashMap<String, String> {
    let path = state::decomposition_path(cfg, cwd, run_id);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };
    let Some(tasks) = val.get("tasks").and_then(|t| t.as_array()) else {
        return HashMap::new();
    };
    tasks
        .iter()
        .filter_map(|t| {
            let id = t.get("id")?.as_str()?.to_owned();
            let title = t.get("title")?.as_str()?.to_owned();
            Some((id, title))
        })
        .collect()
}

pub fn render(cfg: &Config, cwd: &Path, all: bool) {
    let runs = if all {
        state::all_runs(cfg, cwd)
    } else {
        state::open_runs(cfg, cwd)
    };

    if runs.is_empty() {
        println!("{}", if all { "no runs found" } else { "no open runs" });
        return;
    }

    for run in &runs {
        let (done, total) = run.counts();
        let prefix = if run.paused { "⏸" } else { "●" };
        let at = run
            .terminal_label
            .as_deref()
            .map(|l| format!("  @{l}"))
            .unwrap_or_default();
        println!(
            "{prefix} {}  [{done}/{total}]  \"{}\"{}",
            run.run_id, run.goal, at
        );

        let titles = load_titles(cfg, cwd, &run.run_id);
        let n = run.tasks.len();
        for (i, task) in run.tasks.iter().enumerate() {
            let connector = if i + 1 == n { "└─" } else { "├─" };
            let icon = status_icon(task.status);
            let title = titles.get(&task.id).map(|s| s.as_str()).unwrap_or("");
            let branch = task.branch.as_deref().unwrap_or("");
            let branch_str = if branch.is_empty() {
                String::new()
            } else {
                format!("  {branch}")
            };
            println!(
                "  {connector} {icon} {:<4}  {:<32}  {}{}",
                task.id,
                title,
                status_label(task.status),
                branch_str,
            );
        }
    }
}
