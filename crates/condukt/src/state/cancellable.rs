//! `state list-tasks` rows (moved verbatim out of `state.rs`).

use super::{load_decomposition, open_runs, Status};
use crate::config::Config;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

/// A non-terminal task eligible for cancellation, with its run context.
/// Returned by `list_cancellable_tasks` for the skill's AskUserQuestion list.
#[derive(Debug, Serialize)]
pub struct CancellableTask {
    pub run_id: String,
    pub goal: String,
    pub terminal_label: Option<String>,
    /// The run's recorded `owner_session` (not the reader's session); `null`
    /// when the owner is undetermined.
    pub owner_session: Option<String>,
    pub is_paused: bool,
    pub task_id: String,
    /// From the decomposition sidecar; falls back to task_id when unavailable.
    pub task_title: String,
    pub status: String,
}

/// All non-terminal tasks across open runs that can be cancelled.
/// Loads decomposition sidecars to include task titles.
pub fn list_cancellable_tasks(cfg: &Config, cwd: &Path) -> Result<Vec<CancellableTask>> {
    let mut result = Vec::new();
    for run in open_runs(cfg, cwd)? {
        let titles: std::collections::HashMap<String, String> =
            if let Ok(raw) = load_decomposition(cfg, cwd, &run.run_id) {
                if let Ok(dec) = serde_json::from_str::<crate::model::Decomposition>(&raw) {
                    dec.tasks
                        .iter()
                        .map(|t| (t.id.clone(), t.title.clone()))
                        .collect()
                } else {
                    Default::default()
                }
            } else {
                Default::default()
            };

        for task in &run.tasks {
            if !matches!(
                task.status,
                Status::Pending | Status::Running | Status::Done
            ) {
                continue;
            }
            result.push(CancellableTask {
                run_id: run.run_id.clone(),
                goal: run.goal.clone(),
                terminal_label: run.terminal_label.clone(),
                owner_session: run.owner_session.clone(),
                is_paused: run.paused,
                task_id: task.id.clone(),
                task_title: titles
                    .get(&task.id)
                    .cloned()
                    .unwrap_or_else(|| task.id.clone()),
                status: format!("{:?}", task.status).to_lowercase(),
            });
        }
    }
    Ok(result)
}
