//! Deterministic *harvest* of a completed run's grounding facts for cross-task
//! lesson authoring (phase-9 cross-task learning — the WRITE/capture side).
//!
//! The RETRIEVE side already ships: [`harness_core::lessons`] (an idempotent
//! JSONL store + lexical top-K search) plus the condukt SKILL Phase-1
//! `lessons_context` injection. This module is the capture side's *deterministic
//! plumbing*: it reads a finished run's structured facts — the goal, each task's
//! title/`done_criteria`/status, and any recorded `findings` summary — and emits
//! them as JSON so the SKILL's Phase-8 close-out step can author ONE reusable
//! lesson **grounded in those facts** (not hallucinated) and append it via
//! `fugu-router lessons add`.
//!
//! The split honours the north star: what to surface, when, and the fail-soft
//! shape is deterministic CODE here; only the lesson *text* is the LLM's
//! semantic judgment. A missing or corrupt run yields an empty object `{}` and
//! never errors — harvesting must never break the close-out turn.

use crate::config::Config;
use crate::state::{load_decomposition, RunState};
use serde_json::{json, Map, Value};
use std::path::Path;

/// Build the grounding-facts JSON for `run_id`.
///
/// Shape:
/// ```json
/// { "run_id": "...", "goal": "...",
///   "tasks": [ {"id","status","title","done_criteria","findings"} ],
///   "reasons": ["<non-empty findings summaries>"] }
/// ```
/// Fail-soft: an unloadable run (missing / corrupt state) returns `{}`.
pub fn harvest(cfg: &Config, cwd: &Path, run_id: &str) -> Value {
    let run = match RunState::load(cfg, cwd, run_id) {
        Ok(r) => r,
        // No such run / corrupt state — never error out of a close-out step.
        Err(_) => return json!({}),
    };

    // Per-task `title` / `done_criteria` live in the decomposition JSON, not in
    // run-state. Load it best-effort and index by task id; absent → nulls.
    let decomp: Value = load_decomposition(cfg, cwd, run_id)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let decomp_field = |id: &str| -> (Option<String>, Option<String>) {
        let Some(tasks) = decomp.get("tasks").and_then(|t| t.as_array()) else {
            return (None, None);
        };
        for t in tasks {
            if t.get("id").and_then(|v| v.as_str()) == Some(id) {
                let title = t.get("title").and_then(|v| v.as_str()).map(String::from);
                let dc = t
                    .get("done_criteria")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                return (title, dc);
            }
        }
        (None, None)
    };

    let mut tasks = Vec::new();
    let mut reasons = Vec::new();
    for t in &run.tasks {
        let (title, done_criteria) = decomp_field(&t.id);
        // `Status` serializes to a lowercase string ("verified", "failed", ...).
        let status = serde_json::to_value(t.status)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        let findings = t.findings.as_ref().map(|f| f.summary.clone());
        if let Some(s) = &findings {
            if !s.trim().is_empty() {
                reasons.push(s.clone());
            }
        }
        let mut obj = Map::new();
        obj.insert("id".into(), json!(t.id));
        obj.insert("status".into(), json!(status));
        obj.insert("title".into(), json!(title));
        obj.insert("done_criteria".into(), json!(done_criteria));
        obj.insert("findings".into(), json!(findings));
        tasks.push(Value::Object(obj));
    }

    json!({
        "run_id": run.run_id,
        "goal": run.goal,
        "tasks": tasks,
        "reasons": reasons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::{save_decomposition, Findings, RunState, Status, TaskState};
    use tempfile::TempDir;

    fn make_test_cfg(tmp: &Path) -> Config {
        Config {
            worktree_base: tmp.join("worktrees"),
            default_branch: "main".to_string(),
            shared_globs: Vec::new(),
            max_parallel: 4,
            state_dir: tmp.to_path_buf(),
            test_command: None,
            stuck_ttl_secs: 1800,
            build_command: None,
            deploy_command: None,
            loop_max_iters: 10,
            autonomous: false,
            consensus_enabled: false,
            consensus_samples: crate::consensus::DEFAULT_SAMPLES,
            consensus_threshold: crate::consensus::DEFAULT_THRESHOLD,
            single_worktree: false,
            worker_sandbox_enabled: false,
            worker_sandbox_image: None,
            worker_sandbox_memory: None,
            worker_sandbox_cpus: None,
            worker_sandbox_pids_limit: None,
        }
    }

    fn task(id: &str, status: Status, findings: Option<&str>) -> TaskState {
        TaskState {
            id: id.to_string(),
            status,
            findings: findings.map(|s| Findings {
                summary: s.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn harvest_emits_goal_titles_done_criteria_status_and_findings() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let cfg = make_test_cfg(cwd);
        let rid = "run-harvest-001";

        let run = RunState {
            run_id: rid.to_string(),
            goal: "close the write->retrieve loop".to_string(),
            tasks: vec![
                task("t1", Status::Verified, Some("prefer word-boundary match")),
                task("t2", Status::Failed, None),
            ],
            paused: false,
            terminal_label: None,
            recorded_at: None,
        };
        run.save(&cfg, cwd).unwrap();
        save_decomposition(
            &cfg,
            cwd,
            rid,
            r#"{"goal":"close the write->retrieve loop","tasks":[
                {"id":"t1","title":"add harvest subcommand","done_criteria":"emits JSON facts"},
                {"id":"t2","title":"wire SKILL Phase-8","done_criteria":"append via fugu"}]}"#,
        )
        .unwrap();

        let v = harvest(&cfg, cwd, rid);
        assert_eq!(v["run_id"], "run-harvest-001");
        assert_eq!(v["goal"], "close the write->retrieve loop");

        let tasks = v["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["id"], "t1");
        assert_eq!(tasks[0]["status"], "verified");
        assert_eq!(tasks[0]["title"], "add harvest subcommand");
        assert_eq!(tasks[0]["done_criteria"], "emits JSON facts");
        assert_eq!(tasks[0]["findings"], "prefer word-boundary match");
        assert_eq!(tasks[1]["status"], "failed");
        // t2 had no findings → null, and contributes no reason.
        assert!(tasks[1]["findings"].is_null());

        // Only non-empty findings become grounding `reasons`.
        let reasons = v["reasons"].as_array().unwrap();
        assert_eq!(reasons.len(), 1);
        assert_eq!(reasons[0], "prefer word-boundary match");
    }

    #[test]
    fn harvest_missing_run_is_fail_soft_empty_object() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let cfg = make_test_cfg(cwd);
        let v = harvest(&cfg, cwd, "run-does-not-exist");
        assert!(v.is_object());
        assert_eq!(v.as_object().unwrap().len(), 0, "empty object {{}}");
    }

    /// DoD round-trip: the deterministic harvest facts drive an idempotent
    /// append into the cross-project store that lexical search then retrieves —
    /// and a second, byte-identical append (same content-derived id) is a true
    /// no-op. `fugu-router lessons add|search` are thin wrappers over these same
    /// `harness_core::lessons` fns, so exercising them here proves the WRITE
    /// side's seam without spawning a second binary.
    #[test]
    fn harvest_facts_drive_idempotent_append_that_search_retrieves() {
        use harness_core::lessons::{self, Kind, Lesson};

        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let cfg = make_test_cfg(cwd);
        let rid = "run-roundtrip-1";
        let run = RunState {
            run_id: rid.to_string(),
            goal: "match model tier on word boundary".to_string(),
            tasks: vec![task(
                "t1",
                Status::Verified,
                Some("substring tier match is a bug; match on a token boundary"),
            )],
            paused: false,
            terminal_label: None,
            recorded_at: None,
        };
        run.save(&cfg, cwd).unwrap();

        // 1. deterministic harvest of grounding facts
        let facts = harvest(&cfg, cwd, rid);
        let task_summary = facts["goal"].as_str().unwrap().to_string();
        // In production the driver AUTHORS the lesson text; here we lift the
        // harvested finding verbatim as a stand-in so the test stays deterministic.
        let lesson_text = facts["reasons"][0].as_str().unwrap().to_string();

        // Isolate the machine-global store to an absolute temp dir (the override
        // is honored only when absolute — TempDir paths are absolute).
        let store = tmp.path().join("lessons-store");
        std::env::set_var("LESSONS_STORE_DIR", &store);

        // 2. append (mirrors `fugu-router lessons add`: content-derived id → a
        //    byte-identical re-add collapses to the same id).
        let lesson = Lesson {
            id: "lesson-wordboundary".to_string(),
            kind: Kind::ErrorPattern,
            task_summary: task_summary.clone(),
            lesson_text: lesson_text.clone(),
            source_run: rid.to_string(),
            ts: 0,
        };
        lessons::append(&lesson);
        lessons::append(&lesson); // identical content/id → must be a no-op

        let all = lessons::load();
        assert_eq!(
            all.len(),
            1,
            "identical re-add is a true no-op (idempotent)"
        );

        // 3. search retrieves the authored lesson by an overlapping query.
        let hits = lessons::search("word boundary tier match", &all, lessons::DEFAULT_K);
        assert!(
            !hits.is_empty(),
            "authored lesson is retrievable via search"
        );

        std::env::remove_var("LESSONS_STORE_DIR");
    }

    #[test]
    fn harvest_without_decomposition_still_emits_tasks_with_null_titles() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let cfg = make_test_cfg(cwd);
        let rid = "run-harvest-nodecomp";
        let run = RunState {
            run_id: rid.to_string(),
            goal: "g".to_string(),
            tasks: vec![task("t1", Status::Verified, None)],
            paused: false,
            terminal_label: None,
            recorded_at: None,
        };
        run.save(&cfg, cwd).unwrap();
        // no save_decomposition → titles/done_criteria fall back to null
        let v = harvest(&cfg, cwd, rid);
        let tasks = v["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0]["title"].is_null());
        assert!(tasks[0]["done_criteria"].is_null());
        assert_eq!(tasks[0]["status"], "verified");
    }
}
