//! Durable checkpoint + append-only journal for condukt runs (charter #7).
//!
//! Auto-proceeding is only safe if it is reversible. This module is the safety
//! net: [`write_checkpoint`] snapshots a whole [`RunState`] plus each task's
//! worktree branch SHA into a durable, atomically-written store, and every
//! checkpoint/rollback is recorded in an append-only journal. Restoring a
//! snapshot lets a failed autonomous step roll the run back to a known-good
//! phase.
//!
//! Deterministic + fail-soft, mirroring the atomic-write discipline already in
//! `state.rs` / `store.rs`: writes go through a temp file + rename so a crash or
//! concurrent writer never observes a half-written store, and every read
//! degrades to an empty result on a missing/corrupt file rather than panicking.
//! No `unwrap`/`expect` on any IO or serde path.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::{now_secs, RunState};

/// A durable snapshot of a run at a point in time: the full run-state plus the
/// git SHA each task's branch pointed at, so a rollback can restore both the
/// tracking state and (best-effort) the worktrees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Monotonic 1-based sequence within the run (max existing + 1).
    pub seq: u64,
    /// Human label (e.g. the phase name); empty string if unlabelled.
    pub label: String,
    /// Unix seconds when the checkpoint was taken.
    pub created_at: i64,
    /// The snapshotted run-state.
    pub run: RunState,
    /// task id → branch tip SHA at snapshot time (only tasks with a known SHA).
    #[serde(default)]
    pub branch_shas: BTreeMap<String, String>,
}

/// What a journal line records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalKind {
    Checkpoint,
    Rollback,
    AutoRollback,
}

/// One append-only journal event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    /// The checkpoint seq this event refers to.
    pub seq: u64,
    pub kind: JournalKind,
    pub label: String,
    pub created_at: i64,
    #[serde(default)]
    pub note: Option<String>,
}

/// Durable path of the checkpoints store for a run (a JSON array of
/// [`Checkpoint`]). `dir` is the run's project state dir; `run_id` is sanitised
/// so a crafted id can't escape it.
pub fn checkpoint_path(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!(
        "{}.checkpoints.json",
        harness_core::store::safe_session(run_id)
    ))
}

/// Durable path of the append-only journal (JSONL) for a run.
pub fn journal_path(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!(
        "{}.journal.jsonl",
        harness_core::store::safe_session(run_id)
    ))
}

/// Load all checkpoints for a run. A missing or corrupt store yields an empty
/// Vec — never panics.
pub fn load_checkpoints(dir: &Path, run_id: &str) -> Vec<Checkpoint> {
    let path = checkpoint_path(dir, run_id);
    match std::fs::read_to_string(&path) {
        Ok(txt) => serde_json::from_str(&txt).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// The highest-seq checkpoint, if any.
pub fn latest_checkpoint(dir: &Path, run_id: &str) -> Option<Checkpoint> {
    load_checkpoints(dir, run_id)
        .into_iter()
        .max_by_key(|c| c.seq)
}

/// The checkpoint with exactly this seq, if present.
pub fn checkpoint_at(dir: &Path, run_id: &str, seq: u64) -> Option<Checkpoint> {
    load_checkpoints(dir, run_id)
        .into_iter()
        .find(|c| c.seq == seq)
}

/// Load the journal (JSONL) in file order. Missing file → empty; corrupt lines
/// are skipped rather than aborting the read.
pub fn load_journal(dir: &Path, run_id: &str) -> Vec<JournalEntry> {
    let path = journal_path(dir, run_id);
    let Ok(txt) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    txt.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<JournalEntry>(l).ok())
        .collect()
}

/// Append one journal entry (fail-soft: an IO/serialize error is swallowed so a
/// journaling failure never breaks a turn). A single-line append is atomic
/// enough for a log; we open in append mode and write one line.
pub fn append_journal(dir: &Path, run_id: &str, entry: &JournalEntry) {
    let Ok(mut line) = serde_json::to_string(entry) else {
        return;
    };
    line.push('\n');
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path(dir, run_id))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Snapshot `run` (with `branch_shas`) as the next checkpoint, and journal the
/// event. Returns the assigned seq. The checkpoints store is rewritten
/// atomically (temp + rename) so it is never observed half-written.
pub fn write_checkpoint(
    dir: &Path,
    run_id: &str,
    run: &RunState,
    label: &str,
    branch_shas: BTreeMap<String, String>,
) -> Result<u64> {
    let mut all = load_checkpoints(dir, run_id);
    let seq = all.iter().map(|c| c.seq).max().unwrap_or(0) + 1;
    let created_at = now_secs();
    all.push(Checkpoint {
        seq,
        label: label.to_string(),
        created_at,
        run: run.clone(),
        branch_shas,
    });
    atomic_write_json(&checkpoint_path(dir, run_id), &all)?;
    append_journal(
        dir,
        run_id,
        &JournalEntry {
            seq,
            kind: JournalKind::Checkpoint,
            label: label.to_string(),
            created_at,
            note: None,
        },
    );
    Ok(seq)
}

/// Serialize `val` to `path` atomically: write to a sibling temp file (unique
/// per process + call so parallel writers never share one), then rename over
/// the target. Same discipline as `store::save_json` / `RunState::save`.
fn atomic_write_json<T: Serialize>(path: &Path, val: &T) -> Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(val).context("serializing checkpoints")?;
    let pid = std::process::id();
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{pid}.{n}"));
    std::fs::write(&tmp, json.as_bytes()).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Status, TaskState};

    fn sample_run(run_id: &str, status: Status) -> RunState {
        RunState {
            run_id: run_id.to_string(),
            goal: "g".into(),
            tasks: vec![TaskState {
                id: "t1".into(),
                status,
                ..Default::default()
            }],
            paused: false,
            terminal_label: None,
            recorded_at: None,
            recorded_episodes: Vec::new(),
        }
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        let pid = std::process::id();
        d.push(format!("condukt-ckpt-test-{pid}-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mk tmp dir");
        d
    }

    #[test]
    fn write_then_load_roundtrips_snapshot() {
        let dir = tmp_dir("roundtrip");
        let run = sample_run("r1", Status::Running);
        let seq = write_checkpoint(&dir, "r1", &run, "phase-a", BTreeMap::new()).unwrap();
        assert_eq!(seq, 1);
        let loaded = load_checkpoints(&dir, "r1");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].run.run_id, "r1");
        assert_eq!(loaded[0].label, "phase-a");
        assert!(matches!(loaded[0].run.tasks[0].status, Status::Running));
    }

    #[test]
    fn seq_is_monotonic_across_writes() {
        let dir = tmp_dir("monotonic");
        let run = sample_run("r1", Status::Pending);
        assert_eq!(
            write_checkpoint(&dir, "r1", &run, "a", BTreeMap::new()).unwrap(),
            1
        );
        assert_eq!(
            write_checkpoint(&dir, "r1", &run, "b", BTreeMap::new()).unwrap(),
            2
        );
        assert_eq!(
            write_checkpoint(&dir, "r1", &run, "c", BTreeMap::new()).unwrap(),
            3
        );
        assert_eq!(load_checkpoints(&dir, "r1").len(), 3);
    }

    #[test]
    fn latest_and_at_select_correctly() {
        let dir = tmp_dir("select");
        write_checkpoint(
            &dir,
            "r1",
            &sample_run("r1", Status::Pending),
            "a",
            BTreeMap::new(),
        )
        .unwrap();
        write_checkpoint(
            &dir,
            "r1",
            &sample_run("r1", Status::Verified),
            "b",
            BTreeMap::new(),
        )
        .unwrap();
        let latest = latest_checkpoint(&dir, "r1").expect("latest");
        assert_eq!(latest.seq, 2);
        assert!(matches!(latest.run.tasks[0].status, Status::Verified));
        assert_eq!(checkpoint_at(&dir, "r1", 1).expect("at 1").seq, 1);
        assert!(checkpoint_at(&dir, "r1", 99).is_none());
    }

    #[test]
    fn journal_accumulates_in_order() {
        let dir = tmp_dir("journal");
        write_checkpoint(
            &dir,
            "r1",
            &sample_run("r1", Status::Pending),
            "cp",
            BTreeMap::new(),
        )
        .unwrap();
        append_journal(
            &dir,
            "r1",
            &JournalEntry {
                seq: 1,
                kind: JournalKind::Rollback,
                label: "cp".into(),
                created_at: 0,
                note: Some("manual".into()),
            },
        );
        let j = load_journal(&dir, "r1");
        assert_eq!(j.len(), 2);
        assert_eq!(j[0].kind, JournalKind::Checkpoint);
        assert_eq!(j[1].kind, JournalKind::Rollback);
        assert_eq!(j[1].note.as_deref(), Some("manual"));
    }

    #[test]
    fn missing_files_yield_empty_without_panic() {
        let dir = tmp_dir("missing");
        assert!(load_checkpoints(&dir, "nope").is_empty());
        assert!(load_journal(&dir, "nope").is_empty());
        assert!(latest_checkpoint(&dir, "nope").is_none());
    }

    #[test]
    fn corrupt_files_yield_empty_without_panic() {
        let dir = tmp_dir("corrupt");
        std::fs::write(checkpoint_path(&dir, "r1"), b"{not json").unwrap();
        std::fs::write(journal_path(&dir, "r1"), b"garbage\n{also bad\n").unwrap();
        assert!(load_checkpoints(&dir, "r1").is_empty());
        assert!(load_journal(&dir, "r1").is_empty());
    }

    #[test]
    fn branch_shas_survive_roundtrip() {
        let dir = tmp_dir("shas");
        let mut shas = BTreeMap::new();
        shas.insert("t1".to_string(), "abc123".to_string());
        write_checkpoint(&dir, "r1", &sample_run("r1", Status::Running), "x", shas).unwrap();
        let c = latest_checkpoint(&dir, "r1").expect("cp");
        assert_eq!(c.branch_shas.get("t1").map(String::as_str), Some("abc123"));
    }
}
