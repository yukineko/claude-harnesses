//! outcome (DESIGN §7) — record a completed move's judged outcome against the
//! charter's `measuring_stick`, the deterministic core of compass's "measurement
//! loop".
//!
//! # Build is not validation
//!
//! A move shipping does NOT mean the goal got closer. Recording an outcome
//! therefore REQUIRES measured evidence (mirrors the evidence-required guard in
//! `hypothesis::store`): `record` bails if every evidence string is
//! empty/whitespace, so a green build alone can't flip the verdict.
//!
//! # Persistence
//!
//! Outcomes are appended to a small JSON array at `<root>/.compass/outcomes.json`.
//! The CLI passes the project's MAIN worktree root as `<root>` (not the cwd the
//! charter is read from), because the file is gitignored and a per-checkout
//! store would split the history across linked worktrees. Load → append
//! → atomic-write, all under an exclusive lockfile (`outcomes.json.lock`) with
//! a unique temp name, so concurrent `compass` processes lose no records; if
//! the lock cannot be acquired the write is refused (non-zero exit). Mirrors
//! the charter/hypothesis store conventions otherwise. Each
//! record snapshots the charter's `north_star` and `current_gap` at record time
//! so it is self-describing, plus a monotonic `seq` and a `recorded_at`
//! timestamp (unix seconds).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::charter::Charter;

/// The verdict of a completed move judged against the charter `measuring_stick`:
/// 前進 / 不変 / 後退. A clap `ValueEnum` (CLI: `forward|unchanged|backward`) and
/// serde-serialized in the same snake_case form for the persisted record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum Verdict {
    /// 前進 — measured movement toward the goal.
    Forward,
    /// 不変 — no measurable change vs the measuring_stick.
    Unchanged,
    /// 後退 — measured movement away from the goal.
    Backward,
}

/// A single recorded outcome. Self-describing: it snapshots the charter goal /
/// gap it was judged against so a later reader needs no other context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// Monotonic sequence index (0-based), assigned at record time.
    pub seq: u64,
    /// Wall-clock record time, unix seconds (0 if the clock is pre-epoch).
    pub recorded_at: u64,
    /// 前進 / 不変 / 後退 vs the measuring_stick.
    pub verdict: Verdict,
    /// The measured evidence (non-empty, trimmed, empties filtered out).
    pub evidence: Vec<String>,
    /// Snapshot of the charter `north_star` at record time.
    pub north_star: String,
    /// Snapshot of the charter `current_gap` this outcome judged.
    pub current_gap: String,
}

/// On-disk shape: a JSON object with an `outcomes` array (forward-compatible).
#[derive(Debug, Default, Serialize, Deserialize)]
struct OutcomesFile {
    #[serde(default)]
    outcomes: Vec<Outcome>,
}

/// `.compass/outcomes.json` under the project root.
pub fn store_path(root: &Path) -> PathBuf {
    root.join(".compass").join("outcomes.json")
}

/// Load all recorded outcomes (oldest first). A missing file => empty Vec; a
/// corrupt file is a hard error (as for carve state: we don't want to silently
/// drop recorded measurements).
pub fn load(root: &Path) -> Result<Vec<Outcome>> {
    let path = store_path(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let file: OutcomesFile = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            Ok(file.outcomes)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Atomic-write the full outcomes array, creating `.compass/` if absent.
fn save(root: &Path, outcomes: &[Outcome]) -> Result<()> {
    let path = store_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = OutcomesFile {
        outcomes: outcomes.to_vec(),
    };
    let json = serde_json::to_string_pretty(&file).context("serializing outcomes")?;
    write_atomic(&path, &json)
}

/// Retry budget for acquiring a store lock, polled every `retry_delay_ms`
/// (create_new lockfile convention of `harness_core::lessons::acquire_lock`).
///
/// Two separate limits, because "the lock is busy" and "the lock is stuck" are
/// different facts:
/// - `per_holder_attempts`: polls spent while the SAME lockfile instance is in
///   place. Running out means one holder kept the lock the whole time (a crashed
///   process's stale lockfile, or a hung holder), so we refuse. Each time the
///   lockfile is released or replaced by a new instance, the queue made
///   progress, and this count restarts.
/// - `total_attempts`: hard cap across all holders. Running out means the lock
///   kept changing hands but this process never won it, so we refuse.
///
/// Before this split a single 1000-poll budget counted the whole queue. With
/// many concurrent `compass apply`/`evaluate` runs (each holding the carve lock
/// for ~100-240 ms under load, mostly the C2 git subprocesses), a waiter that
/// kept losing the race gave up after ~5 s even though no holder was stuck.
/// The refused `apply` exited non-zero and its answer was never saved.
#[derive(Debug, Clone, Copy)]
struct LockBudget {
    per_holder_attempts: u32,
    total_attempts: u32,
    retry_delay_ms: u64,
}

/// ~5 s with one holder in place (stale lock), ~60 s overall.
const LOCK_BUDGET: LockBudget = LockBudget {
    per_holder_attempts: 1000,
    total_attempts: 12_000,
    retry_delay_ms: 5,
};

/// An exclusive advisory lock on a store file, held as long as this guard
/// lives. The lock IS the lockfile `<store file>.lock` (held iff it exists);
/// dropping the guard removes it on every exit path, including errors.
#[derive(Debug)]
pub(crate) struct StoreLock {
    path: PathBuf,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            // Not silent: a lockfile we failed to remove refuses every later
            // write, so tell the human which file it is.
            eprintln!(
                "compass: failed to release store lock {}: {e}; remove it by hand \
                 once no compass process is running",
                self.path.display()
            );
        }
    }
}

/// `<store file>.lock` for a store path (e.g. `outcomes.json.lock`).
fn lock_path_for(store: &Path) -> PathBuf {
    let mut p = store.as_os_str().to_owned();
    p.push(".lock");
    PathBuf::from(p)
}

/// Identity of one lockfile instance: inode (unix) plus mtime. A new holder
/// creates a new file, so the identity changes when the lock changes hands.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LockInstance {
    ino: u64,
    modified: Option<std::time::SystemTime>,
}

/// What a poll of the lockfile saw while it was held by someone else.
enum Holder {
    /// The lockfile was gone by the time we looked (released).
    Released,
    /// The lockfile instance currently in place.
    Instance(LockInstance),
    /// Could not stat it. Counted as "same holder" (no progress observed), so
    /// an unreadable lockfile still runs out the per-holder budget.
    Unknown,
}

fn current_holder(lock_path: &Path) -> Holder {
    match std::fs::symlink_metadata(lock_path) {
        Ok(m) => {
            #[cfg(unix)]
            let ino = std::os::unix::fs::MetadataExt::ino(&m);
            #[cfg(not(unix))]
            let ino = 0;
            Holder::Instance(LockInstance {
                ino,
                modified: m.modified().ok(),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Holder::Released,
        Err(_) => Holder::Unknown,
    }
}

/// Acquire the exclusive lock for `store` (creating its parent dir), retrying
/// while another process holds it. Fail-closed: if the lock cannot be acquired
/// within the [`LockBudget`] (one holder in place for ~5 s, or ~60 s overall),
/// or the lockfile cannot be created for any other reason, this returns an
/// error naming the lock path and the caller must refuse the write. There is
/// NO fallback to an unlocked write and NO automatic reaping of a stale
/// lockfile: a lock left by a crashed process keeps refusing writes until a
/// human removes the named file.
pub(crate) fn lock_store(store: &Path) -> Result<StoreLock> {
    lock_store_with(store, LOCK_BUDGET)
}

fn lock_store_with(store: &Path, budget: LockBudget) -> Result<StoreLock> {
    let lock_path = lock_path_for(store);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut seen: Option<LockInstance> = None;
    let mut same_holder_polls: u32 = 0;
    for _ in 0..budget.total_attempts {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut f) => {
                use std::io::Write;
                let guard = StoreLock {
                    path: lock_path.clone(),
                };
                // The owner pid is diagnostic only (the lock is the file's
                // existence), so a failed pid write does not void the lock.
                let _ = writeln!(f, "{}", std::process::id());
                return Ok(guard);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match current_holder(&lock_path) {
                    // Released between our create and our stat: retry now.
                    Holder::Released => {
                        seen = None;
                        same_holder_polls = 0;
                        continue;
                    }
                    Holder::Instance(inst) if seen.as_ref() != Some(&inst) => {
                        // A different holder than last poll: the queue moved.
                        seen = Some(inst);
                        same_holder_polls = 0;
                    }
                    Holder::Instance(_) | Holder::Unknown => {}
                }
                same_holder_polls += 1;
                if same_holder_polls >= budget.per_holder_attempts {
                    anyhow::bail!(
                        "refusing to write {}: store lock {} is held by another process \
                         (one holder kept it for ~{} ms). If no compass process is running, \
                         the lock is stale: remove {} and retry.",
                        store.display(),
                        lock_path.display(),
                        u64::from(budget.per_holder_attempts) * budget.retry_delay_ms,
                        lock_path.display()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(budget.retry_delay_ms));
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "refusing to write {}: cannot create store lock {}",
                        store.display(),
                        lock_path.display()
                    )
                });
            }
        }
    }
    anyhow::bail!(
        "refusing to write {}: store lock {} kept changing hands but this process \
         never acquired it (gave up after {} polls, ~{} ms). Retry when fewer \
         compass processes are running.",
        store.display(),
        lock_path.display(),
        budget.total_attempts,
        u64::from(budget.total_attempts) * budget.retry_delay_ms,
    )
}

/// Atomically replace `path` with `contents`: write a uniquely-named temp
/// sibling (`<file>.tmp.<pid>.<nonce>`) then rename it into place. The temp
/// file is removed if the write or rename fails. Callers hold the store lock.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}.{nanos:x}{seq:x}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let res = std::fs::write(&tmp, contents)
        .with_context(|| format!("writing {}", tmp.display()))
        .and_then(|()| {
            std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))
        });
    if res.is_err() {
        // Best-effort cleanup; the original error is what gets reported.
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Current wall-clock as unix seconds (0 if the system clock is before epoch).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Record one completed move's outcome against the charter `measuring_stick` and
/// append it to the store. REQUIRES measured evidence: trims each entry, drops
/// the empties, and bails if nothing non-empty remains (build is not
/// validation). Returns the persisted [`Outcome`].
pub fn record(
    root: &Path,
    charter: &Charter,
    verdict: Verdict,
    evidence: Vec<String>,
) -> Result<Outcome> {
    let evidence: Vec<String> = evidence
        .into_iter()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect();
    if evidence.is_empty() {
        anyhow::bail!(
            "outcome requires measured evidence: pass --evidence \"<observed result>\" \
             (build is not validation)"
        );
    }

    // The whole load/push/save runs under the store's exclusive lock so that
    // concurrent recorders cannot lose each other's entries.
    let _lock = lock_store(&store_path(root))?;
    let mut outcomes = load(root)?;
    // max+1, not last+1: the store may be non-monotonic (hand edit, merge),
    // and last+1 would then duplicate an existing seq. Overflow fails closed.
    let seq = match outcomes.iter().map(|o| o.seq).max() {
        None => 0,
        Some(m) => m.checked_add(1).ok_or_else(|| {
            anyhow::anyhow!("outcome seq overflow: store already holds seq {m} (u64::MAX)")
        })?,
    };
    let outcome = Outcome {
        seq,
        recorded_at: now_unix(),
        verdict,
        evidence,
        north_star: charter.north_star.clone(),
        current_gap: charter.current_gap.clone(),
    };
    outcomes.push(outcome.clone());
    save(root, &outcomes)?;
    Ok(outcome)
}

/// The most recently recorded outcome, or `None` if none exist.
pub fn latest(root: &Path) -> Result<Option<Outcome>> {
    Ok(load(root)?.into_iter().next_back())
}

// `#[allow(dead_code)]` on the pivot-signal API mirrors the `hypothesis::store`
// convention: the deterministic core + its tests ship first; the `pivot-check`
// CLI subcommand that calls it (and flow's loop-end consume) is the parked
// follow-up slice. Until that lands these are the tested entry points.

/// Default pivot threshold: this many consecutive non-forward (unchanged or
/// backward) outcomes at the tail of the history recommends a pivot.
#[allow(dead_code)]
pub const DEFAULT_PIVOT_THRESHOLD: usize = 3;

/// Whether the accumulated outcome trend says to stay the course or to change
/// direction at the north_star level (DESIGN: pivot-or-persevere gate).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    /// Keep the current north_star — the trend is not a sustained stall.
    Persevere,
    /// The tail shows a sustained stall/regression — re-orient the north_star.
    Pivot,
}

/// A pivot-or-persevere recommendation derived from the trailing outcome streak.
/// Self-describing: carries the measured streak, the threshold it was judged
/// against, and a human-readable aggregation reason.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PivotSignal {
    /// persevere | pivot.
    pub recommendation: Recommendation,
    /// Length of the trailing run of consecutive unchanged/backward verdicts.
    pub streak: usize,
    /// The threshold `streak` was compared against (>= => pivot).
    pub threshold: usize,
    /// Aggregation reason: streak length, the verdict run, and the last forward.
    pub reason: String,
}

/// Aggregate the trailing streak of consecutive non-forward (unchanged or
/// backward) outcomes and recommend pivot vs persevere. Deterministic, pure over
/// the supplied history. Empty history or a forward tail => persevere (streak 0);
/// `threshold > 0 && streak >= threshold` => pivot. A forward outcome anywhere in
/// the tail resets the streak (only the run *after* the last forward counts).
#[allow(dead_code)]
pub fn pivot_signal(outcomes: &[Outcome], threshold: usize) -> PivotSignal {
    let mut streak = 0usize;
    let mut last_forward_seq: Option<u64> = None;
    for o in outcomes.iter().rev() {
        if o.verdict == Verdict::Forward {
            last_forward_seq = Some(o.seq);
            break;
        }
        streak += 1;
    }

    let recommendation = if threshold > 0 && streak >= threshold {
        Recommendation::Pivot
    } else {
        Recommendation::Persevere
    };

    let verdict_run: Vec<&str> = outcomes
        .iter()
        .rev()
        .take(streak)
        .map(|o| match o.verdict {
            Verdict::Unchanged => "unchanged",
            Verdict::Backward => "backward",
            Verdict::Forward => "forward", // unreachable within the streak
        })
        .collect();

    let reason = if streak == 0 {
        if outcomes.is_empty() {
            "no outcomes recorded yet; persevere".to_string()
        } else {
            "latest outcome is forward; persevere".to_string()
        }
    } else {
        let cmp = if recommendation == Recommendation::Pivot {
            ">="
        } else {
            "<"
        };
        let tail = match last_forward_seq {
            Some(s) => format!("; last forward at seq {s}"),
            None => "; no forward outcome on record".to_string(),
        };
        format!(
            "{} consecutive non-forward outcome(s) ({} threshold {}): [{}]{}",
            streak,
            cmp,
            threshold,
            verdict_run.join(", "),
            tail
        )
    };

    PivotSignal {
        recommendation,
        streak,
        threshold,
        reason,
    }
}

/// Observed, deterministic facts about a completed move, from which a DEFAULT
/// verdict is derived. This is an advisory default the LLM (skill) can still
/// override via `compass outcome --verdict`; it never records anything itself.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeFacts {
    /// Net change in passing tests vs the baseline (may be negative).
    pub tests_delta: i64,
    /// Count of observed regressions (newly failing tests / broken invariants).
    pub regressions: u64,
    /// Whether this move closed the charter `current_gap` it was judged against.
    pub gap_closed: bool,
}

/// Derive a deterministic DEFAULT [`Verdict`] from observed facts, mirroring the
/// purity of [`pivot_signal`]. Documented rules (regressions dominate):
/// `regressions > 0` => [`Verdict::Backward`]; else a positive test-delta or a
/// closed gap (`tests_delta > 0 || gap_closed`) => [`Verdict::Forward`]; else
/// [`Verdict::Unchanged`]. Advisory only — the skill can override via `--verdict`.
#[allow(dead_code)]
pub fn suggest_verdict(facts: &OutcomeFacts) -> Verdict {
    if facts.regressions > 0 {
        Verdict::Backward
    } else if facts.tests_delta > 0 || facts.gap_closed {
        Verdict::Forward
    } else {
        Verdict::Unchanged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn charter() -> Charter {
        Charter {
            north_star: "ship the measurement loop".to_string(),
            current_gap: "outcomes are never judged".to_string(),
            measuring_stick: "closeness-to-goal / cost".to_string(),
            ..Charter::default()
        }
    }

    #[test]
    fn record_with_evidence_persists_and_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        let rec = record(
            root,
            &charter(),
            Verdict::Forward,
            vec!["latency dropped 30%".to_string(), "  ".to_string()],
        )
        .expect("record");

        // empties filtered, snapshots captured.
        assert_eq!(rec.seq, 0);
        assert_eq!(rec.verdict, Verdict::Forward);
        assert_eq!(rec.evidence, vec!["latency dropped 30%".to_string()]);
        assert_eq!(rec.north_star, "ship the measurement loop");
        assert_eq!(rec.current_gap, "outcomes are never judged");

        // reload from disk: the record round-trips.
        let loaded = load(root).expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], rec);

        // a second record gets a monotonic seq and becomes the latest.
        let rec2 = record(
            root,
            &charter(),
            Verdict::Backward,
            vec!["errors up".to_string()],
        )
        .expect("record 2");
        assert_eq!(rec2.seq, 1);
        assert_eq!(latest(root).expect("latest"), Some(rec2));
    }

    #[test]
    fn empty_evidence_is_rejected_and_persists_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        // all-empty / whitespace-only evidence is refused.
        let err = record(root, &charter(), Verdict::Forward, vec!["   ".to_string()]).unwrap_err();
        assert!(err.to_string().contains("requires measured evidence"));

        let err2 = record(root, &charter(), Verdict::Unchanged, vec![]).unwrap_err();
        assert!(err2.to_string().contains("requires measured evidence"));

        // nothing was written.
        assert!(!store_path(root).exists());
        assert_eq!(load(root).expect("load").len(), 0);
        assert_eq!(latest(root).expect("latest"), None);
    }

    #[test]
    fn latest_is_none_when_no_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(latest(dir.path()).expect("latest"), None);
    }

    /// Build a bare Outcome fixture with just the fields the streak logic reads.
    fn out(seq: u64, verdict: Verdict) -> Outcome {
        Outcome {
            seq,
            recorded_at: 0,
            verdict,
            evidence: vec!["e".to_string()],
            north_star: String::new(),
            current_gap: String::new(),
        }
    }

    #[test]
    fn pivot_signal_empty_history_perseveres() {
        let sig = pivot_signal(&[], DEFAULT_PIVOT_THRESHOLD);
        assert_eq!(sig.recommendation, Recommendation::Persevere);
        assert_eq!(sig.streak, 0);
        assert!(sig.reason.contains("no outcomes recorded"));
    }

    #[test]
    fn pivot_signal_forward_tail_perseveres() {
        // a trailing forward => streak 0 => persevere, regardless of earlier stalls.
        let hist = [
            out(0, Verdict::Backward),
            out(1, Verdict::Unchanged),
            out(2, Verdict::Forward),
        ];
        let sig = pivot_signal(&hist, DEFAULT_PIVOT_THRESHOLD);
        assert_eq!(sig.recommendation, Recommendation::Persevere);
        assert_eq!(sig.streak, 0);
        assert!(sig.reason.contains("latest outcome is forward"));
    }

    #[test]
    fn pivot_signal_below_threshold_perseveres() {
        // threshold-1 (=2) consecutive unchanged at the tail => persevere.
        let hist = [
            out(0, Verdict::Forward),
            out(1, Verdict::Unchanged),
            out(2, Verdict::Unchanged),
        ];
        let sig = pivot_signal(&hist, 3);
        assert_eq!(sig.recommendation, Recommendation::Persevere);
        assert_eq!(sig.streak, 2);
        assert!(sig.reason.contains("< threshold 3"));
        assert!(sig.reason.contains("last forward at seq 0"));
    }

    #[test]
    fn pivot_signal_at_threshold_pivots() {
        // threshold (=3) consecutive backward at the tail => pivot.
        let hist = [
            out(0, Verdict::Backward),
            out(1, Verdict::Backward),
            out(2, Verdict::Backward),
        ];
        let sig = pivot_signal(&hist, 3);
        assert_eq!(sig.recommendation, Recommendation::Pivot);
        assert_eq!(sig.streak, 3);
        assert!(sig.reason.contains(">= threshold 3"));
        assert!(sig.reason.contains("no forward outcome on record"));
    }

    #[test]
    fn pivot_signal_forward_resets_streak() {
        // an intervening forward resets: only the run AFTER it counts.
        let hist = [
            out(0, Verdict::Backward),
            out(1, Verdict::Backward),
            out(2, Verdict::Forward),
            out(3, Verdict::Backward),
        ];
        let sig = pivot_signal(&hist, 3);
        assert_eq!(sig.recommendation, Recommendation::Persevere);
        assert_eq!(sig.streak, 1);
        assert!(sig.reason.contains("last forward at seq 2"));
    }

    #[test]
    fn suggest_verdict_regressions_dominate() {
        // regressions > 0 => backward, even with a positive test-delta.
        let facts = OutcomeFacts {
            tests_delta: 5,
            regressions: 1,
            gap_closed: true,
        };
        assert_eq!(suggest_verdict(&facts), Verdict::Backward);
    }

    #[test]
    fn suggest_verdict_positive_tests_delta_is_forward() {
        let facts = OutcomeFacts {
            tests_delta: 3,
            regressions: 0,
            gap_closed: false,
        };
        assert_eq!(suggest_verdict(&facts), Verdict::Forward);
    }

    #[test]
    fn suggest_verdict_gap_closed_is_forward() {
        // gap closed with a zero test-delta still reads as forward.
        let facts = OutcomeFacts {
            tests_delta: 0,
            regressions: 0,
            gap_closed: true,
        };
        assert_eq!(suggest_verdict(&facts), Verdict::Forward);
    }

    #[test]
    fn suggest_verdict_all_zero_is_unchanged() {
        let facts = OutcomeFacts {
            tests_delta: 0,
            regressions: 0,
            gap_closed: false,
        };
        assert_eq!(suggest_verdict(&facts), Verdict::Unchanged);
    }

    #[test]
    fn verdict_serde_round_trips_all_variants() {
        for (v, snake) in [
            (Verdict::Forward, "\"forward\""),
            (Verdict::Unchanged, "\"unchanged\""),
            (Verdict::Backward, "\"backward\""),
        ] {
            let json = serde_json::to_string(&v).expect("serialize");
            assert_eq!(json, snake);
            let back: Verdict = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, v);
        }
    }

    /// Replace the lockfile with a fresh instance (new inode) every few ms,
    /// never leaving the path free, until `stop` is set; then release it.
    fn spawn_rotator(
        lock: PathBuf,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> std::thread::JoinHandle<()> {
        std::fs::write(&lock, b"holder").expect("seed lock");
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                let mut tmp = lock.as_os_str().to_owned();
                tmp.push(format!(".rot{n}"));
                let tmp = PathBuf::from(tmp);
                std::fs::write(&tmp, n.to_string()).expect("write rot");
                std::fs::rename(&tmp, &lock).expect("rotate lock");
                n += 1;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            std::fs::remove_file(&lock).expect("release lock");
        })
    }

    #[test]
    fn lock_waits_past_per_holder_budget_while_holders_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dir.path().join("s.json");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rot = spawn_rotator(lock_path_for(&store), stop.clone());
        let budget = LockBudget {
            per_holder_attempts: 100,
            total_attempts: 100_000,
            retry_delay_ms: 5,
        };
        let stopper = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                // Several per-holder budgets' worth (>= 100 polls * 5 ms each).
                std::thread::sleep(std::time::Duration::from_millis(1500));
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };
        let t = std::time::Instant::now();
        let got = lock_store_with(&store, budget);
        stopper.join().unwrap();
        rot.join().unwrap();
        assert!(
            got.is_ok(),
            "a busy lock that keeps changing hands must not be refused as stale: {:?}",
            got.err()
        );
        assert!(t.elapsed() >= std::time::Duration::from_millis(1500));
    }

    #[test]
    fn lock_refuses_when_one_holder_keeps_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dir.path().join("s.json");
        std::fs::write(lock_path_for(&store), b"stale").unwrap();
        let budget = LockBudget {
            per_holder_attempts: 20,
            total_attempts: 100_000,
            retry_delay_ms: 5,
        };
        let err = lock_store_with(&store, budget).expect_err("stale lock must refuse");
        assert!(err.to_string().contains("stale"), "{err}");
        assert!(
            lock_path_for(&store).exists(),
            "a stale lock is never reaped"
        );
    }

    #[test]
    fn lock_refuses_at_total_cap_even_while_holders_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dir.path().join("s.json");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rot = spawn_rotator(lock_path_for(&store), stop.clone());
        let budget = LockBudget {
            per_holder_attempts: 1_000_000,
            total_attempts: 50,
            retry_delay_ms: 5,
        };
        let got = lock_store_with(&store, budget);
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        rot.join().unwrap();
        let err = got.expect_err("total cap must refuse");
        assert!(err.to_string().contains("never acquired"), "{err}");
    }
}
