//! Test-only fixture: run state + decomposition for a `<run>/<task>` identity
//! at kind=chore, written where `merge` reads them with `cfg`.
//!
//! Re-anchored per user ruling 2026-11-20 round 4 Q-A (backlog 7768aeaf, spec
//! condukt-merge-proof-contract-7768aeaf R5/R6): a merge whose task is named or
//! recorded (`branch.<b>.conduktTask`) now reads the task's kind from run
//! state, and an absent run is `merge-proof-contract(undetermined-task)`.
//! kind=chore is outside the proof contract, so each test still guards only its
//! own property. The files are git-excluded so a later `git add .` in a fixture
//! does not commit them (the branches' content is unchanged).
//!
//! Test-only: the inner `#![cfg(test)]` (not an outer attribute on the `mod`
//! line) keeps worktree.rs under its file-growth cap.
#![cfg(test)]

use crate::config::Config;
use std::fs;
use std::path::Path;

pub(super) fn record_fixture_run_state(cfg: &Config, repo: &Path, identity: &str) {
    let (run, task) = identity.split_once('/').unwrap();
    let rs = crate::state::RunState {
        run_id: run.to_string(),
        goal: "g".to_string(),
        tasks: vec![crate::state::TaskState {
            id: task.to_string(),
            status: crate::state::Status::Pending,
            ..Default::default()
        }],
        paused: false,
        terminal_label: None,
        owner_session: None,
        recorded_at: None,
        recorded_episodes: Vec::new(),
    };
    rs.save(cfg, repo).unwrap();
    let dec = serde_json::json!({"goal": "g", "tasks": [
        {"id": task, "title": "t", "kind": "chore"}
    ]});
    crate::state::save_decomposition(cfg, repo, run, &dec.to_string()).unwrap();
    let exclude = repo.join(".git").join("info").join("exclude");
    let mut text = match fs::read_to_string(&exclude) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => panic!("fixture: read {}: {e}", exclude.display()),
    };
    let line = format!("/state/*/{run}.*\n");
    if !text.contains(&line) {
        text.push_str(&line);
        fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        fs::write(&exclude, text).unwrap();
    }
}
