//! Durable async escalation channel — a per-project persistent queue so a
//! blocked / GATED task can enqueue a question **out-of-band** instead of
//! blocking inline on an `AskUserQuestion`.
//!
//! # Why
//!
//! When a worker (or the `/condukt` skill) hits a decision it cannot make
//! autonomously, the synchronous path is an inline `AskUserQuestion` that
//! *stalls the turn* until a human answers. That does not compose with a
//! long-running autonomous loop: the loop wants to record "I need a human
//! answer for X" and keep making progress on unrelated tasks, then resume X
//! once the answer lands. This module is that durable channel:
//!
//! - `escalate add` — enqueue a question (run, task, options, recommended index)
//!   and get back a stable id.
//! - `escalate list` — show the still-OPEN escalations for a run.
//! - `escalate resolve` — record a human's chosen option against an id; the
//!   record stays in the store (now resolved, with its answer) so the caller can
//!   resume the task with it.
//!
//! # Persistence (mirrors `claim.rs`)
//!
//! The store is a single project-scoped JSON file at
//! `<state_dir>/<project-key>/escalations.json` — right beside `claims.json`,
//! so unrelated projects never share a queue. Writes are atomic (temp +
//! rename). Everything is fail-soft: a missing or corrupt file is treated as an
//! empty registry rather than an error, and the module never panics.
//!
//! # Determinism
//!
//! The record id is a short content hash of `run + task + question +
//! existing-count` (FNV-1a), NOT a wall-clock/`rand` value, so tests can assert
//! ids deterministically. A `created_at` timestamp (unix seconds) is recorded
//! separately for observability and never feeds the id.

use crate::config::Config;
use crate::store::{project_key, repo_root};
use anyhow::Result;
use harness_core::projkey::fnv1a32;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One enqueued escalation: a question awaiting an out-of-band human answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalation {
    /// Stable content-hash id (`run + task + question + prior-count`).
    pub id: String,
    /// The condukt run this question belongs to.
    pub run: String,
    /// The task within the run that is blocked on this answer.
    pub task: String,
    /// The question being asked.
    pub question: String,
    /// The offered options (a caller resolves by choosing one of these).
    pub options: Vec<String>,
    /// 0-based index into `options` of the recommended default.
    pub recommended: usize,
    /// When the escalation was enqueued (unix seconds) — observability only.
    pub created_at: i64,
    /// Whether a human has answered (and this record is retired from `list`).
    pub resolved: bool,
    /// The chosen option once resolved (so the task can resume with it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<String>,
}

/// The on-disk registry: an ordered list of escalations (append-on-add). A bare
/// list keeps the JSON trivially forward/backward compatible and preserves
/// enqueue order for `list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub escalations: Vec<Escalation>,
}

/// `<state_dir>/<project-key>/escalations.json` — beside `claims.json`, so it is
/// per-project and unrelated projects never share a queue.
fn escalations_path(cfg: &Config, cwd: &Path) -> PathBuf {
    cfg.state_dir
        .join(project_key(&repo_root(cwd)))
        .join("escalations.json")
}

/// Fail-soft load: a missing or corrupt registry is treated as empty rather than
/// breaking the caller (mirrors `claim.rs`).
fn load(path: &Path) -> Registry {
    match std::fs::read_to_string(path) {
        Ok(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        Err(_) => Registry::default(),
    }
}

/// Atomic write (temp + rename), mirroring `RunState::save` / `claim.rs::save`.
fn save(path: &Path, reg: &Registry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(reg)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Derive the deterministic content-hash id for a new escalation. Uses the
/// prior record count so two otherwise-identical questions in the same run get
/// distinct ids, while a given (count, run, task, question) is stable across
/// runs of the process (no wall-clock / rand input).
fn derive_id(run: &str, task: &str, question: &str, prior_count: usize) -> String {
    let material = format!("{prior_count}\u{1f}{run}\u{1f}{task}\u{1f}{question}");
    format!("esc-{:08x}", fnv1a32(&material))
}

/// Enqueue a new escalation and persist it. Returns the stored record (with its
/// derived id). Atomic write; fail-soft load.
///
/// Idempotent for OPEN duplicates: if an existing record with the same
/// `(run, task, question)` is still unresolved, that record is returned
/// unchanged instead of appending a new one (content-dedup backpressure). A
/// RESOLVED match does not dedup — a re-ask after an answer still creates a
/// new open record.
#[allow(clippy::too_many_arguments)]
pub fn add_escalation(
    cfg: &Config,
    cwd: &Path,
    run: &str,
    task: &str,
    question: &str,
    options: &[String],
    recommended: usize,
    now: i64,
) -> Result<Escalation> {
    let path = escalations_path(cfg, cwd);
    let mut reg = load(&path);

    // Content-dedup backpressure: an identical (run, task, question) that is
    // still OPEN is returned as-is rather than appended again, so a flood of
    // repeated re-asks (e.g. under codegen retry storms) collapses onto a
    // single durable record instead of piling up duplicates. A RESOLVED match
    // does NOT dedup — a re-ask after an answer must create a fresh open
    // record.
    if let Some(existing) = reg
        .escalations
        .iter()
        .find(|e| !e.resolved && e.run == run && e.task == task && e.question == question)
    {
        return Ok(existing.clone());
    }

    let rec = Escalation {
        id: derive_id(run, task, question, reg.escalations.len()),
        run: run.to_string(),
        task: task.to_string(),
        question: question.to_string(),
        options: options.to_vec(),
        recommended,
        created_at: now,
        resolved: false,
        chosen: None,
    };
    reg.escalations.push(rec.clone());
    save(&path, &reg)?;
    Ok(rec)
}

/// List the still-OPEN (unresolved) escalations for `run`, in enqueue order.
pub fn list_escalations(cfg: &Config, cwd: &Path, run: &str) -> Result<Vec<Escalation>> {
    let path = escalations_path(cfg, cwd);
    let reg = load(&path);
    Ok(reg
        .escalations
        .into_iter()
        .filter(|e| e.run == run && !e.resolved)
        .collect())
}

/// Resolve the escalation with `id` by recording the chosen option. Marks the
/// record resolved (so a later `list` no longer shows it as open) but keeps it
/// in the store with its answer so the caller can resume the task. Returns the
/// updated record, or `None` if no record matched (fail-soft — no error).
pub fn resolve_escalation(
    cfg: &Config,
    cwd: &Path,
    id: &str,
    choice: &str,
) -> Result<Option<Escalation>> {
    let path = escalations_path(cfg, cwd);
    let mut reg = load(&path);
    let mut updated = None;
    for e in reg.escalations.iter_mut() {
        if e.id == id {
            e.resolved = true;
            e.chosen = Some(choice.to_string());
            updated = Some(e.clone());
            break;
        }
    }
    if updated.is_some() {
        save(&path, &reg)?;
    }
    Ok(updated)
}

/// Retrieve a single escalation by id regardless of resolved state (so a caller
/// can fetch a resolved record's answer to resume the task). Fail-soft.
// Library retrieval API for the resume path; consumed by tests today and the
// resume wiring in a follow-up task.
#[allow(dead_code)]
pub fn get_escalation(cfg: &Config, cwd: &Path, id: &str) -> Result<Option<Escalation>> {
    let path = escalations_path(cfg, cwd);
    let reg = load(&path);
    Ok(reg.escalations.into_iter().find(|e| e.id == id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn make_tmp_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p =
            std::env::temp_dir().join(format!("condukt-escalate-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_cfg(tmp: &Path) -> Config {
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
            adversarial_enabled: false,
            adversarial_size: crate::adversarial::DEFAULT_PANEL,
            adversarial_min_voters: crate::adversarial::DEFAULT_MIN_VOTERS,
            adversarial_block_ratio: crate::adversarial::DEFAULT_BLOCK_RATIO,
            single_worktree: false,
            worker_sandbox_enabled: false,
            worker_sandbox_image: None,
            worker_sandbox_memory: None,
            worker_sandbox_cpus: None,
            worker_sandbox_pids_limit: None,
        }
    }

    fn opts(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_persists_and_is_retrievable() {
        let tmp = make_tmp_dir("add");
        let cfg = make_cfg(&tmp);
        let rec = add_escalation(
            &cfg,
            &tmp,
            "runA",
            "t1",
            "Which approach?",
            &opts(&["a", "b"]),
            0,
            1234,
        )
        .unwrap();
        assert!(!rec.id.is_empty());
        assert_eq!(rec.run, "runA");
        assert_eq!(rec.task, "t1");
        assert_eq!(rec.question, "Which approach?");
        assert_eq!(rec.options, opts(&["a", "b"]));
        assert_eq!(rec.recommended, 0);
        assert_eq!(rec.created_at, 1234);
        assert!(!rec.resolved);
        assert!(rec.chosen.is_none());

        // Persisted to the durable store and retrievable by id.
        let got = get_escalation(&cfg, &tmp, &rec.id).unwrap().unwrap();
        assert_eq!(got, rec);
        // The file lives beside claims.json under the project key.
        assert!(escalations_path(&cfg, &tmp).exists());
    }

    #[test]
    fn list_shows_only_unresolved_for_the_run() {
        let tmp = make_tmp_dir("list");
        let cfg = make_cfg(&tmp);
        let a = add_escalation(&cfg, &tmp, "runA", "t1", "Q1", &opts(&["x", "y"]), 0, 1).unwrap();
        let _b = add_escalation(&cfg, &tmp, "runA", "t2", "Q2", &opts(&["x", "y"]), 1, 2).unwrap();
        // A different run's escalation must NOT appear.
        let _c = add_escalation(&cfg, &tmp, "runB", "t9", "Q9", &opts(&["x"]), 0, 3).unwrap();

        let open = list_escalations(&cfg, &tmp, "runA").unwrap();
        assert_eq!(open.len(), 2, "two open for runA");

        // Resolve one → it drops out of the open list, but the other stays.
        resolve_escalation(&cfg, &tmp, &a.id, "x").unwrap();
        let open = list_escalations(&cfg, &tmp, "runA").unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].task, "t2");
    }

    #[test]
    fn resolve_stores_choice_and_record_remains() {
        let tmp = make_tmp_dir("resolve");
        let cfg = make_cfg(&tmp);
        let rec = add_escalation(&cfg, &tmp, "runA", "t1", "Q", &opts(&["a", "b"]), 0, 10).unwrap();

        let updated = resolve_escalation(&cfg, &tmp, &rec.id, "b")
            .unwrap()
            .unwrap();
        assert!(updated.resolved);
        assert_eq!(updated.chosen.as_deref(), Some("b"));

        // No longer OPEN...
        assert!(list_escalations(&cfg, &tmp, "runA").unwrap().is_empty());
        // ...but still in the store, with its answer, for resume.
        let got = get_escalation(&cfg, &tmp, &rec.id).unwrap().unwrap();
        assert!(got.resolved);
        assert_eq!(got.chosen.as_deref(), Some("b"));
    }

    #[test]
    fn resolve_unknown_id_is_a_soft_noop() {
        let tmp = make_tmp_dir("resolve-missing");
        let cfg = make_cfg(&tmp);
        add_escalation(&cfg, &tmp, "runA", "t1", "Q", &opts(&["a"]), 0, 1).unwrap();
        let got = resolve_escalation(&cfg, &tmp, "esc-doesnotexist", "a").unwrap();
        assert!(got.is_none());
        // The real one is untouched / still open.
        assert_eq!(list_escalations(&cfg, &tmp, "runA").unwrap().len(), 1);
    }

    #[test]
    fn missing_store_lists_empty() {
        let tmp = make_tmp_dir("missing");
        let cfg = make_cfg(&tmp);
        // No add has happened → no file → fail-soft empty list, no error/panic.
        assert!(list_escalations(&cfg, &tmp, "runA").unwrap().is_empty());
        assert!(get_escalation(&cfg, &tmp, "whatever").unwrap().is_none());
    }

    #[test]
    fn corrupt_store_is_treated_as_empty() {
        let tmp = make_tmp_dir("corrupt");
        let cfg = make_cfg(&tmp);
        let path = escalations_path(&cfg, &tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json at all {{{").unwrap();
        // Fail-soft: add still succeeds (registry read as empty then written).
        let rec = add_escalation(&cfg, &tmp, "runA", "t1", "Q", &opts(&["a"]), 0, 1).unwrap();
        let got = get_escalation(&cfg, &tmp, &rec.id).unwrap();
        assert!(got.is_some());
    }

    #[test]
    fn id_is_deterministic_and_not_time_dependent() {
        // Same (count, run, task, question) → same id regardless of created_at.
        let id1 = derive_id("runA", "t1", "Q", 0);
        let id2 = derive_id("runA", "t1", "Q", 0);
        assert_eq!(id1, id2);
        // Different prior-count disambiguates identical questions.
        assert_ne!(
            derive_id("runA", "t1", "Q", 0),
            derive_id("runA", "t1", "Q", 1)
        );
    }

    #[test]
    fn add_dedups_identical_open_escalation() {
        let tmp = make_tmp_dir("dedup-open");
        let cfg = make_cfg(&tmp);
        let first = add_escalation(
            &cfg,
            &tmp,
            "runA",
            "t1",
            "Which approach?",
            &opts(&["a", "b"]),
            0,
            1,
        )
        .unwrap();
        // Same (run, task, question) re-enqueued while still OPEN must NOT
        // create a second record — it must return the same record/id.
        let second = add_escalation(
            &cfg,
            &tmp,
            "runA",
            "t1",
            "Which approach?",
            &opts(&["a", "b"]),
            0,
            2,
        )
        .unwrap();
        assert_eq!(first.id, second.id);

        let path = escalations_path(&cfg, &tmp);
        let reg = load(&path);
        assert_eq!(reg.escalations.len(), 1, "must not duplicate an open ask");
    }

    #[test]
    fn add_creates_new_open_record_after_resolve() {
        let tmp = make_tmp_dir("dedup-reask");
        let cfg = make_cfg(&tmp);
        let first =
            add_escalation(&cfg, &tmp, "runA", "t1", "Q", &opts(&["a", "b"]), 0, 1).unwrap();
        resolve_escalation(&cfg, &tmp, &first.id, "a").unwrap();

        // Re-asking the same (run, task, question) after resolution must
        // create a fresh OPEN record, not dedup against the resolved one.
        let second =
            add_escalation(&cfg, &tmp, "runA", "t1", "Q", &opts(&["a", "b"]), 0, 2).unwrap();
        assert_ne!(first.id, second.id);
        assert!(!second.resolved);

        let path = escalations_path(&cfg, &tmp);
        let reg = load(&path);
        assert_eq!(reg.escalations.len(), 2, "resolved + new open");

        let open = list_escalations(&cfg, &tmp, "runA").unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, second.id);
    }
}
