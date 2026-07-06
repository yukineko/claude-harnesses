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
//! Outcomes are appended to a small JSON array at `.compass/outcomes.json` (same
//! `.compass/` dir as the charter, resolved off the project root). Load → append
//! → atomic-write, mirroring the charter/hypothesis store conventions. Each
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
/// corrupt file is a hard error (unlike carve state, we don't want to silently
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
    // Write to a temp sibling then rename (mirror the hypothesis store).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
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

    let mut outcomes = load(root)?;
    let seq = outcomes.last().map(|o| o.seq + 1).unwrap_or(0);
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
}
